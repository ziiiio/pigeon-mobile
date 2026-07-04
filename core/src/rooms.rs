//! Rooms: the FFI surface over the store's room list and the create/join flows
//! (M2.3), plus timeline reads (M2.4) and plaintext send (M2.5).
//!
//! Reads (`list_rooms`, `timeline`) are synchronous — they come straight from
//! the local store (offline-first; no network on the read path). Writes
//! (`create_room`, `join_room`, `invite`, `send_message`) are async HTTP calls;
//! their *effects* land in the store on the next `/sync` and reach the UI through
//! [`crate::sync::SyncObserver`], so the view-models re-read rather than trusting
//! a write's return value for state.

use crate::session::PigeonClient;
use crate::store::{RoomSummary, SendState, StoredEvent};
use crate::CoreError;

/// A room in the room list — folded current state from the store. The FFI record
/// the UI renders (M2.3). Mirrors [`RoomSummary`]; kept as its own type so the
/// FFI surface can evolve (unread counts, last-message preview) without touching
/// the store's internal shape.
#[derive(Debug, Clone, uniffi::Record)]
pub struct Room {
    /// The room id (`!…:server`). Stable handle for opening the timeline.
    pub room_id: String,
    /// Display name, folded from `p.room.name` — `None` if the room is unnamed
    /// (the UI falls back to members / the id).
    pub name: Option<String>,
    /// Topic, folded from `p.room.topic`.
    pub topic: Option<String>,
    /// True once the room is encrypted (E2EE — transparent to the UI in M3).
    pub encrypted: bool,
    /// Most recent activity (`origin_server_ts`) for list ordering. Display
    /// clock — approximate, never a correctness input.
    pub last_activity_ts: i64,
}

impl From<RoomSummary> for Room {
    fn from(r: RoomSummary) -> Self {
        Room {
            room_id: r.room_id,
            name: r.name,
            topic: r.topic,
            encrypted: r.encrypted,
            last_activity_ts: r.last_activity_ts,
        }
    }
}

/// A timeline entry the UI renders (M2.4). Exactly one of `body` (a text
/// message) or `system_text` (a pre-rendered state/membership line) is set for a
/// displayable event; both are `None` for events the UI hides (e.g. power
/// levels). Rendering the system line **in the core** keeps the native side free
/// of protocol knowledge (CLAUDE.md Gotcha #9) — Kotlin/Swift never parse event
/// content.
#[derive(Debug, Clone, uniffi::Record)]
pub struct TimelineEvent {
    /// Server content-hash id — stable, unique; the UI's list key + dedup.
    pub event_id: String,
    /// Full sender id (`@user:server`). The UI compares it to the session user
    /// to align its own messages; it is not a secret.
    pub sender: String,
    /// Sender wall-clock millis — display only (never an ordering key).
    pub origin_server_ts: i64,
    /// Opaque load-older cursor (the DAG depth): pass the oldest loaded event's
    /// value back as `before` to page backwards. Treat as opaque in the UI.
    pub cursor: i64,
    /// A text message body (`content.body`), for `p.room.message`.
    pub body: Option<String>,
    /// A pre-rendered system line for a state/membership event, else `None`.
    pub system_text: Option<String>,
    /// A local echo whose send is still in flight / queued (offline). The UI
    /// dims it and shows a "sending" hint (M2.5).
    pub pending: bool,
    /// A local echo whose send failed terminally. The UI marks it "not sent".
    pub failed: bool,
}

impl From<StoredEvent> for TimelineEvent {
    fn from(ev: StoredEvent) -> Self {
        let body = if ev.event_type == "p.room.message" {
            ev.content
                .get("body")
                .and_then(|v| v.as_str())
                .map(str::to_owned)
        } else {
            None
        };
        // Only render a system line when there's no message body to show.
        let system_text = if body.is_none() {
            render_system(&ev)
        } else {
            None
        };
        TimelineEvent {
            event_id: ev.event_id,
            sender: ev.sender,
            origin_server_ts: ev.origin_server_ts,
            cursor: ev.depth,
            body,
            system_text,
            pending: ev.send_state == SendState::Sending,
            failed: ev.send_state == SendState::Failed,
        }
    }
}

/// Render a state/membership event as a human system line, or `None` for the
/// event types the timeline doesn't surface (power levels, join rules, unknown).
/// The rendering lives here so no protocol parsing leaks into native code.
fn render_system(ev: &StoredEvent) -> Option<String> {
    let str_field = |key: &str| ev.content.get(key).and_then(|v| v.as_str());
    match ev.event_type.as_str() {
        "p.room.member" => {
            let membership = str_field("membership").unwrap_or("");
            // state_key is the target of the membership; fall back to the sender.
            let target = ev.state_key.as_deref().unwrap_or(&ev.sender);
            Some(match membership {
                "join" => format!("{target} joined"),
                "leave" => format!("{target} left"),
                "invite" => format!("{} invited {}", ev.sender, target),
                "ban" => format!("{} banned {}", ev.sender, target),
                "knock" => format!("{target} requested to join"),
                other => format!("{target}: {other}"),
            })
        }
        "p.room.name" => Some(match str_field("name") {
            Some(name) => format!("Room name set to \u{201c}{name}\u{201d}"),
            None => "Room name removed".to_owned(),
        }),
        "p.room.topic" => Some("Topic updated".to_owned()),
        "p.room.encryption" => Some("Encryption enabled".to_owned()),
        "p.room.create" => Some("Room created".to_owned()),
        _ => None,
    }
}

// Synchronous reads straight from the store (offline-first).
#[uniffi::export]
impl PigeonClient {
    /// The room list from the local store, most-recent-activity first. A pure
    /// local read — no network. The sync loop keeps it current; the UI re-reads
    /// on [`crate::sync::SyncObserver::on_change`].
    pub fn list_rooms(&self) -> Result<Vec<Room>, CoreError> {
        Ok(self
            .store
            .list_rooms()?
            .into_iter()
            .map(Room::from)
            .collect())
    }

    /// A timeline page for `room_id`, oldest-first — a pure local store read.
    /// Returns the newest `limit` events when `before` is `None`; pass the oldest
    /// loaded event's `cursor` as `before` to page backwards (scroll-to-load-
    /// older). The sync loop keeps the tail current; the UI re-reads on change.
    pub fn timeline(
        &self,
        room_id: String,
        limit: u32,
        before: Option<i64>,
    ) -> Result<Vec<TimelineEvent>, CoreError> {
        Ok(self
            .store
            .timeline(&room_id, limit, before)?
            .into_iter()
            .map(TimelineEvent::from)
            .collect())
    }
}

// Async writes to the homeserver. Their effects surface via the sync loop.
#[uniffi::export(async_runtime = "tokio")]
impl PigeonClient {
    /// Create a room and return its id. Optional `name`/`topic`. Plaintext for
    /// M2 (encryption is an M3 concern — created unencrypted here). The room
    /// appears in `list_rooms` once the running sync loop folds its state in.
    pub async fn create_room(
        &self,
        name: Option<String>,
        topic: Option<String>,
    ) -> Result<String, CoreError> {
        Ok(self
            .api
            .create_room(name.as_deref(), topic.as_deref(), false)
            .await?)
    }

    /// Join a room by id. The membership + timeline arrive on the next sync.
    pub async fn join_room(&self, room_id: String) -> Result<(), CoreError> {
        self.api.join_room(&room_id).await?;
        Ok(())
    }

    /// Backfill recent history for a room from the server and persist it, then
    /// return how many events were newly stored. Used when a room is opened so
    /// the timeline has content even before the sync loop has covered it.
    ///
    /// The server's messages endpoint returns only the latest `limit` events
    /// (no older-than cursor yet — CLAUDE.md M2 note), so this tops up recent
    /// history rather than paging arbitrarily far back; deeper scroll-back pages
    /// through what the store already holds (`timeline`'s `before`). Idempotent —
    /// events already seen are ignored (dedup on `event_id`).
    pub async fn fetch_messages(&self, room_id: String, limit: u32) -> Result<u32, CoreError> {
        let resp = self.api.messages(&room_id, limit).await?;
        let chunk: Vec<serde_json::Value> = resp["chunk"].as_array().cloned().unwrap_or_default();
        Ok(self.store.apply_events(&chunk)? as u32)
    }

    /// Send a plaintext message (M2.5). Offline-first with **local echo**: the
    /// message is written to the store immediately (so the timeline shows it as
    /// "sending") and queued, then a flush attempts delivery. This returns once
    /// the queued echo exists — it does not fail if the network is down; the
    /// message stays queued and the sync loop retries it (see [`flush_pending`]).
    /// The caller re-reads the timeline to see the echo and its state.
    pub async fn send_message(&self, room_id: String, body: String) -> Result<(), CoreError> {
        self.store
            .queue_send(&room_id, &self.session().user_id, &body)?;
        // Best-effort immediate delivery; leftover/queued sends are retried by
        // the sync loop. A transport failure here is not an error to the caller.
        self.flush_pending().await?;
        Ok(())
    }

    /// Attempt to deliver every queued outbound message, oldest first, and return
    /// whether the store changed (an echo was confirmed or marked failed) so the
    /// caller can refresh. Called after `send_message` and once per sync cycle
    /// (offline-first retry). A transport error stops the pass — the remaining
    /// sends stay queued for the next attempt; a server rejection fails that one
    /// send terminally and moves on.
    pub async fn flush_pending(&self) -> Result<bool, CoreError> {
        let mut changed = false;
        for send in self.store.pending_sends()? {
            let content = serde_json::json!({ "body": send.body, "msgtype": "p.text" });
            match self
                .api
                .send_message(&send.room_id, &send.txn_id, &content)
                .await
            {
                Ok(event_id) => {
                    self.store.resolve_send(&send.txn_id, &event_id)?;
                    changed = true;
                }
                // Offline: leave this and the rest queued; retry next cycle.
                Err(crate::api::ApiError::Network { .. }) => break,
                // The server rejected it (e.g. no longer joined): fail it so the
                // user sees it didn't send, and continue with the others.
                Err(_) => {
                    self.store.fail_send(&send.txn_id)?;
                    changed = true;
                }
            }
        }
        Ok(changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn stored(
        event_type: &str,
        state_key: Option<&str>,
        content: serde_json::Value,
    ) -> StoredEvent {
        StoredEvent {
            event_id: "$e".to_owned(),
            room_id: "!r:s".to_owned(),
            sender: "@alice:s".to_owned(),
            event_type: event_type.to_owned(),
            state_key: state_key.map(str::to_owned),
            origin_server_ts: 100,
            depth: 3,
            content: content.clone(),
            payload: content,
            send_state: SendState::Confirmed,
        }
    }

    #[test]
    fn message_maps_to_body() {
        let ev = TimelineEvent::from(stored(
            "p.room.message",
            None,
            json!({ "body": "hello", "msgtype": "p.text" }),
        ));
        assert_eq!(ev.body.as_deref(), Some("hello"));
        assert_eq!(ev.system_text, None);
        assert_eq!(ev.cursor, 3); // depth is the load-older cursor
    }

    #[test]
    fn membership_renders_a_system_line() {
        let join = TimelineEvent::from(stored(
            "p.room.member",
            Some("@bob:s"),
            json!({ "membership": "join" }),
        ));
        assert_eq!(join.body, None);
        assert_eq!(join.system_text.as_deref(), Some("@bob:s joined"));

        let invite = TimelineEvent::from(stored(
            "p.room.member",
            Some("@bob:s"),
            json!({ "membership": "invite" }),
        ));
        assert_eq!(
            invite.system_text.as_deref(),
            Some("@alice:s invited @bob:s")
        );
    }

    #[test]
    fn name_and_encryption_render_system_lines() {
        let name = TimelineEvent::from(stored(
            "p.room.name",
            Some(""),
            json!({ "name": "General" }),
        ));
        assert_eq!(
            name.system_text.as_deref(),
            Some("Room name set to \u{201c}General\u{201d}")
        );

        let enc = TimelineEvent::from(stored("p.room.encryption", Some(""), json!({})));
        assert_eq!(enc.system_text.as_deref(), Some("Encryption enabled"));
    }

    #[test]
    fn hidden_state_events_render_nothing() {
        // power_levels / join_rules aren't surfaced — the UI skips None/None.
        let ev = TimelineEvent::from(stored("p.room.power_levels", Some(""), json!({})));
        assert_eq!(ev.body, None);
        assert_eq!(ev.system_text, None);
    }
}
