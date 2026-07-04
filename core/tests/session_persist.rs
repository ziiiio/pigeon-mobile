//! Keystore persistence + restore tests (M1.3), against a mock homeserver and a
//! mock key store. They drive the real FFI surface — `set_key_store`, `login`,
//! `restore_session` — and cover the offline-first restore decisions.
//!
//! These share the process-global key store, so they're `#[serial]`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use pigeon_mobile_core::session::{login, restore_session, set_key_store, KeyStore, KeyStoreError};
use serde_json::json;
use serial_test::serial;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// An in-memory `KeyStore` whose backing map the test retains a handle to, so it
/// can assert what the core persisted/deleted.
#[derive(Clone, Default)]
struct MockKeyStore {
    map: Arc<Mutex<HashMap<String, Vec<u8>>>>,
}

impl KeyStore for MockKeyStore {
    fn put(&self, key: String, value: Vec<u8>) -> Result<(), KeyStoreError> {
        self.map.lock().unwrap().insert(key, value);
        Ok(())
    }
    fn get(&self, key: String) -> Result<Option<Vec<u8>>, KeyStoreError> {
        Ok(self.map.lock().unwrap().get(&key).cloned())
    }
    fn delete(&self, key: String) -> Result<(), KeyStoreError> {
        self.map.lock().unwrap().remove(&key);
        Ok(())
    }
}

/// The keystore entry holding the session blob (identity + token). Distinct from
/// the MLS device-state entry that login/logout also write/clear (M3.1), so the
/// assertions below target the session entry by key rather than "the only entry".
const SESSION_KEY: &str = "pigeon.session.v1";

impl MockKeyStore {
    /// Install a fresh mock key store and keep a handle to inspect it.
    fn install() -> Self {
        let store = MockKeyStore::default();
        set_key_store(Box::new(store.clone()));
        store
    }
    /// Whether a session blob is currently persisted.
    fn has_session(&self) -> bool {
        self.map.lock().unwrap().contains_key(SESSION_KEY)
    }
    /// The persisted session blob, decoded as JSON (`None` if absent).
    fn session_blob(&self) -> Option<serde_json::Value> {
        self.map
            .lock()
            .unwrap()
            .get(SESSION_KEY)
            .map(|bytes| serde_json::from_slice(bytes).expect("stored blob is JSON"))
    }
}

fn auth_body() -> serde_json::Value {
    json!({
        "user_id": "@alice:test.example",
        "device_id": "DEVICE1",
        "access_token": "secret-token"
    })
}

async fn mount_login(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/_pigeon/client/v1/login"))
        .respond_with(ResponseTemplate::new(200).set_body_json(auth_body()))
        .mount(server)
        .await;
}

#[tokio::test]
#[serial]
async fn login_persists_session_blob() {
    let store = MockKeyStore::install();
    let server = MockServer::start().await;
    mount_login(&server).await;

    login(server.uri(), "alice".into(), "hunter2".into())
        .await
        .expect("login ok");

    // The identity AND the token were persisted (the whole blob is keystore-
    // protected at rest — Gotcha #1).
    let blob = store.session_blob().expect("a session blob was persisted");
    assert_eq!(blob["user_id"], "@alice:test.example");
    assert_eq!(blob["device_id"], "DEVICE1");
    assert_eq!(blob["server"], server.uri());
    assert_eq!(blob["access_token"], "secret-token");
}

#[tokio::test]
#[serial]
async fn restore_validates_token_and_returns_session() {
    let _store = MockKeyStore::install();
    let server = MockServer::start().await;
    mount_login(&server).await;
    Mock::given(method("GET"))
        .and(path("/_pigeon/client/v1/account/whoami"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "user_id": "@alice:test.example", "device_id": "DEVICE1" })),
        )
        .mount(&server)
        .await;

    login(server.uri(), "alice".into(), "hunter2".into())
        .await
        .expect("login ok");

    let restored = restore_session().await.expect("restore ok");
    let client = restored.expect("a session was restored");
    assert_eq!(client.session().user_id, "@alice:test.example");
    assert_eq!(client.session().server, server.uri());
}

#[tokio::test]
#[serial]
async fn restore_clears_revoked_token() {
    let store = MockKeyStore::install();
    let server = MockServer::start().await;
    mount_login(&server).await;
    // The stored token is no longer recognised by the server.
    Mock::given(method("GET"))
        .and(path("/_pigeon/client/v1/account/whoami"))
        .respond_with(ResponseTemplate::new(401).set_body_json(
            json!({ "errcode": "P_UNKNOWN_TOKEN", "error": "access token is not recognised" }),
        ))
        .mount(&server)
        .await;

    login(server.uri(), "alice".into(), "hunter2".into())
        .await
        .expect("login ok");
    assert!(store.has_session(), "login persisted a session");

    let restored = restore_session().await.expect("restore ok");
    assert!(restored.is_none(), "a revoked token yields no session");
    assert!(!store.has_session(), "the dead session was cleared");
}

#[tokio::test]
#[serial]
async fn restore_is_optimistic_when_offline() {
    let store = MockKeyStore::install();
    let server = MockServer::start().await;
    mount_login(&server).await;

    login(server.uri(), "alice".into(), "hunter2".into())
        .await
        .expect("login ok");

    // Repoint the persisted session at a dead port so restore's whoami fails at
    // the transport layer — the offline-first path. (Dropping the mock server
    // isn't synchronous, so it can't be relied on to refuse connections.)
    {
        let mut map = store.map.lock().unwrap();
        let mut blob: serde_json::Value = serde_json::from_slice(&map[SESSION_KEY]).unwrap();
        blob["server"] = json!("http://127.0.0.1:1");
        map.insert(SESSION_KEY.into(), blob.to_string().into_bytes());
    }

    let restored = restore_session().await.expect("restore ok");
    assert!(
        restored.is_some(),
        "offline restore keeps the session rather than logging out"
    );
    assert!(
        store.has_session(),
        "the session was NOT cleared while offline"
    );
}

#[tokio::test]
#[serial]
async fn restore_with_no_stored_session_is_none() {
    let _store = MockKeyStore::install(); // fresh + empty
    let restored = restore_session().await.expect("restore ok");
    assert!(restored.is_none());
}

#[tokio::test]
#[serial]
async fn logout_revokes_token_and_clears_session() {
    let store = MockKeyStore::install();
    let server = MockServer::start().await;
    mount_login(&server).await;
    // The revoke endpoint must be hit with this session's bearer token.
    Mock::given(method("POST"))
        .and(path("/_pigeon/client/v1/logout"))
        .and(header("authorization", "Bearer secret-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;

    let client = login(server.uri(), "alice".into(), "hunter2".into())
        .await
        .expect("login ok");
    assert!(store.has_session(), "login persisted a session");

    client.logout().await.expect("logout ok");
    assert!(!store.has_session(), "logout cleared the local session");
}

#[tokio::test]
#[serial]
async fn logout_clears_session_even_when_server_revoke_fails() {
    // No `/logout` mock is mounted, so the revoke returns a 404. Logout must
    // still wipe the local session (best-effort revoke — offline-friendly).
    let store = MockKeyStore::install();
    let server = MockServer::start().await;
    mount_login(&server).await;

    let client = login(server.uri(), "alice".into(), "hunter2".into())
        .await
        .expect("login ok");
    assert!(store.has_session());

    client
        .logout()
        .await
        .expect("logout ok despite server revoke failing");
    assert!(
        !store.has_session(),
        "the local session is cleared regardless of the server's response"
    );

    // And a subsequent restore finds nothing — the user is fully signed out.
    let restored = restore_session().await.expect("restore ok");
    assert!(restored.is_none(), "no session survives logout");
}
