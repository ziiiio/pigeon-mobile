//! HTTP-level tests for the api/session layer against a mock homeserver.
//!
//! These assert the *client* half of the contract — the request shape we send
//! and how we map the server's responses — without a real server or Docker.
//! They complement the pure unit tests in `api.rs` and drive the actual FFI
//! surface (`session::register`/`login` → `Session`/`CoreError`) end to end.
//!
//! The full oneshot-homeserver e2e (real Postgres via testcontainers) is a
//! heavier, Docker-gated lane tracked separately in ROADMAP M1; it proves
//! protocol-compatibility against the real server rather than a canned mock.

use pigeon_mobile_core::api::{Api, ErrorCode};
use pigeon_mobile_core::session;
use pigeon_mobile_core::CoreError;
use serde_json::json;
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// The `AuthResponse` body both register and login return on success.
fn auth_body() -> serde_json::Value {
    json!({
        "user_id": "@alice:test.example",
        "device_id": "DEVICE1",
        "access_token": "secret-token"
    })
}

#[tokio::test]
async fn register_sends_expected_request_and_parses_auth() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/_pigeon/client/v1/register"))
        .and(body_json(
            json!({ "username": "alice", "password": "hunter2" }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(auth_body()))
        .expect(1)
        .mount(&server)
        .await;

    let api = Api::new(server.uri(), None).unwrap();
    let auth = api.register("alice", "hunter2").await.expect("register ok");

    assert_eq!(auth.user_id, "@alice:test.example");
    assert_eq!(auth.device_id, "DEVICE1");
    assert_eq!(auth.access_token, "secret-token");
}

#[tokio::test]
async fn login_uses_password_flow_shape() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/_pigeon/client/v1/login"))
        // The password flow is a tagged enum on the server: `type` selects it.
        .and(body_json(
            json!({ "type": "p.login.password", "user": "alice", "password": "hunter2" }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(auth_body()))
        .expect(1)
        .mount(&server)
        .await;

    let api = Api::new(server.uri(), None).unwrap();
    let auth = api.login("alice", "hunter2").await.expect("login ok");
    assert_eq!(auth.device_id, "DEVICE1");
}

#[tokio::test]
async fn bearer_token_is_attached_when_set() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/_pigeon/client/v1/account/whoami"))
        .and(header("authorization", "Bearer secret-token"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "user_id": "@alice:test.example", "device_id": "DEVICE1" })),
        )
        .expect(1)
        .mount(&server)
        .await;

    let api = Api::new(server.uri(), Some("secret-token".to_owned())).unwrap();
    api.whoami().await.expect("whoami ok"); // 200 only if the bearer header matched
}

#[tokio::test]
async fn server_p_error_maps_to_typed_code() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/_pigeon/client/v1/register"))
        .respond_with(
            ResponseTemplate::new(403).set_body_json(
                json!({ "errcode": "P_USER_IN_USE", "error": "user already exists" }),
            ),
        )
        .mount(&server)
        .await;

    let api = Api::new(server.uri(), None).unwrap();
    let err = api.register("alice", "hunter2").await.unwrap_err();
    match err {
        pigeon_mobile_core::api::ApiError::Server { status, code, .. } => {
            assert_eq!(status, 403);
            assert_eq!(code, ErrorCode::UserInUse);
        }
        other => panic!("expected a Server error, got {other:?}"),
    }
}

// --- The FFI surface end to end -------------------------------------------
// Drives the exact functions the native UI calls: `session::login`/`register`
// return a `PigeonClient` exposing only the non-secret `Session`, and server
// errors arrive as a typed `CoreError` the UI can branch on.

#[tokio::test]
async fn ffi_login_returns_session_without_token() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/_pigeon/client/v1/login"))
        .respond_with(ResponseTemplate::new(200).set_body_json(auth_body()))
        .mount(&server)
        .await;

    let client = session::login(server.uri(), "alice".into(), "hunter2".into())
        .await
        .expect("login ok");
    let s = client.session();
    assert_eq!(s.user_id, "@alice:test.example");
    assert_eq!(s.device_id, "DEVICE1");
    assert_eq!(s.server, server.uri());
    // The `Session` record has no token field at all — the access token stays
    // inside the core (Gotcha #1). This is a compile-time guarantee; asserted
    // here for the record.
}

#[tokio::test]
async fn ffi_register_maps_server_error_to_core_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/_pigeon/client/v1/register"))
        .respond_with(
            ResponseTemplate::new(403).set_body_json(
                json!({ "errcode": "P_USER_IN_USE", "error": "user already exists" }),
            ),
        )
        .mount(&server)
        .await;

    // Match without unwrap_err: `PigeonClient` intentionally isn't `Debug` (its
    // `Api` holds the token — keeping it out of debug output, Gotcha #2).
    match session::register(server.uri(), "alice".into(), "hunter2".into()).await {
        Ok(_) => panic!("expected an error, got a client"),
        Err(CoreError::Api { code, .. }) => assert_eq!(code, ErrorCode::UserInUse),
        Err(other) => panic!("expected CoreError::Api, got {other:?}"),
    }
}

#[tokio::test]
async fn ffi_login_network_failure_is_typed_network_error() {
    // Nothing is listening on this port → a transport failure, not an HTTP
    // error. It must surface as the retryable `CoreError::Network`.
    match session::login(
        "http://127.0.0.1:1".into(),
        "alice".into(),
        "hunter2".into(),
    )
    .await
    {
        Ok(_) => panic!("expected a network error, got a client"),
        Err(CoreError::Network { .. }) => {}
        Err(other) => panic!("expected CoreError::Network, got {other:?}"),
    }
}
