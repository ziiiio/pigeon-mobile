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
use std::time::Duration;

use pigeon_mobile_core::api::ErrorCode;
use pigeon_mobile_core::session::{
    login, register, restore_session, set_key_store, KeyStore, KeyStoreError, PigeonClient,
};
use pigeon_mobile_core::sync::SyncObserver;
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

/// A no-op sync observer. The conversation test polls the store directly rather
/// than reacting to callbacks, so it only needs the loop to keep running.
struct NoopObserver;
impl SyncObserver for NoopObserver {
    fn on_change(&self) {}
    fn on_status(&self, _connected: bool) {}
}

/// Run `client`'s sync loop in the background so its store folds in server state.
/// Returns the task handle; abort it to cancel the loop (Gotcha #6).
fn spawn_sync(client: Arc<PigeonClient>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // The loop only returns on a fatal error; the test aborts it when done.
        let _ = client.run_sync(Box::new(NoopObserver)).await;
    })
}

/// Poll `f` until it returns `true` or the timeout elapses. Used to wait for a
/// message to arrive over `/sync` without racing the long-poll.
async fn wait_until<F: Fn() -> bool>(label: &str, f: F) {
    for _ in 0..150 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("timed out waiting for: {label}");
}

/// The M2 exit gate: two clients hold a plaintext conversation through a real
/// homeserver — create + invite + join (membership), then messages flow both
/// ways over the sync loop. This drives the same FFI the UI does.
#[tokio::test]
async fn two_clients_hold_a_plaintext_conversation() -> anyhow::Result<()> {
    // A shared key store is fine: each client keeps its token in-core, and the
    // conversation path never calls `restore_session` (which reads the blob).
    set_key_store(Box::new(MemStore::default()));

    let (base, _ts) = spawn().await?;

    // Two real accounts on the live server.
    let alice = register(base.clone(), "alice".into(), "hunter2".into()).await?;
    let bob = register(base.clone(), "bob".into(), "hunter2".into()).await?;
    let bob_id = bob.session().user_id.clone();
    assert_eq!(bob_id, "@bob:test.example");

    // Alice creates a plaintext room and invites Bob (M2.6).
    let room = alice
        .create_room(Some("general".into()), Some("plaintext chat".into()))
        .await?;
    alice.invite(room.clone(), bob_id.clone()).await?;

    // Bob accepts by joining the room id (the server surfaces no invite list).
    bob.join_room(room.clone()).await?;

    // Both sync loops run in the background, folding server state into each store.
    let alice_sync = spawn_sync(alice.clone());
    let bob_sync = spawn_sync(bob.clone());

    // Membership: Bob's store learns about the room once his sync covers it.
    {
        let bob = bob.clone();
        let room = room.clone();
        wait_until("bob sees the room", || {
            bob.list_rooms().unwrap().iter().any(|r| r.room_id == room)
        })
        .await;
    }

    // Alice sends; Bob receives it over sync.
    alice
        .send_message(room.clone(), "hello bob".into())
        .await?;
    {
        let bob = bob.clone();
        let room = room.clone();
        wait_until("bob receives alice's message", || {
            bob.timeline(room.clone(), 100, None)
                .unwrap()
                .iter()
                .any(|e| e.body.as_deref() == Some("hello bob"))
        })
        .await;
    }

    // Bob replies; Alice receives it — a full round-trip conversation.
    bob.send_message(room.clone(), "hi alice".into()).await?;
    {
        let alice = alice.clone();
        let room = room.clone();
        wait_until("alice receives bob's reply", || {
            alice
                .timeline(room.clone(), 100, None)
                .unwrap()
                .iter()
                .any(|e| e.body.as_deref() == Some("hi alice"))
        })
        .await;
    }

    // Membership is visible in the timeline: Bob's join renders as a system line
    // (the core pre-renders it — no protocol parsing in the UI, Gotcha #9).
    let alice_tl = alice.timeline(room.clone(), 100, None)?;
    assert!(
        alice_tl
            .iter()
            .any(|e| e.system_text.as_deref() == Some("@bob:test.example joined")),
        "alice's timeline shows bob's join as a system line; got: {:?}",
        alice_tl
            .iter()
            .map(|e| (&e.body, &e.system_text))
            .collect::<Vec<_>>()
    );

    // Cancel both loops — drops the in-flight `/sync` (Gotcha #6).
    alice_sync.abort();
    bob_sync.abort();

    Ok(())
}

/// The M3 exit gate: two clients exchange **real MLS-encrypted** messages through
/// the real homeserver. Alice creates an encrypted room and invites Bob (which
/// claims his KeyPackage, adds him to the group, and ships the Welcome
/// to-device); Bob's sync joins the group and decrypts Alice's message; the reply
/// round-trips. Finally we fetch the room's stored events straight from the server
/// and assert they carry only ciphertext — the server never sees plaintext.
#[tokio::test]
async fn two_clients_exchange_encrypted_messages() -> anyhow::Result<()> {
    let store = MemStore::default();
    set_key_store(Box::new(store.clone()));
    let (base, _ts) = spawn().await?;

    // Two real accounts; each publishes its device keys on register.
    let alice = register(base.clone(), "alice".into(), "hunter2".into()).await?;
    let bob = register(base.clone(), "bob".into(), "hunter2".into()).await?;
    let bob_id = bob.session().user_id.clone();

    // Alice creates an ENCRYPTED room and invites Bob — this claims Bob's
    // KeyPackage, adds him to the MLS group, and sends the Welcome to-device.
    let room = alice
        .create_encrypted_room(Some("secret room".into()), None)
        .await?;
    alice.invite(room.clone(), bob_id.clone()).await?;
    // Bob accepts (server-side membership) so he receives the timeline.
    bob.join_room(room.clone()).await?;

    let alice_sync = spawn_sync(alice.clone());
    let bob_sync = spawn_sync(bob.clone());

    // Alice sends an encrypted message. Bob's sync must pick up the Welcome (via
    // to_device), join the group, and DECRYPT the message — proving real MLS
    // interop through the real server, not a canned mock.
    let secret = "the eagle lands at dawn";
    alice.send_message(room.clone(), secret.into()).await?;
    {
        let bob = bob.clone();
        let room = room.clone();
        wait_until("bob decrypts alice's encrypted message", || {
            bob.timeline(room.clone(), 100, None)
                .unwrap()
                .iter()
                .any(|e| e.body.as_deref() == Some(secret))
        })
        .await;
    }

    // Bob replies (encrypted); Alice decrypts — a full encrypted round-trip.
    let reply = "roger, moving in";
    bob.send_message(room.clone(), reply.into()).await?;
    {
        let alice = alice.clone();
        let room = room.clone();
        wait_until("alice decrypts bob's encrypted reply", || {
            alice
                .timeline(room.clone(), 100, None)
                .unwrap()
                .iter()
                .any(|e| e.body.as_deref() == Some(reply))
        })
        .await;
    }

    // The server only ever saw ciphertext: fetch the room's stored events straight
    // from the homeserver (with Bob's token, pulled from the persisted blob) and
    // assert no plaintext appears, and that the messages are p.room.encrypted.
    let token = {
        let map = store.map.lock().unwrap();
        let blob = map
            .get("pigeon.session.v1")
            .expect("a session blob is persisted");
        let v: serde_json::Value = serde_json::from_slice(blob).unwrap();
        v["access_token"].as_str().unwrap().to_owned()
    };
    let url = format!("{base}/_pigeon/client/v1/rooms/{room}/messages?limit=100");
    let body: serde_json::Value = reqwest::Client::new()
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await?
        .json()
        .await?;
    let raw = body.to_string();
    assert!(
        !raw.contains(secret) && !raw.contains(reply),
        "the server must never see message plaintext"
    );
    let chunk = body["chunk"].as_array().expect("messages chunk");
    let encrypted_count = chunk
        .iter()
        .filter(|e| e["type"] == "p.room.encrypted")
        .count();
    assert!(
        encrypted_count >= 2,
        "both messages are stored as p.room.encrypted ciphertext (got {encrypted_count})"
    );

    alice_sync.abort();
    bob_sync.abort();
    Ok(())
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

    // Snapshot the persisted (still-valid) session blob before logging out, so we
    // can prove server-side revocation independently of the local clear below.
    // (Login also persists an MLS device-state entry now — M3.1 — so grab the
    // session entry by key, not "the only entry".)
    let pre_logout_blob = store
        .map
        .lock()
        .unwrap()
        .get("pigeon.session.v1")
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
