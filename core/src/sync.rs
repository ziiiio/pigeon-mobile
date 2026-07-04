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
                    // We're online — a good moment to (re)transmit queued sends
                    // (offline-first retry, M2.5). Either can change the store.
                    let flushed = self.flush_pending().await?;
                    backoff = BACKOFF_START;
                    observer.on_status(true);
                    if applied || flushed {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
}
