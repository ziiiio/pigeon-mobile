//! The local persistence store (M2.1) — offline-first SQLite.
//!
//! This is the client's on-device source of truth. Reads come from here; the
//! sync loop (M2.2) reconciles it against the server. It holds four things, the
//! scope of M2.1:
//!
//! - the **timeline event log** — an append-only table keyed by the server's
//!   content-hash `event_id`, so re-applying a `/sync` batch is idempotent
//!   (sync is at-least-once by design — CLAUDE.md Gotcha #8);
//! - **current room state**, folded from the state events in that log
//!   (`p.room.name`/`topic`/`encryption`/`member`/…), last-writer-wins by DAG
//!   `depth`. Rooms have no wire object — name/topic/membership are all state
//!   events we fold ourselves;
//! - **membership**, which is just the current `p.room.member` state per target
//!   user (invites/leaves land here in M2.6);
//! - the **sync token** — the opaque composite `next_batch`, stored verbatim
//!   (CLAUDE.md Gotcha #5: never parse or synthesise one).
//!
//! ## Design notes
//!
//! - **Append-friendly.** `events` is insert-only (`INSERT OR IGNORE` on the
//!   `event_id` primary key). Derived tables (`room_state`) are folded on write;
//!   nothing is mutated destructively, so a schema addition is a new table/column
//!   plus a migration, never a rewrite.
//! - **Ordering.** Timeline order mirrors the server: `depth ASC, event_id ASC`
//!   (the DAG position; `event.rs` in the server repo). `origin_server_ts` is
//!   sender wall-clock — untrusted, display-only, never an ordering key.
//! - **Sync, not async.** rusqlite is blocking, but these ops are fast local
//!   I/O; they run synchronously off the FFI's async path (a `std::sync::Mutex`
//!   guards the one connection and is never held across an `.await`). A future
//!   heavy scan can offload via `tokio::task::spawn_blocking`.
//! - **No secrets here.** Keys/tokens live in the platform keystore
//!   ([`crate::session`]), never in this DB in clear (CLAUDE.md Gotcha #1).
//!
//! The store is a plain Rust module (no UniFFI surface of its own yet); the sync
//! loop and the room/timeline FFI records are built on top of it in M2.2–M2.4.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{Connection, OptionalExtension};
use serde_json::Value;

use crate::CoreError;

/// The current schema version. Bumped when a migration is added; see [`migrate`].
const SCHEMA_VERSION: i64 = 3;

/// A failure from the local store: either the SQLite layer or a wire event that
/// wasn't the shape the protocol promised. Surfaced across the FFI as
/// [`CoreError::Storage`] (a corrupt/unreadable local DB is a storage fault).
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// The SQLite layer failed (open, migrate, query, disk).
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    /// A `/sync` event was missing a field the store requires to index it.
    #[error("malformed event: {reason}")]
    Malformed { reason: String },
}

impl From<StoreError> for CoreError {
    fn from(err: StoreError) -> Self {
        CoreError::Storage {
            reason: err.to_string(),
        }
    }
}

/// A room member's current membership, read from the latest `p.room.member`
/// state event for a target user. Unknown wire values are preserved verbatim
/// ([`Membership::Other`]) so a newer server can't make the client panic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Membership {
    Join,
    Invite,
    Leave,
    Ban,
    Knock,
    Other(String),
}

impl Membership {
    /// Map a wire `content.membership` string to a typed value.
    pub fn from_wire(s: &str) -> Self {
        match s {
            "join" => Self::Join,
            "invite" => Self::Invite,
            "leave" => Self::Leave,
            "ban" => Self::Ban,
            "knock" => Self::Knock,
            other => Self::Other(other.to_owned()),
        }
    }
}

/// A stored timeline event, parsed from the wire JSON into the fields the store
/// indexes on. The full event is retained verbatim in [`StoredEvent::payload`]
/// so nothing is lost (signatures, DAG edges, unknown fields all survive a
/// round-trip through the DB).
#[derive(Debug, Clone)]
pub struct StoredEvent {
    /// Server content-hash id (`$…`) — the immutable, globally-unique dedup key.
    pub event_id: String,
    pub room_id: String,
    pub sender: String,
    /// The `type` field (`p.room.message`, `p.room.member`, …).
    pub event_type: String,
    /// Present ⇒ this is a state event; the key is the target (a user id for
    /// membership, `""` for room-scoped state like name/topic).
    pub state_key: Option<String>,
    /// Sender wall-clock millis. **Display-only** — never an ordering key.
    pub origin_server_ts: i64,
    /// DAG depth — the timeline ordering authority.
    pub depth: i64,
    /// The event `content` (e.g. `{ "body", "msgtype" }` for a message).
    pub content: Value,
    /// The whole event JSON, retained losslessly.
    pub payload: Value,
    /// Local send state (M2.5): [`SendState::Confirmed`] for a real event from
    /// the server; [`SendState::Sending`]/[`SendState::Failed`] for a local echo
    /// still in the outbound queue. Confirmed for anything parsed from the wire.
    pub send_state: SendState,
    /// Decrypted plaintext body of a `p.room.encrypted` event, cached on first
    /// decrypt (M3.5). `Some` once decrypted (the ratchet advances and is
    /// persisted, so we never re-decrypt — Gotcha #3); `None` for plaintext
    /// events, or an encrypted event not yet / never decrypted.
    pub decrypted: Option<String>,
    /// A `p.room.encrypted` event whose decryption failed terminally (M3.5) —
    /// the UI shows an "unable to decrypt" placeholder. Distinct from "not yet
    /// attempted" (which stays hidden until the sync loop decrypts it).
    pub decrypt_failed: bool,
}

/// The delivery state of a timeline event's *own* send (M2.5). Only a local echo
/// is ever `Sending`/`Failed`; everything from `/sync` is `Confirmed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendState {
    /// A real, server-acknowledged event (or any event received via sync).
    Confirmed,
    /// A local echo whose send is in flight or queued for retry (offline).
    Sending,
    /// A local echo whose send failed terminally (e.g. not permitted).
    Failed,
}

impl SendState {
    /// Map the `events.local` column (0/1/2) to a state.
    fn from_local(local: i64) -> Self {
        match local {
            1 => SendState::Sending,
            2 => SendState::Failed,
            _ => SendState::Confirmed,
        }
    }
}

impl StoredEvent {
    /// Parse a `/sync` timeline event into a [`StoredEvent`]. Pure — no I/O, so
    /// it is unit-tested without a database or runtime. A missing required field
    /// is a protocol mismatch ([`StoreError::Malformed`]), not a DB fault.
    pub fn from_wire(event: &Value) -> Result<Self, StoreError> {
        let field = |key: &str| -> Result<String, StoreError> {
            event[key]
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| StoreError::Malformed {
                    reason: format!("event missing string field `{key}`"),
                })
        };
        // origin_server_ts and depth are `u64` on the wire; they fit comfortably
        // in the i64 SQLite stores as INTEGER (well under 2^63 for real values).
        let int = |key: &str| -> Result<i64, StoreError> {
            event[key].as_i64().ok_or_else(|| StoreError::Malformed {
                reason: format!("event missing integer field `{key}`"),
            })
        };
        Ok(StoredEvent {
            event_id: field("event_id")?,
            room_id: field("room_id")?,
            sender: field("sender")?,
            event_type: field("type")?,
            // `state_key` is omitted on non-state events (server `skip_serializing_if`).
            state_key: event["state_key"].as_str().map(str::to_owned),
            origin_server_ts: int("origin_server_ts")?,
            depth: int("depth")?,
            content: event.get("content").cloned().unwrap_or(Value::Null),
            payload: event.clone(),
            // Anything parsed from the wire is a confirmed, server-side event.
            send_state: SendState::Confirmed,
            // Decryption fields come from the DB row, not the wire; a freshly
            // parsed wire event is not-yet-decrypted.
            decrypted: None,
            decrypt_failed: false,
        })
    }
}

/// A queued outbound message awaiting (re)send (M2.5). Held in `pending_sends`
/// until the server acks it; the sync loop retries these when online.
#[derive(Debug, Clone)]
pub struct PendingSend {
    /// Client transaction id — identifies the attempt (the server ignores it, so
    /// it's our own dedup key, not the server's).
    pub txn_id: String,
    pub room_id: String,
    pub body: String,
}

/// A `p.room.encrypted` event awaiting decryption (M3.5). The sync loop decrypts
/// these in DAG order (the MLS ratchet is order-sensitive — Gotcha #3) and caches
/// the plaintext, so each is decrypted exactly once.
#[derive(Debug, Clone)]
pub struct PendingDecrypt {
    pub event_id: String,
    pub room_id: String,
    /// The base64 MLS ciphertext (`content.ciphertext`).
    pub ciphertext_b64: String,
}

/// A room as it appears in the room list — current-state fields folded from the
/// event log. Built in M2.1; the FFI-visible record and live updates are M2.3.
#[derive(Debug, Clone, PartialEq)]
pub struct RoomSummary {
    pub room_id: String,
    /// From the latest `p.room.name` (`content.name`), if any.
    pub name: Option<String>,
    /// From the latest `p.room.topic` (`content.topic`), if any.
    pub topic: Option<String>,
    /// True once a `p.room.encryption` state event has been seen (E2EE marker).
    pub encrypted: bool,
    /// Most recent `origin_server_ts` in the room — for list ordering. Display
    /// clock, so approximate, but adequate to sort "most recent first".
    pub last_activity_ts: i64,
}

/// The local SQLite store. One per session; cheap to clone the handle by sharing
/// the `Arc` the session holds. The single connection is guarded by a mutex —
/// ops are short and synchronous, so it is never held across an `.await`.
pub struct Store {
    conn: Mutex<Connection>,
}

impl Store {
    /// Open (creating if absent) the store at `path` and bring the schema up to
    /// date. Used on device with the host's app-private data dir.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    /// Open an in-memory store — for host unit tests (no file, no device).
    pub fn open_in_memory() -> Result<Self, StoreError> {
        let conn = Connection::open_in_memory()?;
        Self::init(conn)
    }

    fn init(conn: Connection) -> Result<Self, StoreError> {
        // WAL for durability + reader/writer concurrency; enforce FKs so a
        // dangling state→event reference can't be written.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", true)?;
        migrate(&conn)?;
        Ok(Store {
            conn: Mutex::new(conn),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().expect("store connection mutex poisoned")
    }

    // --- Sync token (opaque composite `next_batch`) --------------------------

    /// Persist the sync token verbatim (CLAUDE.md Gotcha #5 — never parse it).
    /// Single-row table; replaces any previous value.
    pub fn save_sync_token(&self, token: &str) -> Result<(), StoreError> {
        self.lock().execute(
            "INSERT INTO sync_token (id, token) VALUES (0, ?1)
             ON CONFLICT(id) DO UPDATE SET token = excluded.token",
            [token],
        )?;
        Ok(())
    }

    /// Load the persisted sync token, or `None` before the first sync completes.
    pub fn load_sync_token(&self) -> Result<Option<String>, StoreError> {
        let token = self
            .lock()
            .query_row("SELECT token FROM sync_token WHERE id = 0", [], |r| {
                r.get::<_, String>(0)
            })
            .optional()?;
        Ok(token)
    }

    // --- Writes: append events + fold current state --------------------------

    /// Apply a batch of `/sync` timeline events: append each to the log
    /// (idempotent on `event_id`) and fold any state event into current state.
    /// Runs in one transaction, so a batch lands atomically. Returns the count of
    /// events newly inserted (already-seen events are counted as 0 — re-sync is a
    /// no-op, Gotcha #8).
    pub fn apply_events(&self, events: &[Value]) -> Result<usize, StoreError> {
        // Parse first (pure, fail-fast) so a malformed event aborts the batch
        // before any partial write.
        let parsed: Vec<StoredEvent> = events
            .iter()
            .map(StoredEvent::from_wire)
            .collect::<Result<_, _>>()?;

        let mut guard = self.lock();
        let tx = guard.transaction()?;
        let mut inserted = 0usize;
        for ev in &parsed {
            inserted += insert_event(&tx, ev)?;
            if ev.state_key.is_some() {
                fold_state(&tx, ev)?;
            }
        }
        tx.commit()?;
        Ok(inserted)
    }

    // --- Reads ----------------------------------------------------------------

    /// A timeline page for a room, oldest-first (`depth ASC, event_id ASC`,
    /// mirroring the server). Returns the newest `limit` events when `before` is
    /// `None`; pass a `depth` as `before` to page backwards (backfill, M2.4).
    /// Includes every event type — state and membership render inline in the
    /// timeline (as the reference CLI does), so the UI decides what to show.
    pub fn timeline(
        &self,
        room_id: &str,
        limit: u32,
        before: Option<i64>,
    ) -> Result<Vec<StoredEvent>, StoreError> {
        let guard = self.lock();
        // Select the newest `limit` (optionally strictly older than `before`)
        // descending, then reverse to oldest-first for display. `local` carries
        // the send state of a local echo (0 for confirmed/synced events);
        // `decrypted`/`decrypt_state` carry the cached plaintext of an encrypted
        // event (M3.5).
        let sql = "SELECT payload, local, decrypted, decrypt_state FROM events
                   WHERE room_id = ?1 AND (?2 IS NULL OR depth < ?2)
                   ORDER BY depth DESC, event_id DESC
                   LIMIT ?3";
        let mut stmt = guard.prepare(sql)?;
        let rows = stmt.query_map(rusqlite::params![room_id, before, limit], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?;
        let mut events = Vec::new();
        for row in rows {
            let (payload, local, decrypted, decrypt_state) = row?;
            events.push(parse_stored_payload(
                &payload,
                local,
                decrypted,
                decrypt_state,
            )?);
        }
        events.reverse();
        Ok(events)
    }

    /// The room list, one entry per known room, most-recent-activity first.
    /// Name/topic/encryption are folded current state (CLAUDE.md: rooms have no
    /// wire object). The server only syncs rooms you have joined, so every room
    /// here is one you're in.
    pub fn list_rooms(&self) -> Result<Vec<RoomSummary>, StoreError> {
        let guard = self.lock();
        // One row per room that has any state, with name/topic pulled from the
        // current-state event's content and `encrypted` from the presence of a
        // `p.room.encryption` state event.
        let sql = "
            SELECT r.room_id,
                   (SELECT json_extract(e.payload, '$.content.name')
                      FROM room_state s JOIN events e ON e.event_id = s.event_id
                     WHERE s.room_id = r.room_id AND s.type = 'p.room.name' AND s.state_key = ''),
                   (SELECT json_extract(e.payload, '$.content.topic')
                      FROM room_state s JOIN events e ON e.event_id = s.event_id
                     WHERE s.room_id = r.room_id AND s.type = 'p.room.topic' AND s.state_key = ''),
                   EXISTS(SELECT 1 FROM room_state s
                           WHERE s.room_id = r.room_id AND s.type = 'p.room.encryption'),
                   COALESCE((SELECT MAX(origin_server_ts) FROM events e WHERE e.room_id = r.room_id), 0)
              FROM (SELECT DISTINCT room_id FROM room_state) r
             ORDER BY 5 DESC, r.room_id ASC";
        let mut stmt = guard.prepare(sql)?;
        let rows = stmt.query_map([], |r| {
            Ok(RoomSummary {
                room_id: r.get(0)?,
                name: r.get(1)?,
                topic: r.get(2)?,
                encrypted: r.get::<_, i64>(3)? != 0,
                last_activity_ts: r.get(4)?,
            })
        })?;
        let mut rooms = Vec::new();
        for row in rows {
            rooms.push(row?);
        }
        Ok(rooms)
    }

    /// The current membership of `user_id` in `room_id`, from the latest
    /// `p.room.member` state event for that target — `None` if the user has no
    /// membership event in the room.
    pub fn membership(
        &self,
        room_id: &str,
        user_id: &str,
    ) -> Result<Option<Membership>, StoreError> {
        let ev = self.current_state(room_id, "p.room.member", user_id)?;
        Ok(ev.and_then(|e| {
            e.content
                .get("membership")
                .and_then(Value::as_str)
                .map(Membership::from_wire)
        }))
    }

    /// The current-state event for `(type, state_key)` in a room, or `None`.
    /// The generic current-state accessor the folded state answers from.
    pub fn current_state(
        &self,
        room_id: &str,
        event_type: &str,
        state_key: &str,
    ) -> Result<Option<StoredEvent>, StoreError> {
        let guard = self.lock();
        let payload = guard
            .query_row(
                "SELECT e.payload FROM room_state s JOIN events e ON e.event_id = s.event_id
                 WHERE s.room_id = ?1 AND s.type = ?2 AND s.state_key = ?3",
                rusqlite::params![room_id, event_type, state_key],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        match payload {
            // A state event is always a confirmed, server-side event (local = 0)
            // and is never encrypted (no decrypted plaintext to carry).
            Some(p) => Ok(Some(parse_stored_payload(&p, 0, None, 0)?)),
            None => Ok(None),
        }
    }

    /// Whether `room_id` is flagged end-to-end encrypted — a `p.room.encryption`
    /// state event has been folded into current state (mirrors the `encrypted`
    /// column of [`list_rooms`](Store::list_rooms)). Used on the send path to
    /// refuse a plaintext downgrade when the room is encrypted but we hold no MLS
    /// group yet (Gotcha #1 — never leak plaintext into an E2EE room).
    pub fn is_room_encrypted(&self, room_id: &str) -> Result<bool, StoreError> {
        let guard = self.lock();
        let encrypted: i64 = guard.query_row(
            "SELECT EXISTS(SELECT 1 FROM room_state
                            WHERE room_id = ?1 AND type = 'p.room.encryption')",
            rusqlite::params![room_id],
            |r| r.get(0),
        )?;
        Ok(encrypted != 0)
    }

    // --- Outbound send queue + local echo (M2.5) -----------------------------

    /// Queue an outbound message: write a provisional local echo into the
    /// timeline (so the UI shows it immediately) and a `pending_sends` row for
    /// the sender to (re)transmit. Returns the transaction id identifying the
    /// attempt. The echo sits just after the newest known event (depth = max+1)
    /// and is marked `Sending` until the server acks it.
    pub fn queue_send(
        &self,
        room_id: &str,
        sender: &str,
        body: &str,
    ) -> Result<String, StoreError> {
        let (txn_id, event_id) = next_send_ids();
        let ts = now_millis();

        let mut guard = self.lock();
        let tx = guard.transaction()?;
        // Place the echo after the newest event in the room.
        let depth: i64 = tx.query_row(
            "SELECT COALESCE(MAX(depth), 0) + 1 FROM events WHERE room_id = ?1",
            [room_id],
            |r| r.get(0),
        )?;
        // Build a full event payload (including `depth`) so a timeline read can
        // round-trip it back through `StoredEvent::from_wire` like any event.
        let payload = serde_json::json!({
            "event_id": event_id,
            "room_id": room_id,
            "sender": sender,
            "type": "p.room.message",
            "origin_server_ts": ts,
            "depth": depth,
            "content": { "body": body, "msgtype": "p.text" },
        });
        tx.execute(
            "INSERT INTO events
               (event_id, room_id, sender, type, state_key, origin_server_ts, depth, payload, local)
             VALUES (?1, ?2, ?3, 'p.room.message', NULL, ?4, ?5, ?6, 1)",
            rusqlite::params![event_id, room_id, sender, ts, depth, payload.to_string()],
        )?;
        tx.execute(
            "INSERT INTO pending_sends (txn_id, room_id, body, event_id, created_ts)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![txn_id, room_id, body, event_id, ts],
        )?;
        tx.commit()?;
        Ok(txn_id)
    }

    /// The outbound queue, oldest first — what the sender must (re)transmit.
    pub fn pending_sends(&self) -> Result<Vec<PendingSend>, StoreError> {
        let guard = self.lock();
        let mut stmt = guard.prepare(
            "SELECT txn_id, room_id, body FROM pending_sends ORDER BY created_ts ASC, txn_id ASC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(PendingSend {
                txn_id: r.get(0)?,
                room_id: r.get(1)?,
                body: r.get(2)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Mark a queued send acknowledged by the server under `real_event_id`.
    /// Promotes the provisional echo to the real id (so the authoritative event
    /// arriving via `/sync` dedups against it — no duplicate, no flicker), or
    /// drops the echo if sync already delivered that event. Clears the queue row.
    pub fn resolve_send(&self, txn_id: &str, real_event_id: &str) -> Result<(), StoreError> {
        let mut guard = self.lock();
        let tx = guard.transaction()?;
        let provisional: Option<String> = tx
            .query_row(
                "SELECT event_id FROM pending_sends WHERE txn_id = ?1",
                [txn_id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(provisional) = provisional {
            let real_exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM events WHERE event_id = ?1)",
                [real_event_id],
                |r| r.get::<_, i64>(0),
            )? != 0;
            if real_exists {
                // Sync raced ahead of the ack: drop the now-redundant echo.
                tx.execute("DELETE FROM events WHERE event_id = ?1", [&provisional])?;
            } else {
                // Promote the echo to the confirmed event in place — rewrite both
                // the column and the payload's own `event_id` (a timeline read
                // rebuilds the event from the payload), and clear the local flag.
                tx.execute(
                    "UPDATE events
                        SET event_id = ?1,
                            local = 0,
                            payload = json_set(payload, '$.event_id', ?1)
                      WHERE event_id = ?2",
                    rusqlite::params![real_event_id, provisional],
                )?;
            }
        }
        tx.execute("DELETE FROM pending_sends WHERE txn_id = ?1", [txn_id])?;
        tx.commit()?;
        Ok(())
    }

    /// Mark a queued send terminally failed: flag its echo `Failed` and remove it
    /// from the retry queue. The user sees the message didn't send.
    pub fn fail_send(&self, txn_id: &str) -> Result<(), StoreError> {
        let mut guard = self.lock();
        let tx = guard.transaction()?;
        if let Some(event_id) = tx
            .query_row(
                "SELECT event_id FROM pending_sends WHERE txn_id = ?1",
                [txn_id],
                |r| r.get::<_, String>(0),
            )
            .optional()?
        {
            tx.execute(
                "UPDATE events SET local = 2 WHERE event_id = ?1",
                [&event_id],
            )?;
        }
        tx.execute("DELETE FROM pending_sends WHERE txn_id = ?1", [txn_id])?;
        tx.commit()?;
        Ok(())
    }

    // --- Encrypted-message plaintext cache (M3.5) ----------------------------

    /// The `p.room.encrypted` events not yet decrypted, in DAG order (`depth ASC,
    /// event_id ASC`). The MLS ratchet advances per decrypt and is order-sensitive
    /// (Gotcha #3), so the caller must decrypt these in exactly this order and
    /// exactly once. Events with no `content.ciphertext` are skipped (malformed —
    /// they'd never decrypt).
    pub fn pending_decrypts(&self) -> Result<Vec<PendingDecrypt>, StoreError> {
        let guard = self.lock();
        let mut stmt = guard.prepare(
            "SELECT event_id, room_id, json_extract(payload, '$.content.ciphertext')
             FROM events
             WHERE type = 'p.room.encrypted' AND decrypt_state = 0
             ORDER BY depth ASC, event_id ASC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (event_id, room_id, ciphertext) = row?;
            if let Some(ciphertext_b64) = ciphertext {
                out.push(PendingDecrypt {
                    event_id,
                    room_id,
                    ciphertext_b64,
                });
            }
        }
        Ok(out)
    }

    /// Cache the decrypted plaintext of an encrypted event (M3.5) and mark it
    /// decrypted, so it renders as a normal message and is never re-decrypted
    /// (the ratchet has already advanced — Gotcha #3).
    pub fn set_decrypted(&self, event_id: &str, plaintext: &str) -> Result<(), StoreError> {
        self.lock().execute(
            "UPDATE events SET decrypted = ?2, decrypt_state = ?3 WHERE event_id = ?1",
            rusqlite::params![event_id, plaintext, DECRYPT_OK],
        )?;
        Ok(())
    }

    /// Mark an encrypted event as terminally undecryptable (M3.5) — the ratchet
    /// couldn't produce the plaintext (wrong epoch / tampered / not a member). The
    /// UI shows an "unable to decrypt" placeholder; we won't retry.
    pub fn set_decrypt_failed(&self, event_id: &str) -> Result<(), StoreError> {
        self.lock().execute(
            "UPDATE events SET decrypt_state = ?2 WHERE event_id = ?1",
            rusqlite::params![event_id, DECRYPT_FAILED],
        )?;
        Ok(())
    }
}

/// Append one event to the log; idempotent on `event_id`. Returns 1 if the row
/// was new, 0 if it was already present (re-sync).
fn insert_event(conn: &Connection, ev: &StoredEvent) -> Result<usize, StoreError> {
    let changed = conn.execute(
        "INSERT OR IGNORE INTO events
           (event_id, room_id, sender, type, state_key, origin_server_ts, depth, payload)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![
            ev.event_id,
            ev.room_id,
            ev.sender,
            ev.event_type,
            ev.state_key,
            ev.origin_server_ts,
            ev.depth,
            ev.payload.to_string(),
        ],
    )?;
    Ok(changed)
}

/// Fold a state event into current state: it becomes the current `(room, type,
/// state_key)` entry only if its `depth` is at least the incumbent's, so later
/// state wins and re-applying the same event is a no-op (last-writer-wins by DAG
/// depth). Assumes the event row already exists (inserted just before).
fn fold_state(conn: &Connection, ev: &StoredEvent) -> Result<(), StoreError> {
    let state_key = ev
        .state_key
        .as_deref()
        .expect("fold_state called on a non-state event");
    conn.execute(
        "INSERT INTO room_state (room_id, type, state_key, event_id, depth)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(room_id, type, state_key) DO UPDATE SET
           event_id = excluded.event_id, depth = excluded.depth
         WHERE excluded.depth >= room_state.depth",
        rusqlite::params![ev.room_id, ev.event_type, state_key, ev.event_id, ev.depth],
    )?;
    Ok(())
}

/// Rebuild a [`StoredEvent`] from a stored `payload` JSON string plus its
/// `local` send-state flag and its `decrypted`/`decrypt_state` columns (M3.5). A
/// payload we wrote that no longer parses is a storage corruption, not a protocol
/// error.
fn parse_stored_payload(
    payload: &str,
    local: i64,
    decrypted: Option<String>,
    decrypt_state: i64,
) -> Result<StoredEvent, StoreError> {
    let value: Value = serde_json::from_str(payload).map_err(|e| StoreError::Malformed {
        reason: format!("corrupt stored event payload: {e}"),
    })?;
    let mut event = StoredEvent::from_wire(&value)?;
    event.send_state = SendState::from_local(local);
    event.decrypted = decrypted;
    event.decrypt_failed = decrypt_state == DECRYPT_FAILED;
    Ok(event)
}

/// `decrypt_state` column values (M3.5). 0 = not applicable / not yet attempted.
const DECRYPT_OK: i64 = 1;
const DECRYPT_FAILED: i64 = 2;

/// Milliseconds since the Unix epoch — the local echo's display timestamp and
/// the send queue's ordering key. Best-effort (0 if the clock is before the
/// epoch); display-only, never an ordering key across devices.
fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Generate a unique `(txn_id, provisional_event_id)` for an outbound send. The
/// monotonic counter disambiguates sends within the same millisecond. The
/// provisional id is prefixed `$local-` so it can never collide with a server
/// content-hash id (`$<base64>`).
fn next_send_ids() -> (String, String) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let ts = now_millis();
    (format!("m{ts}-{n}"), format!("$local-{ts}-{n}"))
}

/// Bring the schema from whatever version is on disk up to [`SCHEMA_VERSION`].
/// Append-friendly: a future change adds a `version < N` block, never edits an
/// existing one. Tracked via SQLite's `user_version` pragma.
fn migrate(conn: &Connection) -> Result<(), StoreError> {
    let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if version < 1 {
        conn.execute_batch(
            "
            -- The opaque composite sync token (`next_batch`), stored verbatim.
            CREATE TABLE sync_token (
                id    INTEGER PRIMARY KEY CHECK (id = 0),
                token TEXT NOT NULL
            );

            -- Append-only event log. `event_id` is the server content hash, so
            -- INSERT OR IGNORE makes re-syncing a batch idempotent. `payload`
            -- retains the whole event JSON losslessly; the promoted columns are
            -- the ones we filter/order on.
            CREATE TABLE events (
                event_id         TEXT PRIMARY KEY,
                room_id          TEXT NOT NULL,
                sender           TEXT NOT NULL,
                type             TEXT NOT NULL,
                state_key        TEXT,
                origin_server_ts INTEGER NOT NULL,
                depth            INTEGER NOT NULL,
                payload          TEXT NOT NULL
            );
            -- Timeline reads: filter by room, order by DAG depth.
            CREATE INDEX idx_events_room_depth ON events (room_id, depth, event_id);

            -- Current room state, folded from state events (last-writer-wins by
            -- depth). One row per (room, type, state_key); points at the winning
            -- event. Membership is just type = 'p.room.member'.
            CREATE TABLE room_state (
                room_id   TEXT NOT NULL,
                type      TEXT NOT NULL,
                state_key TEXT NOT NULL,
                event_id  TEXT NOT NULL REFERENCES events (event_id),
                depth     INTEGER NOT NULL,
                PRIMARY KEY (room_id, type, state_key)
            );
            ",
        )?;
    }
    if version < 2 {
        // M2.5: outbound send queue + local echo. `events.local` marks a
        // provisional echo's send state (0 confirmed, 1 sending, 2 failed);
        // `pending_sends` is the retry queue keyed by client transaction id.
        conn.execute_batch(
            "
            ALTER TABLE events ADD COLUMN local INTEGER NOT NULL DEFAULT 0;

            CREATE TABLE pending_sends (
                txn_id     TEXT PRIMARY KEY,
                room_id    TEXT NOT NULL,
                body       TEXT NOT NULL,
                event_id   TEXT NOT NULL,
                created_ts INTEGER NOT NULL
            );
            ",
        )?;
    }
    if version < 3 {
        // M3.5: cache decrypted plaintext of encrypted events. `decrypted` holds
        // the plaintext once decrypted; `decrypt_state` is 0 (n/a or pending),
        // 1 (decrypted — plaintext cached), or 2 (terminally undecryptable). The
        // ratchet advances on decrypt and is persisted, so plaintext is cached
        // rather than re-derived (Gotcha #3).
        conn.execute_batch(
            "
            ALTER TABLE events ADD COLUMN decrypted TEXT;
            ALTER TABLE events ADD COLUMN decrypt_state INTEGER NOT NULL DEFAULT 0;
            -- Find undecrypted encrypted events fast (the sync loop's decrypt pass).
            CREATE INDEX idx_events_pending_decrypt
                ON events (type, decrypt_state) WHERE type = 'p.room.encrypted';
            ",
        )?;
    }
    conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A minimal wire event. `state_key: None` ⇒ a non-state (message) event.
    #[allow(clippy::too_many_arguments)] // a test builder mirroring the wire fields
    fn event(
        id: &str,
        room: &str,
        sender: &str,
        depth: i64,
        ts: i64,
        ty: &str,
        state_key: Option<&str>,
        content: Value,
    ) -> Value {
        let mut e = json!({
            "event_id": id,
            "room_id": room,
            "sender": sender,
            "type": ty,
            "origin_server_ts": ts,
            "depth": depth,
            "content": content,
        });
        if let Some(sk) = state_key {
            e["state_key"] = json!(sk);
        }
        e
    }

    fn message(id: &str, room: &str, sender: &str, depth: i64, ts: i64, body: &str) -> Value {
        event(
            id,
            room,
            sender,
            depth,
            ts,
            "p.room.message",
            None,
            json!({ "body": body, "msgtype": "p.text" }),
        )
    }

    #[test]
    fn from_wire_reads_fields_and_detects_state() {
        let msg = StoredEvent::from_wire(&message("$a", "!r:s", "@u:s", 3, 100, "hi")).unwrap();
        assert_eq!(msg.event_id, "$a");
        assert_eq!(msg.event_type, "p.room.message");
        assert_eq!(msg.state_key, None);
        assert_eq!(msg.depth, 3);
        assert_eq!(msg.content["body"], "hi");

        let member = StoredEvent::from_wire(&event(
            "$m",
            "!r:s",
            "@u:s",
            1,
            50,
            "p.room.member",
            Some("@u:s"),
            json!({ "membership": "join" }),
        ))
        .unwrap();
        assert_eq!(member.state_key.as_deref(), Some("@u:s"));
    }

    #[test]
    fn from_wire_rejects_missing_field() {
        // No `sender` → a protocol mismatch, surfaced as Malformed.
        let bad = json!({ "event_id": "$a", "room_id": "!r:s", "type": "p.room.message",
                          "origin_server_ts": 1, "depth": 1, "content": {} });
        match StoredEvent::from_wire(&bad) {
            Err(StoreError::Malformed { reason }) => assert!(reason.contains("sender")),
            other => panic!("expected Malformed, got {other:?}"),
        }
    }

    #[test]
    fn sync_token_round_trips_and_replaces() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(store.load_sync_token().unwrap(), None);

        // Composite token stored verbatim — never parsed.
        store.save_sync_token("42_7").unwrap();
        assert_eq!(store.load_sync_token().unwrap().as_deref(), Some("42_7"));

        // A later token replaces the previous one (single row).
        store.save_sync_token("99_12").unwrap();
        assert_eq!(store.load_sync_token().unwrap().as_deref(), Some("99_12"));
    }

    #[test]
    fn apply_events_appends_and_is_idempotent() {
        let store = Store::open_in_memory().unwrap();
        let batch = vec![
            message("$a", "!r:s", "@u:s", 1, 100, "one"),
            message("$b", "!r:s", "@u:s", 2, 200, "two"),
        ];
        // Both events are new.
        assert_eq!(store.apply_events(&batch).unwrap(), 2);
        // Re-applying the same batch (at-least-once delivery) inserts nothing.
        assert_eq!(store.apply_events(&batch).unwrap(), 0);

        let tl = store.timeline("!r:s", 10, None).unwrap();
        assert_eq!(tl.len(), 2);
        assert_eq!(tl[0].event_id, "$a");
        assert_eq!(tl[1].event_id, "$b");
    }

    #[test]
    fn timeline_orders_by_depth_and_paginates() {
        let store = Store::open_in_memory().unwrap();
        // Insert out of order; timeline must return depth-ascending.
        store
            .apply_events(&[
                message("$c", "!r:s", "@u:s", 3, 300, "three"),
                message("$a", "!r:s", "@u:s", 1, 100, "one"),
                message("$b", "!r:s", "@u:s", 2, 200, "two"),
            ])
            .unwrap();

        // Newest `limit` = 2 → the two highest-depth, oldest-first.
        let page = store.timeline("!r:s", 2, None).unwrap();
        assert_eq!(
            page.iter().map(|e| e.event_id.as_str()).collect::<Vec<_>>(),
            vec!["$b", "$c"],
        );

        // Backfill: strictly older than depth 3 → $a, $b.
        let older = store.timeline("!r:s", 10, Some(3)).unwrap();
        assert_eq!(
            older
                .iter()
                .map(|e| e.event_id.as_str())
                .collect::<Vec<_>>(),
            vec!["$a", "$b"],
        );
    }

    #[test]
    fn timeline_is_scoped_to_its_room() {
        let store = Store::open_in_memory().unwrap();
        store
            .apply_events(&[
                message("$a", "!r1:s", "@u:s", 1, 100, "in r1"),
                message("$b", "!r2:s", "@u:s", 1, 100, "in r2"),
            ])
            .unwrap();
        let r1 = store.timeline("!r1:s", 10, None).unwrap();
        assert_eq!(r1.len(), 1);
        assert_eq!(r1[0].event_id, "$a");
    }

    #[test]
    fn state_is_folded_last_writer_wins_by_depth() {
        let store = Store::open_in_memory().unwrap();
        store
            .apply_events(&[event(
                "$n1",
                "!r:s",
                "@u:s",
                1,
                100,
                "p.room.name",
                Some(""),
                json!({ "name": "First" }),
            )])
            .unwrap();
        assert_eq!(
            store.list_rooms().unwrap()[0].name.as_deref(),
            Some("First")
        );

        // A higher-depth name wins.
        store
            .apply_events(&[event(
                "$n2",
                "!r:s",
                "@u:s",
                5,
                500,
                "p.room.name",
                Some(""),
                json!({ "name": "Second" }),
            )])
            .unwrap();
        assert_eq!(
            store.list_rooms().unwrap()[0].name.as_deref(),
            Some("Second")
        );

        // A stale (lower-depth) name that arrives late must NOT overwrite.
        store
            .apply_events(&[event(
                "$n0",
                "!r:s",
                "@u:s",
                2,
                200,
                "p.room.name",
                Some(""),
                json!({ "name": "Stale" }),
            )])
            .unwrap();
        assert_eq!(
            store.list_rooms().unwrap()[0].name.as_deref(),
            Some("Second")
        );
    }

    #[test]
    fn list_rooms_folds_name_topic_encryption_and_orders_by_activity() {
        let store = Store::open_in_memory().unwrap();
        store
            .apply_events(&[
                // Room A: named, with a topic, and a later message.
                event(
                    "$a_n",
                    "!a:s",
                    "@u:s",
                    1,
                    100,
                    "p.room.name",
                    Some(""),
                    json!({ "name": "Alpha" }),
                ),
                event(
                    "$a_t",
                    "!a:s",
                    "@u:s",
                    1,
                    100,
                    "p.room.topic",
                    Some(""),
                    json!({ "topic": "about A" }),
                ),
                message("$a_m", "!a:s", "@u:s", 2, 900, "hi"),
                // Room B: encrypted, no name, older activity.
                event(
                    "$b_e",
                    "!b:s",
                    "@u:s",
                    1,
                    300,
                    "p.room.encryption",
                    Some(""),
                    json!({ "algorithm": "p.mls.1" }),
                ),
            ])
            .unwrap();

        let rooms = store.list_rooms().unwrap();
        assert_eq!(rooms.len(), 2);
        // Most recent activity first: A (ts 900) before B (ts 300).
        assert_eq!(rooms[0].room_id, "!a:s");
        assert_eq!(rooms[0].name.as_deref(), Some("Alpha"));
        assert_eq!(rooms[0].topic.as_deref(), Some("about A"));
        assert!(!rooms[0].encrypted);
        assert_eq!(rooms[1].room_id, "!b:s");
        assert_eq!(rooms[1].name, None);
        assert!(rooms[1].encrypted);
    }

    #[test]
    fn is_room_encrypted_reflects_the_marker() {
        let store = Store::open_in_memory().unwrap();
        store
            .apply_events(&[
                // Plaintext room: just a message.
                message("$p_m", "!plain:s", "@u:s", 1, 100, "hi"),
                // Encrypted room: carries a p.room.encryption marker.
                event(
                    "$e_e",
                    "!enc:s",
                    "@u:s",
                    1,
                    100,
                    "p.room.encryption",
                    Some(""),
                    json!({ "algorithm": "p.mls.1" }),
                ),
            ])
            .unwrap();

        assert!(store.is_room_encrypted("!enc:s").unwrap());
        assert!(!store.is_room_encrypted("!plain:s").unwrap());
        // A room we've never seen isn't encrypted (and doesn't error).
        assert!(!store.is_room_encrypted("!unknown:s").unwrap());
    }

    #[test]
    fn membership_reads_latest_member_state_per_user() {
        let store = Store::open_in_memory().unwrap();
        store
            .apply_events(&[
                event(
                    "$j",
                    "!r:s",
                    "@a:s",
                    1,
                    100,
                    "p.room.member",
                    Some("@a:s"),
                    json!({ "membership": "join" }),
                ),
                event(
                    "$i",
                    "!r:s",
                    "@a:s",
                    1,
                    100,
                    "p.room.member",
                    Some("@b:s"),
                    json!({ "membership": "invite" }),
                ),
            ])
            .unwrap();
        assert_eq!(
            store.membership("!r:s", "@a:s").unwrap(),
            Some(Membership::Join)
        );
        assert_eq!(
            store.membership("!r:s", "@b:s").unwrap(),
            Some(Membership::Invite)
        );
        assert_eq!(store.membership("!r:s", "@nobody:s").unwrap(), None);

        // A later leave supersedes the join for that user.
        store
            .apply_events(&[event(
                "$l",
                "!r:s",
                "@a:s",
                4,
                400,
                "p.room.member",
                Some("@a:s"),
                json!({ "membership": "leave" }),
            )])
            .unwrap();
        assert_eq!(
            store.membership("!r:s", "@a:s").unwrap(),
            Some(Membership::Leave)
        );
    }

    #[test]
    fn unknown_membership_is_preserved() {
        assert_eq!(Membership::from_wire("knock"), Membership::Knock);
        assert_eq!(
            Membership::from_wire("weird"),
            Membership::Other("weird".to_owned())
        );
    }

    #[test]
    fn current_state_generic_accessor() {
        let store = Store::open_in_memory().unwrap();
        store
            .apply_events(&[event(
                "$t",
                "!r:s",
                "@u:s",
                1,
                100,
                "p.room.topic",
                Some(""),
                json!({ "topic": "hello" }),
            )])
            .unwrap();
        let ev = store
            .current_state("!r:s", "p.room.topic", "")
            .unwrap()
            .unwrap();
        assert_eq!(ev.content["topic"], "hello");
        assert!(store
            .current_state("!r:s", "p.room.name", "")
            .unwrap()
            .is_none());
    }

    #[test]
    fn malformed_event_aborts_batch_without_partial_write() {
        let store = Store::open_in_memory().unwrap();
        let batch = vec![
            message("$ok", "!r:s", "@u:s", 1, 100, "fine"),
            json!({ "event_id": "$bad", "room_id": "!r:s" }), // missing fields
        ];
        assert!(matches!(
            store.apply_events(&batch),
            Err(StoreError::Malformed { .. })
        ));
        // The whole batch was rejected pre-write: nothing landed.
        assert_eq!(store.timeline("!r:s", 10, None).unwrap().len(), 0);
    }

    #[test]
    fn schema_persists_across_reopen() {
        let dir = std::env::temp_dir().join(format!("pigeon-store-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("store.sqlite3");
        let _ = std::fs::remove_file(&path);

        {
            let store = Store::open(&path).unwrap();
            store.save_sync_token("7_3").unwrap();
            store
                .apply_events(&[message("$a", "!r:s", "@u:s", 1, 100, "persisted")])
                .unwrap();
        }
        // Reopen: migration is a no-op (already at version) and data survives.
        {
            let store = Store::open(&path).unwrap();
            assert_eq!(store.load_sync_token().unwrap().as_deref(), Some("7_3"));
            assert_eq!(store.timeline("!r:s", 10, None).unwrap().len(), 1);
        }
        let _ = std::fs::remove_file(&path);
    }

    // --- Outbound send queue + local echo (M2.5) -----------------------------

    #[test]
    fn queue_send_writes_a_pending_echo() {
        let store = Store::open_in_memory().unwrap();
        let txn = store.queue_send("!r:s", "@me:s", "hello").unwrap();

        // The echo shows in the timeline immediately, marked Sending.
        let tl = store.timeline("!r:s", 10, None).unwrap();
        assert_eq!(tl.len(), 1);
        assert_eq!(tl[0].sender, "@me:s");
        assert_eq!(tl[0].content["body"], "hello");
        assert_eq!(tl[0].send_state, SendState::Sending);

        // And it's queued for (re)transmission.
        let pending = store.pending_sends().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].txn_id, txn);
        assert_eq!(pending[0].body, "hello");
    }

    #[test]
    fn resolve_send_promotes_echo_and_clears_queue() {
        let store = Store::open_in_memory().unwrap();
        let txn = store.queue_send("!r:s", "@me:s", "hi").unwrap();
        store.resolve_send(&txn, "$real").unwrap();

        // The echo became the confirmed event under the real id; queue is empty.
        let tl = store.timeline("!r:s", 10, None).unwrap();
        assert_eq!(tl.len(), 1);
        assert_eq!(tl[0].event_id, "$real");
        assert_eq!(tl[0].send_state, SendState::Confirmed);
        assert!(store.pending_sends().unwrap().is_empty());

        // The authoritative event later arriving via sync dedups (no duplicate).
        store
            .apply_events(&[message("$real", "!r:s", "@me:s", 9, 900, "hi")])
            .unwrap();
        assert_eq!(store.timeline("!r:s", 10, None).unwrap().len(), 1);
    }

    #[test]
    fn resolve_send_drops_echo_if_sync_raced_ahead() {
        let store = Store::open_in_memory().unwrap();
        let txn = store.queue_send("!r:s", "@me:s", "hi").unwrap();
        // Sync delivered the real event before our ack arrived.
        store
            .apply_events(&[message("$real", "!r:s", "@me:s", 9, 900, "hi")])
            .unwrap();
        store.resolve_send(&txn, "$real").unwrap();

        // Exactly one copy remains (the confirmed one); no leftover echo.
        let tl = store.timeline("!r:s", 10, None).unwrap();
        assert_eq!(tl.len(), 1);
        assert_eq!(tl[0].event_id, "$real");
        assert_eq!(tl[0].send_state, SendState::Confirmed);
    }

    // --- Encrypted-message plaintext cache (M3.5) ----------------------------

    fn encrypted(id: &str, room: &str, depth: i64, ciphertext: &str) -> Value {
        event(
            id,
            room,
            "@alice:s",
            depth,
            depth * 100,
            "p.room.encrypted",
            None,
            json!({ "algorithm": "p.mls.1", "ciphertext": ciphertext }),
        )
    }

    #[test]
    fn pending_decrypts_returns_encrypted_events_in_dag_order() {
        let store = Store::open_in_memory().unwrap();
        store
            .apply_events(&[
                encrypted("$e2", "!r:s", 2, "CT2"),
                message("$m", "!r:s", "@a:s", 1, 100, "plaintext"),
                encrypted("$e1", "!r:s", 1, "CT1"),
            ])
            .unwrap();

        // Only encrypted events, oldest-first (ratchet order — Gotcha #3).
        let pending = store.pending_decrypts().unwrap();
        assert_eq!(
            pending
                .iter()
                .map(|p| p.event_id.as_str())
                .collect::<Vec<_>>(),
            vec!["$e1", "$e2"]
        );
        assert_eq!(pending[0].ciphertext_b64, "CT1");
    }

    #[test]
    fn set_decrypted_caches_plaintext_and_clears_pending() {
        let store = Store::open_in_memory().unwrap();
        store
            .apply_events(&[encrypted("$e1", "!r:s", 1, "CT1")])
            .unwrap();

        // Before: pending, no cached plaintext in the timeline read.
        assert_eq!(store.pending_decrypts().unwrap().len(), 1);
        assert_eq!(store.timeline("!r:s", 10, None).unwrap()[0].decrypted, None);

        store.set_decrypted("$e1", "hello").unwrap();

        // After: plaintext cached, no longer pending (never re-decrypt — Gotcha #3).
        assert!(store.pending_decrypts().unwrap().is_empty());
        let tl = store.timeline("!r:s", 10, None).unwrap();
        assert_eq!(tl[0].decrypted.as_deref(), Some("hello"));
        assert!(!tl[0].decrypt_failed);
    }

    #[test]
    fn set_decrypt_failed_marks_terminal_and_clears_pending() {
        let store = Store::open_in_memory().unwrap();
        store
            .apply_events(&[encrypted("$e1", "!r:s", 1, "CT1")])
            .unwrap();
        store.set_decrypt_failed("$e1").unwrap();

        assert!(store.pending_decrypts().unwrap().is_empty());
        let tl = store.timeline("!r:s", 10, None).unwrap();
        assert_eq!(tl[0].decrypted, None);
        assert!(tl[0].decrypt_failed);
    }

    #[test]
    fn decrypt_cache_survives_reopen() {
        let dir = std::env::temp_dir().join(format!("pigeon-dec-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("dec.sqlite3");
        let _ = std::fs::remove_file(&path);
        {
            let store = Store::open(&path).unwrap();
            store
                .apply_events(&[encrypted("$e1", "!r:s", 1, "CT1")])
                .unwrap();
            store.set_decrypted("$e1", "persisted plaintext").unwrap();
        }
        {
            // Reopen: the v3 migration is a no-op and the cached plaintext survives
            // (so we never need to re-decrypt after the ratchet moved — Gotcha #3).
            let store = Store::open(&path).unwrap();
            let tl = store.timeline("!r:s", 10, None).unwrap();
            assert_eq!(tl[0].decrypted.as_deref(), Some("persisted plaintext"));
            assert!(store.pending_decrypts().unwrap().is_empty());
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn fail_send_flags_echo_and_dequeues() {
        let store = Store::open_in_memory().unwrap();
        let txn = store.queue_send("!r:s", "@me:s", "nope").unwrap();
        store.fail_send(&txn).unwrap();

        let tl = store.timeline("!r:s", 10, None).unwrap();
        assert_eq!(tl.len(), 1);
        assert_eq!(tl[0].send_state, SendState::Failed);
        // Removed from the retry queue — we won't keep resending it.
        assert!(store.pending_sends().unwrap().is_empty());
    }
}
