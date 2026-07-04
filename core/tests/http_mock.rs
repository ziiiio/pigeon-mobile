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

use std::sync::{Arc, Mutex};

use pigeon_mobile_core::api::{Api, ErrorCode};
use pigeon_mobile_core::session;
use pigeon_mobile_core::sync::SyncObserver;
use pigeon_mobile_core::CoreError;
use serde_json::json;
use wiremock::matchers::{body_json, header, method, path, query_param};
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

// --- Sync (M2.2) ----------------------------------------------------------

/// A `/sync` body with one joined room carrying `events`.
fn sync_body(next_batch: &str, room_id: &str, events: serde_json::Value) -> serde_json::Value {
    json!({
        "next_batch": next_batch,
        "rooms": { "join": { room_id: { "timeline": { "events": events, "limited": false } } } },
        "to_device": { "events": [] }
    })
}

fn msg(id: &str, room: &str, body: &str) -> serde_json::Value {
    json!({
        "event_id": id, "room_id": room, "sender": "@alice:test.example",
        "type": "p.room.message", "origin_server_ts": 100, "depth": 1,
        "content": { "body": body, "msgtype": "p.text" }
    })
}

#[tokio::test]
async fn sync_sends_since_timeout_limit_and_bearer() {
    let server = MockServer::start().await;
    // First sync: no `since`, with the timeout/limit query and the bearer.
    Mock::given(method("GET"))
        .and(path("/_pigeon/client/v1/sync"))
        .and(query_param("timeout", "30000"))
        .and(query_param("limit", "100"))
        .and(header("authorization", "Bearer secret-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(sync_body(
            "9_0",
            "!r:test.example",
            json!([msg("$a", "!r:test.example", "hi")]),
        )))
        .expect(1..)
        .mount(&server)
        .await;

    let api = Api::new(server.uri(), Some("secret-token".to_owned())).unwrap();
    let resp = api.sync(None, 30_000, 100).await.expect("sync ok");
    assert_eq!(resp["next_batch"], "9_0");
}

/// Captures the observer callbacks so the test can assert the loop drove them.
#[derive(Clone, Default)]
struct Recorder {
    changes: Arc<Mutex<u32>>,
    last_connected: Arc<Mutex<Option<bool>>>,
}
impl SyncObserver for Recorder {
    fn on_change(&self) {
        *self.changes.lock().unwrap() += 1;
    }
    fn on_status(&self, connected: bool) {
        *self.last_connected.lock().unwrap() = Some(connected);
    }
}

#[tokio::test]
async fn run_sync_applies_a_batch_and_notifies_then_cancels() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/_pigeon/client/v1/login"))
        .respond_with(ResponseTemplate::new(200).set_body_json(auth_body()))
        .mount(&server)
        .await;
    // Every /sync returns the same one-event batch; the first application is a
    // real change, replays are no-ops (idempotent) — so on_change fires once.
    Mock::given(method("GET"))
        .and(path("/_pigeon/client/v1/sync"))
        .respond_with(ResponseTemplate::new(200).set_body_json(sync_body(
            "1_0",
            "!r:test.example",
            json!([msg("$a", "!r:test.example", "hello")]),
        )))
        .mount(&server)
        .await;

    let client = session::login(server.uri(), "alice".into(), "hunter2".into())
        .await
        .expect("login ok");
    let rec = Recorder::default();

    // The loop runs forever; the host cancels it. Simulate that with a timeout —
    // cancellation drops the future mid-loop, exactly as a cancelled coroutine
    // would (Gotcha #6).
    let _ = tokio::time::timeout(
        std::time::Duration::from_millis(300),
        client.run_sync(Box::new(rec.clone())),
    )
    .await;

    // The batch landed in the store, on_change fired, and we saw "connected".
    assert_eq!(client.session().user_id, "@alice:test.example");
    assert_eq!(
        *rec.changes.lock().unwrap(),
        1,
        "one real change, replays no-op"
    );
    assert_eq!(*rec.last_connected.lock().unwrap(), Some(true));
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
