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
use crate::store::RoomSummary;
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
}
