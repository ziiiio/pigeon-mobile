//! The `/sync` long-poll loop (M2.2).
//!
//! [`PigeonClient::run_sync`] drives the client's single source of truth: it
//! long-polls `/sync`, folds each batch into the local [`crate::store`], and
//! notifies the host so the UI can re-read. The token that threads the poll is
//! the server's opaque composite `next_batch` — stored and returned verbatim,
//! never parsed (CLAUDE.md Gotcha #5).
//!
//! **Cancellation (Gotcha #6).** `run_sync` is an endless async fn; the host
//! runs it inside a cancellable coroutine (Android `viewModelScope`) and cancels
//! it when the app backgrounds or the screen closes. UniFFI drops the Rust
//! future at the next `.await`, which cancels the in-flight `reqwest` request —
//! no leaked sockets, no per-screen sync tasks piling up.
//!
//! **Offline-first.** A transport failure doesn't end the loop: it reports
//! disconnected, backs off, and retries. Only a fatal server error (a revoked
//! token) returns `Err`, letting the UI drop to the signed-out state.

use serde_json::Value;
use std::time::Duration;

use crate::session::PigeonClient;
use crate::{CoreError, LogLevel};

/// How long the server may hold a `/sync` open waiting for events. The server
/// hard-caps at 60s; 30s is a battery/liveness middle ground.
const SYNC_TIMEOUT_MS: u64 = 30_000;
/// Max events per room per batch (server caps at 500). Coarse batching keeps the
/// FFI/DB work per round bounded (Gotcha #7).
const SYNC_LIMIT: u32 = 100;
/// Reconnect backoff bounds after a transport failure.
const BACKOFF_START: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);

/// A host-provided observer of sync progress. The native layer implements it and
/// refreshes its view-models on `on_change`. Kept deliberately coarse (Gotcha
/// #7): it signals *that* something changed, and the UI re-reads the store —
/// rather than streaming per-event deltas across the FFI.
#[uniffi::export(callback_interface)]
pub trait SyncObserver: Send + Sync {
    /// The store changed (a batch with new events landed). Re-read room list /
    /// timeline from the store.
    fn on_change(&self);
    /// Connectivity to the homeserver changed: `true` after a successful sync,
    /// `false` when a transport error puts the loop into backoff. Drives an
    /// offline indicator; not an error the UI must act on.
    fn on_status(&self, connected: bool);
}

#[uniffi::export(async_runtime = "tokio")]
impl PigeonClient {
    /// Run the sync loop until cancelled (the host cancels the coroutine) or a
    /// fatal error occurs. Persists the opaque token, applies each batch to the
    /// store, and calls `observer.on_change()` when new events land.
    ///
    /// Returns `Ok(())` only if the loop is asked to stop cleanly (it otherwise
    /// runs forever); `Err` on a fatal server/protocol error (e.g. the token was
    /// revoked), which the UI treats as "signed out".
    pub async fn run_sync(&self, observer: Box<dyn SyncObserver>) -> Result<(), CoreError> {
        let mut backoff = BACKOFF_START;
        loop {
            // Read the cursor fresh each round: the opaque token, passed verbatim.
            let since = self.store.load_sync_token()?;
            match self
                .api
                .sync(since.as_deref(), SYNC_TIMEOUT_MS, SYNC_LIMIT)
                .await
            {
                Ok(resp) => {
                    let applied = apply_sync(self, &resp)?;
                    // Process inbound to-device messages — notably MLS Welcomes
                    // that add us to encrypted groups (M3.3). Idempotent on
                    // at-least-once delivery (Gotcha #8). Do this BEFORE decrypt
                    // so a Welcome and the first message in the same batch work.
                    let joined = apply_to_device(self, &resp);
                    // Decrypt any newly-arrived encrypted events and cache their
                    // plaintext (M3.5) before signalling — so the UI re-reads
                    // plaintext, not pending ciphertext.
                    let decrypted = self.decrypt_pending()?;
                    // We're online — a good moment to (re)transmit queued sends
                    // (offline-first retry, M2.5). Any of these can change the store.
                    let flushed = self.flush_pending().await?;
                    backoff = BACKOFF_START;
                    observer.on_status(true);
                    if applied || joined || decrypted || flushed {
                        observer.on_change();
                    }
                }
                // Offline / unreachable: report disconnected, back off, retry.
                // The loop survives — do not surface network blips as fatal.
                Err(crate::api::ApiError::Network { reason }) => {
                    crate::emit(
                        LogLevel::Info,
                        "sync",
                        &format!("sync transport error, backing off {backoff:?}: {reason}"),
                    );
                    observer.on_status(false);
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(BACKOFF_MAX);
                }
                // Anything else (revoked token, protocol mismatch) is fatal —
                // end the loop and let the UI react (likely log out).
                Err(other) => return Err(other.into()),
            }
        }
    }
}

/// Fold one `/sync` response into the store: append every joined room's timeline
/// events (idempotently, in one transaction) and advance the stored token.
/// Returns whether any new event landed (so the caller can skip a no-op refresh).
/// Pure over the store — no network — so it is unit-tested directly.
fn apply_sync(client: &PigeonClient, resp: &Value) -> Result<bool, CoreError> {
    // Flatten every joined room's timeline into one batch. Each event carries
    // its own `room_id`, so the store routes them without the room key here.
    let mut events: Vec<Value> = Vec::new();
    if let Some(join) = resp["rooms"]["join"].as_object() {
        for room in join.values() {
            if let Some(timeline) = room["timeline"]["events"].as_array() {
                events.extend(timeline.iter().cloned());
            }
        }
    }

    let inserted = client.store.apply_events(&events)?;

    // Advance the cursor. Persist verbatim even when the batch was empty — the
    // server may still move the position forward (Gotcha #5: never synthesise).
    if let Some(next) = resp["next_batch"].as_str() {
        client.store.save_sync_token(next)?;
    }
    Ok(inserted > 0)
}

/// Process the `to_device.events` block of a `/sync` response (M3.3): join any
/// MLS group we've been invited to via a `p.mls.welcome`. Returns whether a new
/// group was joined (so the caller can refresh — a newly-joined room's ciphertext
/// becomes decryptable). **Never fatal** — a bad/duplicate Welcome is logged and
/// skipped so it can't wedge the sync loop; and it is **idempotent** on
/// at-least-once delivery (Gotcha #8) by skipping a Welcome for a room whose group
/// we already hold. Never logs key material (Gotcha #2).
fn apply_to_device(client: &PigeonClient, resp: &Value) -> bool {
    let Some(events) = resp["to_device"]["events"].as_array() else {
        return false;
    };
    let Some(e2ee) = client.e2ee.as_ref() else {
        return false; // E2EE unavailable this session — nothing to process.
    };

    let mut joined = false;
    for ev in events {
        if ev["type"] != "p.mls.welcome" {
            continue;
        }
        let Some(welcome_b64) = ev["content"]["welcome"].as_str() else {
            continue;
        };
        // The Welcome carries its room id out-of-band, letting us dedup
        // at-least-once redeliveries: if we already hold the group, skip.
        let room_id = ev["content"]["room_id"].as_str();
        if let Some(room) = room_id {
            match e2ee.has_group(room) {
                Ok(true) => continue,
                Ok(false) => {}
                Err(err) => {
                    crate::emit(
                        LogLevel::Warn,
                        "e2ee",
                        &format!("could not check group membership for {room}: {err}"),
                    );
                    continue;
                }
            }
        }
        match e2ee.join_from_welcome(welcome_b64) {
            Ok(()) => {
                joined = true;
                crate::emit(
                    LogLevel::Info,
                    "e2ee",
                    &format!(
                        "joined encrypted group{}",
                        room_id.map(|r| format!(" for {r}")).unwrap_or_default()
                    ),
                );
            }
            Err(err) => crate::emit(
                LogLevel::Warn,
                "e2ee",
                &format!("failed to join from Welcome (skipping): {err}"),
            ),
        }
    }
    joined
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    // `login` (in `client()`) now also mints an MLS identity that persists through
    // the process-global key store (M3.1). The e2ee unit tests in this same test
    // binary install their own key store, so serialise anything that logs in to
    // avoid racing on that global. (No key store is installed here, so the writes
    // are no-ops — but the guard keeps it that way if one ever is.)
    use serial_test::serial;

    /// A `/sync`-shaped response with one joined room carrying `events`.
    fn sync_response(next_batch: &str, room_id: &str, events: Value) -> Value {
        json!({
            "next_batch": next_batch,
            "rooms": { "join": { room_id: { "timeline": { "events": events, "limited": false } } } },
            "to_device": { "events": [] }
        })
    }

    fn message(id: &str, room: &str, sender: &str, depth: i64, ts: i64, body: &str) -> Value {
        json!({
            "event_id": id, "room_id": room, "sender": sender, "type": "p.room.message",
            "origin_server_ts": ts, "depth": depth,
            "content": { "body": body, "msgtype": "p.text" }
        })
    }

    // apply_sync is exercised end-to-end (real HTTP + real cancellation) by the
    // `e2e/` oneshot-homeserver lane; these unit tests pin the folding + token
    // advance without a network, driving the store through a real client built
    // by the in-memory-store login path.
    async fn client() -> std::sync::Arc<PigeonClient> {
        // No store dir set in tests → in-memory store; wiremock backs the login.
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/_pigeon/client/v1/login"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "user_id": "@t:s", "device_id": "D", "access_token": "tok"
            })))
            .mount(&server)
            .await;
        crate::session::login(server.uri(), "t".into(), "p".into())
            .await
            .unwrap()
    }

    #[tokio::test]
    #[serial]
    async fn apply_sync_folds_events_and_advances_token() {
        let client = client().await;
        let resp = sync_response(
            "10_0",
            "!r:s",
            json!([
                message("$a", "!r:s", "@t:s", 1, 100, "hi"),
                message("$b", "!r:s", "@t:s", 2, 200, "there"),
            ]),
        );
        assert!(apply_sync(&client, &resp).unwrap());
        assert_eq!(
            client.store.load_sync_token().unwrap().as_deref(),
            Some("10_0")
        );
        assert_eq!(client.store.timeline("!r:s", 10, None).unwrap().len(), 2);
    }

    #[tokio::test]
    #[serial]
    async fn apply_sync_is_idempotent_and_reports_no_change_on_replay() {
        let client = client().await;
        let resp = sync_response(
            "5_0",
            "!r:s",
            json!([message("$a", "!r:s", "@t:s", 1, 100, "hi")]),
        );
        assert!(apply_sync(&client, &resp).unwrap());
        // Re-delivered batch (at-least-once): nothing new → no change signalled.
        assert!(!apply_sync(&client, &resp).unwrap());
        assert_eq!(client.store.timeline("!r:s", 10, None).unwrap().len(), 1);
    }

    #[tokio::test]
    #[serial]
    async fn apply_sync_advances_token_on_empty_batch() {
        let client = client().await;
        let empty = json!({ "next_batch": "7_2", "rooms": { "join": {} } });
        assert!(!apply_sync(&client, &empty).unwrap());
        // Token advances even with no events — the position may have moved.
        assert_eq!(
            client.store.load_sync_token().unwrap().as_deref(),
            Some("7_2")
        );
    }

    #[tokio::test]
    #[serial]
    async fn to_device_welcome_joins_group_and_is_idempotent() {
        use crate::e2ee::E2ee;

        // `bob` is the syncing client (login gave it its own MLS engine). `alice`
        // hosts the group and adds bob from a KeyPackage he publishes.
        let bob = client().await;
        let alice = E2ee::create("@alice:s").unwrap();
        let bob_kp = bob
            .e2ee
            .as_ref()
            .unwrap()
            .key_packages(1)
            .unwrap()
            .remove(0);
        alice.create_group("!enc:s").unwrap();
        let welcome = alice.add_member("!enc:s", &bob_kp).unwrap();

        // A /sync carrying the Welcome as a to-device event.
        let resp = json!({
            "next_batch": "1_1",
            "rooms": { "join": {} },
            "to_device": { "events": [
                { "sender": "@alice:s", "type": "p.mls.welcome",
                  "content": { "welcome": welcome, "room_id": "!enc:s" } }
            ] }
        });

        assert!(!bob.e2ee.as_ref().unwrap().has_group("!enc:s").unwrap());
        assert!(apply_to_device(&bob, &resp), "bob joined the group");
        assert!(bob.e2ee.as_ref().unwrap().has_group("!enc:s").unwrap());

        // Re-delivered Welcome (at-least-once): skipped, no re-join (Gotcha #8).
        assert!(
            !apply_to_device(&bob, &resp),
            "duplicate Welcome is idempotent"
        );
    }

    #[tokio::test]
    #[serial]
    async fn to_device_ignores_non_welcome_and_bad_welcomes() {
        let bob = client().await;
        // A non-Welcome to-device event and a malformed Welcome must not join or
        // wedge the loop — both are skipped, no change.
        let resp = json!({
            "next_batch": "1_1",
            "to_device": { "events": [
                { "sender": "@x:s", "type": "p.other", "content": {} },
                { "sender": "@x:s", "type": "p.mls.welcome",
                  "content": { "welcome": "!!not base64!!", "room_id": "!enc:s" } }
            ] }
        });
        assert!(!apply_to_device(&bob, &resp));
        assert!(!bob.e2ee.as_ref().unwrap().has_group("!enc:s").unwrap());
    }
}
