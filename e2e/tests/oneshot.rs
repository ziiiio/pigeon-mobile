//! Oneshot-homeserver end-to-end test — the M1 exit gate.
//!
//! Unlike the mock-HTTP tests in `core`, this drives the real `pigeon` server
//! (in-process, over a real TCP socket, backed by a real Postgres spun via
//! testcontainers) through the core's own FFI functions. It proves the client
//! half agrees with the actual wire contract, not a canned mock.
//!
//! Requires Docker. Run from `e2e/`:  `cargo test`. Not built by the dev
//! container's core workflow.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use pigeon_mobile_core::api::ErrorCode;
use pigeon_mobile_core::session::{
    login, register, restore_session, set_key_store, KeyStore, KeyStoreError,
};
use pigeon_mobile_core::CoreError;
use tokio::net::TcpListener;

/// An in-memory key store so the restore path has somewhere to persist to.
#[derive(Clone, Default)]
struct MemStore {
    map: Arc<Mutex<HashMap<String, Vec<u8>>>>,
}

impl KeyStore for MemStore {
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

/// Boot the in-process homeserver on a real TCP port and return its base URL.
/// The returned `TestServer` owns the Postgres container — keep it in scope for
/// the duration of the test.
async fn spawn() -> anyhow::Result<(String, tests_integration::TestServer)> {
    let ts = tests_integration::spawn_server().await?;
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let addr = listener.local_addr()?;
    let router = ts.router.clone();
    tokio::spawn(async move {
        axum::serve(listener, router.into_make_service())
            .await
            .expect("server task");
    });
    Ok((format!("http://{addr}"), ts))
}

#[tokio::test]
async fn register_login_restore_against_real_homeserver() -> anyhow::Result<()> {
    // A key store must be installed so login/register persist and restore has
    // something to reload. Keep a handle so the logout assertions below can
    // re-inject the pre-logout blob and prove the token was revoked server-side.
    let store = MemStore::default();
    set_key_store(Box::new(store.clone()));

    let (base, _ts) = spawn().await?;

    // Register a fresh account — the server stamps the full id + a device id.
    let client = register(base.clone(), "alice".into(), "hunter2".into()).await?;
    assert_eq!(client.session().user_id, "@alice:test.example");
    assert!(!client.session().device_id.is_empty());
    assert_eq!(client.session().server, base);

    // Log in with the same credentials.
    let client2 = login(base.clone(), "alice".into(), "hunter2".into()).await?;
    assert_eq!(client2.session().user_id, "@alice:test.example");

    // Wrong password → the server's P_FORBIDDEN, surfaced as a typed code.
    match login(base.clone(), "alice".into(), "nope".into()).await {
        Ok(_) => panic!("wrong password should fail"),
        Err(CoreError::Api { code, .. }) => assert_eq!(code, ErrorCode::Forbidden),
        Err(other) => panic!("expected Forbidden, got {other:?}"),
    }

    // Duplicate registration → P_USER_IN_USE.
    match register(base.clone(), "alice".into(), "hunter2".into()).await {
        Ok(_) => panic!("duplicate register should fail"),
        Err(CoreError::Api { code, .. }) => assert_eq!(code, ErrorCode::UserInUse),
        Err(other) => panic!("expected UserInUse, got {other:?}"),
    }

    // The last successful login persisted a session; restore validates the token
    // against the REAL `/account/whoami` and hands it back.
    let restored = restore_session().await?.expect("a session was restored");
    assert_eq!(restored.session().user_id, "@alice:test.example");

    // Snapshot the persisted (still-valid) blob before logging out, so we can
    // prove server-side revocation independently of the local clear below.
    let pre_logout_blob = store
        .map
        .lock()
        .unwrap()
        .values()
        .next()
        .cloned()
        .expect("a session blob is persisted before logout");

    // Log out: revoke the token against the REAL server, then clear local state.
    restored.logout().await?;

    // The local session is gone — a fresh restore finds nothing to reload.
    assert!(
        restore_session().await?.is_none(),
        "logout cleared the persisted session"
    );

    // And the token really is dead server-side: re-inject the old blob and let
    // restore validate it against the REAL `/account/whoami`. A revoked token
    // comes back as P_UNKNOWN_TOKEN, so restore clears it and yields None —
    // proving logout revoked server-side, not just wiped the local keystore.
    store
        .map
        .lock()
        .unwrap()
        .insert("pigeon.session.v1".into(), pre_logout_blob);
    assert!(
        restore_session().await?.is_none(),
        "the token was revoked server-side; restore rejects and clears it"
    );

    Ok(())
}
