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
use pigeon_mobile_core::e2ee::E2ee;
use pigeon_mobile_core::session;
use pigeon_mobile_core::sync::SyncObserver;
use pigeon_mobile_core::CoreError;
use serde_json::json;
use wiremock::matchers::{
    body_json, body_partial_json, header, method, path, path_regex, query_param,
};
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

// --- Rooms: create / join (M2.3) ------------------------------------------

/// Log in against `server` and return the client (in-memory store in tests).
async fn logged_in(
    server: &MockServer,
) -> std::sync::Arc<pigeon_mobile_core::session::PigeonClient> {
    Mock::given(method("POST"))
        .and(path("/_pigeon/client/v1/login"))
        .respond_with(ResponseTemplate::new(200).set_body_json(auth_body()))
        .mount(server)
        .await;
    session::login(server.uri(), "alice".into(), "hunter2".into())
        .await
        .expect("login ok")
}

#[tokio::test]
async fn create_room_posts_name_topic_and_returns_id() {
    let server = MockServer::start().await;
    let client = logged_in(&server).await;
    Mock::given(method("POST"))
        .and(path("/_pigeon/client/v1/createRoom"))
        .and(body_json(json!({ "name": "General", "topic": "chatter" })))
        .and(header("authorization", "Bearer secret-token"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "room_id": "!new:test.example" })),
        )
        .expect(1)
        .mount(&server)
        .await;

    let id = client
        .create_room(Some("General".into()), Some("chatter".into()))
        .await
        .expect("create ok");
    assert_eq!(id, "!new:test.example");
}

#[tokio::test]
async fn create_room_omits_unset_fields() {
    let server = MockServer::start().await;
    let client = logged_in(&server).await;
    // No name/topic → an empty body (plaintext M2: no `encryption` either).
    Mock::given(method("POST"))
        .and(path("/_pigeon/client/v1/createRoom"))
        .and(body_json(json!({})))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "room_id": "!x:test.example" })),
        )
        .expect(1)
        .mount(&server)
        .await;

    let id = client.create_room(None, None).await.expect("create ok");
    assert_eq!(id, "!x:test.example");
}

#[tokio::test]
async fn join_room_posts_to_join_path() {
    let server = MockServer::start().await;
    let client = logged_in(&server).await;
    Mock::given(method("POST"))
        .and(path("/_pigeon/client/v1/rooms/!r:test.example/join"))
        .and(header("authorization", "Bearer secret-token"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "room_id": "!r:test.example" })),
        )
        .expect(1)
        .mount(&server)
        .await;

    client
        .join_room("!r:test.example".into())
        .await
        .expect("join ok");
}

// --- Timeline backfill (M2.4) ---------------------------------------------

#[tokio::test]
async fn fetch_messages_pulls_chunk_and_persists_new_events() {
    let server = MockServer::start().await;
    let client = logged_in(&server).await;
    Mock::given(method("GET"))
        .and(path("/_pigeon/client/v1/rooms/!r:test.example/messages"))
        .and(query_param("limit", "50"))
        .and(header("authorization", "Bearer secret-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "chunk": [
                msg("$a", "!r:test.example", "one"),
                msg("$b", "!r:test.example", "two"),
            ]
        })))
        .mount(&server)
        .await;

    // Two events land the first time; re-fetching the same chunk is idempotent.
    let n = client
        .fetch_messages("!r:test.example".into(), 50)
        .await
        .expect("fetch ok");
    assert_eq!(n, 2);
    let again = client
        .fetch_messages("!r:test.example".into(), 50)
        .await
        .expect("fetch ok");
    assert_eq!(again, 0);

    // They are now readable through the timeline, oldest-first.
    let tl = client
        .timeline("!r:test.example".into(), 10, None)
        .expect("timeline ok");
    assert_eq!(tl.len(), 2);
    assert_eq!(tl[0].body.as_deref(), Some("one"));
    assert_eq!(tl[1].body.as_deref(), Some("two"));
}

// --- Send: local echo + delivery (M2.5) -----------------------------------

#[tokio::test]
async fn send_message_echoes_then_confirms_on_ack() {
    let server = MockServer::start().await;
    let client = logged_in(&server).await;
    Mock::given(method("PUT"))
        .and(path_regex(
            r"^/_pigeon/client/v1/rooms/!r:test\.example/send/p\.room\.message/.+$",
        ))
        .and(body_json(
            json!({ "body": "hi there", "msgtype": "p.text" }),
        ))
        .and(header("authorization", "Bearer secret-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "event_id": "$server" })))
        .expect(1)
        .mount(&server)
        .await;

    client
        .send_message("!r:test.example".into(), "hi there".into())
        .await
        .expect("send ok");

    // After the ack, the echo is the confirmed server event, not pending.
    let tl = client
        .timeline("!r:test.example".into(), 10, None)
        .expect("timeline ok");
    assert_eq!(tl.len(), 1);
    assert_eq!(tl[0].event_id, "$server");
    assert_eq!(tl[0].body.as_deref(), Some("hi there"));
    assert!(!tl[0].pending && !tl[0].failed);
}

#[tokio::test]
async fn send_message_marks_echo_failed_on_server_rejection() {
    let server = MockServer::start().await;
    let client = logged_in(&server).await;
    // No /send route mounted → the server 404s → a terminal rejection. The echo
    // survives (offline-first: the message is never lost) but is flagged failed
    // and dropped from the retry queue so we don't resend it forever.
    // (The network/offline path — echo stays *pending* for retry — is guaranteed
    // by queue_send persisting the echo before flush and flush's
    // `Err(Network) => break`, and is exercised end-to-end by the e2e lane.)
    client
        .send_message("!r:test.example".into(), "queued".into())
        .await
        .expect("send returns ok");

    let tl = client
        .timeline("!r:test.example".into(), 10, None)
        .expect("timeline ok");
    assert_eq!(tl.len(), 1);
    assert_eq!(tl[0].body.as_deref(), Some("queued"));
    assert!(tl[0].failed, "a server rejection flags the echo failed");
    assert!(!tl[0].pending);
}

// --- Invite (M2.6) --------------------------------------------------------

#[tokio::test]
async fn invite_posts_user_id_to_invite_path() {
    let server = MockServer::start().await;
    let client = logged_in(&server).await;
    Mock::given(method("POST"))
        .and(path("/_pigeon/client/v1/rooms/!r:test.example/invite"))
        .and(body_json(json!({ "user_id": "@bob:test.example" })))
        .and(header("authorization", "Bearer secret-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "event_id": "$inv" })))
        .expect(1)
        .mount(&server)
        .await;

    client
        .invite("!r:test.example".into(), "@bob:test.example".into())
        .await
        .expect("invite ok");
}

// --- Encrypted rooms + invite-with-Welcome (M3.4) -------------------------

#[tokio::test]
async fn create_encrypted_room_posts_encryption_flag() {
    let server = MockServer::start().await;
    let client = logged_in(&server).await;
    Mock::given(method("POST"))
        .and(path("/_pigeon/client/v1/createRoom"))
        .and(body_json(json!({ "name": "Secret", "encryption": true })))
        .and(header("authorization", "Bearer secret-token"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "room_id": "!enc:test.example" })),
        )
        .expect(1)
        .mount(&server)
        .await;

    let room = client
        .create_encrypted_room(Some("Secret".into()), None)
        .await
        .expect("create encrypted room ok");
    assert_eq!(room, "!enc:test.example");
}

#[tokio::test]
async fn invite_to_encrypted_room_claims_keys_and_ships_welcome() {
    let server = MockServer::start().await;
    let alice = logged_in(&server).await;

    // Alice creates an encrypted room she hosts the group for.
    Mock::given(method("POST"))
        .and(path("/_pigeon/client/v1/createRoom"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "room_id": "!enc:test.example" })),
        )
        .mount(&server)
        .await;
    let room = alice
        .create_encrypted_room(Some("Secret".into()), None)
        .await
        .expect("create encrypted room");

    // Bob is a real second device publishing a real KeyPackage (so add_member,
    // which validates it through openmls, actually succeeds).
    let bob = E2ee::create("@bob:test.example").expect("bob device");
    let bob_kp = bob.key_packages(1).expect("bob key package").remove(0);

    // The invite dance: server invite, then query → claim → sendToDevice Welcome.
    Mock::given(method("POST"))
        .and(path("/_pigeon/client/v1/rooms/!enc:test.example/invite"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "event_id": "$inv" })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/_pigeon/client/v1/keys/query"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "device_keys": { "@bob:test.example": {
                "BOBDEV": { "user_id": "@bob:test.example", "device_id": "BOBDEV",
                            "algorithms": ["p.mls.1"], "keys": {}, "signatures": {} }
            } }
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/_pigeon/client/v1/keys/claim"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "one_time_keys": { "@bob:test.example": {
                "BOBDEV": { "key_id": "kp-0", "package": bob_kp }
            } }
        })))
        .expect(1)
        .mount(&server)
        .await;
    // The Welcome must be shipped to Bob's device, tagged with the room id. The
    // opaque `welcome` blob is dynamic, so assert the surrounding shape only.
    Mock::given(method("PUT"))
        .and(path_regex(
            r"^/_pigeon/client/v1/sendToDevice/p\.mls\.welcome/.+$",
        ))
        .and(body_partial_json(json!({
            "messages": { "@bob:test.example": { "BOBDEV": { "room_id": "!enc:test.example" } } }
        })))
        .and(header("authorization", "Bearer secret-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;

    alice
        .invite(room.clone(), "@bob:test.example".into())
        .await
        .expect("invite to encrypted room ok");
    assert_eq!(room, "!enc:test.example");

    // End-to-end proof (not just request shapes): pull the Welcome we actually
    // shipped out of the recorded sendToDevice request and confirm Bob can join
    // the group from it — i.e. add_member produced a valid, self-contained MLS
    // Welcome for Bob's real KeyPackage.
    let requests = server.received_requests().await.expect("recorded requests");
    let welcome_req = requests
        .iter()
        .find(|r| r.url.path().contains("/sendToDevice/p.mls.welcome/"))
        .expect("a Welcome was sent to-device");
    let body: serde_json::Value = serde_json::from_slice(&welcome_req.body).unwrap();
    let welcome = body["messages"]["@bob:test.example"]["BOBDEV"]["welcome"]
        .as_str()
        .expect("welcome present in to-device content");

    assert!(!bob.has_group("!enc:test.example").unwrap());
    bob.join_from_welcome(welcome)
        .expect("bob joins from Welcome");
    assert!(
        bob.has_group("!enc:test.example").unwrap(),
        "Bob holds the group after joining from the shipped Welcome"
    );
}

#[tokio::test]
async fn send_in_encrypted_room_puts_p_room_encrypted_ciphertext() {
    let server = MockServer::start().await;
    let client = logged_in(&server).await;

    Mock::given(method("POST"))
        .and(path("/_pigeon/client/v1/createRoom"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "room_id": "!enc:test.example" })),
        )
        .mount(&server)
        .await;
    let room = client
        .create_encrypted_room(None, None)
        .await
        .expect("create encrypted room");

    // A send in this room must go out as p.room.encrypted ciphertext — never the
    // plaintext body. The server only ever sees ciphertext.
    Mock::given(method("PUT"))
        .and(path_regex(
            r"^/_pigeon/client/v1/rooms/!enc:test\.example/send/p\.room\.encrypted/.+$",
        ))
        .and(body_partial_json(json!({ "algorithm": "p.mls.1" })))
        .and(header("authorization", "Bearer secret-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "event_id": "$enc1" })))
        .expect(1)
        .mount(&server)
        .await;

    client
        .send_message(room.clone(), "top secret".into())
        .await
        .expect("send ok");

    // The recorded request carries ciphertext, not the plaintext body.
    let requests = server.received_requests().await.expect("recorded requests");
    let send = requests
        .iter()
        .find(|r| r.url.path().contains("/send/p.room.encrypted/"))
        .expect("an encrypted send was made");
    let body = String::from_utf8_lossy(&send.body);
    assert!(body.contains("ciphertext"), "carries ciphertext");
    assert!(
        !body.contains("top secret"),
        "the plaintext body must never be sent"
    );

    // Locally, the sender still sees the plaintext (its own echo, promoted to the
    // server's event id — it can't self-decrypt its own MLS message).
    let tl = client.timeline(room, 10, None).unwrap();
    assert_eq!(tl.last().unwrap().body.as_deref(), Some("top secret"));
}

// --- Encrypted key backup / restore (M4.3) --------------------------------

#[tokio::test]
async fn backup_puts_blob_and_returns_recovery_key() {
    let server = MockServer::start().await;
    let client = logged_in(&server).await;

    // Give the device a group so the backup has real state to protect.
    Mock::given(method("POST"))
        .and(path("/_pigeon/client/v1/createRoom"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "room_id": "!enc:test.example" })),
        )
        .mount(&server)
        .await;
    client
        .create_encrypted_room(None, None)
        .await
        .expect("create encrypted room");

    // The backup is stored in the reserved room_keys slot as an opaque blob.
    Mock::given(method("PUT"))
        .and(path(
            "/_pigeon/client/v1/room_keys/key/!e2ee-backup/mls-device-state",
        ))
        .and(body_partial_json(json!({})))
        .and(header("authorization", "Bearer secret-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;

    let recovery_key = client.backup().await.expect("backup ok");
    assert!(!recovery_key.is_empty(), "a recovery key is returned");

    // The stored body carries a base64 blob, never anything plaintext-identifying.
    let requests = server.received_requests().await.expect("recorded requests");
    let put = requests
        .iter()
        .find(|r| r.url.path().contains("/room_keys/key/"))
        .expect("a backup PUT was made");
    let body: serde_json::Value = serde_json::from_slice(&put.body).unwrap();
    assert!(body["blob"].as_str().is_some(), "stores a base64 blob");
}

#[tokio::test]
async fn restore_backup_fetches_blob_and_recovers_identity() {
    // Produce a real backup blob from a standalone device, then have a freshly
    // logged-in client restore from it (the server serves the stored blob).
    let donor = E2ee::create("@alice:test.example").expect("donor device");
    donor.create_group("!enc:test.example").expect("group");
    let (recovery_key, blob) = donor.create_backup().expect("backup");

    let server = MockServer::start().await;
    let client = logged_in(&server).await;
    Mock::given(method("GET"))
        .and(path(
            "/_pigeon/client/v1/room_keys/key/!e2ee-backup/mls-device-state",
        ))
        .and(header("authorization", "Bearer secret-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "blob": blob })))
        .expect(1)
        .mount(&server)
        .await;

    // Restoring swaps in the recovered identity + groups (key re-publish is
    // best-effort — /keys/upload 404s here, which is fine).
    client
        .restore_backup(recovery_key)
        .await
        .expect("restore ok");
}

#[tokio::test]
async fn restore_backup_errors_when_no_backup_on_server() {
    let server = MockServer::start().await;
    let client = logged_in(&server).await;
    Mock::given(method("GET"))
        .and(path(
            "/_pigeon/client/v1/room_keys/key/!e2ee-backup/mls-device-state",
        ))
        .respond_with(
            ResponseTemplate::new(404).set_body_json(
                json!({ "errcode": "P_NOT_FOUND", "error": "no such backed-up key" }),
            ),
        )
        .mount(&server)
        .await;

    match client.restore_backup("cmVjb3Zlcnk=".into()).await {
        Err(CoreError::Crypto { .. }) => {}
        other => panic!("expected a Crypto error for a missing backup, got {other:?}"),
    }
}

// --- Media upload / download / image messages (M4.1) ----------------------

#[tokio::test]
async fn upload_media_posts_raw_bytes_and_returns_uri() {
    let server = MockServer::start().await;
    let client = logged_in(&server).await;
    Mock::given(method("POST"))
        .and(path("/_pigeon/media/v1/upload"))
        .and(header("content-type", "image/png"))
        .and(header("authorization", "Bearer secret-token"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "content_uri": "pigeon://test.example/mediaABC" })),
        )
        .expect(1)
        .mount(&server)
        .await;

    let uri = client
        .upload_media(b"\x89PNG rawbytes".to_vec(), "image/png".into())
        .await
        .expect("upload ok");
    assert_eq!(uri, "pigeon://test.example/mediaABC");

    // The raw bytes were sent verbatim (not JSON-wrapped).
    let requests = server.received_requests().await.unwrap();
    let up = requests
        .iter()
        .find(|r| r.url.path() == "/_pigeon/media/v1/upload")
        .unwrap();
    assert_eq!(up.body, b"\x89PNG rawbytes");
}

#[tokio::test]
async fn download_media_fetches_raw_bytes_by_uri() {
    let server = MockServer::start().await;
    let client = logged_in(&server).await;
    Mock::given(method("GET"))
        .and(path("/_pigeon/media/v1/download/test.example/mediaABC"))
        .and(header("authorization", "Bearer secret-token"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"\x89PNG rawbytes".to_vec()))
        .expect(1)
        .mount(&server)
        .await;

    let bytes = client
        .download_media("pigeon://test.example/mediaABC".into())
        .await
        .expect("download ok");
    assert_eq!(bytes, b"\x89PNG rawbytes");
}

#[tokio::test]
async fn download_media_rejects_a_malformed_uri() {
    let server = MockServer::start().await;
    let client = logged_in(&server).await;
    match client.download_media("https://not-pigeon/x".into()).await {
        Err(CoreError::Protocol { .. }) => {}
        other => panic!("expected a Protocol error, got {other:?}"),
    }
}

#[tokio::test]
async fn send_image_uploads_bytes_then_posts_a_p_image_message() {
    let server = MockServer::start().await;
    let client = logged_in(&server).await;
    // In a plaintext room, send_image uploads the raw bytes then references the
    // returned URL in a p.image message (no encryption).
    Mock::given(method("POST"))
        .and(path("/_pigeon/media/v1/upload"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "content_uri": "pigeon://test.example/mediaABC" })),
        )
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path_regex(
            r"^/_pigeon/client/v1/rooms/!r:test\.example/send/p\.room\.message/.+$",
        ))
        .and(body_partial_json(json!({
            "msgtype": "p.image",
            "body": "cat.jpg",
            "url": "pigeon://test.example/mediaABC",
            "info": { "mimetype": "image/png" }
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "event_id": "$img" })))
        .expect(1)
        .mount(&server)
        .await;

    client
        .send_image(
            "!r:test.example".into(),
            b"\x89PNG rawbytes".to_vec(),
            "image/png".into(),
            4,
            3,
            "cat.jpg".into(),
        )
        .await
        .expect("send image ok");
}

#[tokio::test]
async fn upload_media_rejects_oversize_client_side() {
    let server = MockServer::start().await;
    let client = logged_in(&server).await;
    // One byte over the 50 MiB cap — rejected before any network call.
    let too_big = vec![0u8; 50 * 1024 * 1024 + 1];
    match client.upload_media(too_big, "image/png".into()).await {
        Err(CoreError::Api {
            code: ErrorCode::LimitExceeded,
            ..
        }) => {}
        other => panic!("expected a LimitExceeded error, got {other:?}"),
    }
}

#[tokio::test]
async fn send_image_in_encrypted_room_encrypts_bytes_and_carries_key_in_event() {
    let server = MockServer::start().await;
    let client = logged_in(&server).await;
    Mock::given(method("POST"))
        .and(path("/_pigeon/client/v1/createRoom"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "room_id": "!enc:test.example" })),
        )
        .mount(&server)
        .await;
    let room = client
        .create_encrypted_room(None, None)
        .await
        .expect("create encrypted room");

    Mock::given(method("POST"))
        .and(path("/_pigeon/media/v1/upload"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "content_uri": "pigeon://test.example/ct1" })),
        )
        .expect(1)
        .mount(&server)
        .await;
    // The message must be p.room.encrypted (ciphertext only) — NOT p.image.
    Mock::given(method("PUT"))
        .and(path_regex(
            r"^/_pigeon/client/v1/rooms/!enc:test\.example/send/p\.room\.encrypted/.+$",
        ))
        .and(body_partial_json(json!({ "algorithm": "p.mls.1" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "event_id": "$enc" })))
        .expect(1)
        .mount(&server)
        .await;

    let plaintext_bytes = b"\x89PNG the secret photo".to_vec();
    client
        .send_image(
            room,
            plaintext_bytes.clone(),
            "image/png".into(),
            4,
            3,
            "secret.png".into(),
        )
        .await
        .expect("send encrypted image ok");

    let requests = server.received_requests().await.unwrap();
    // What was uploaded is ciphertext, never the plaintext image bytes.
    let upload = requests
        .iter()
        .find(|r| r.url.path() == "/_pigeon/media/v1/upload")
        .expect("an upload was made");
    assert_ne!(
        upload.body, plaintext_bytes,
        "the uploaded bytes must be ciphertext, not the plaintext image"
    );
    // The message on the wire is encrypted ciphertext — no plaintext caption/url.
    let send = requests
        .iter()
        .find(|r| r.url.path().contains("/send/p.room.encrypted/"))
        .expect("an encrypted send was made");
    let body = String::from_utf8_lossy(&send.body);
    assert!(body.contains("ciphertext"));
    assert!(
        !body.contains("secret.png"),
        "no plaintext caption on the wire"
    );
    assert!(
        !body.contains("pigeon://test.example/ct1"),
        "no plaintext url on the wire"
    );
    // And crucially there is no p.room.message (p.image) leak.
    assert!(
        !requests
            .iter()
            .any(|r| r.url.path().contains("/send/p.room.message/")),
        "an encrypted-room image must not send a plaintext p.image message"
    );
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
