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
//! - **Text the server never saw is sealed at rest** (schema v4). The cached
//!   plaintext of encrypted messages (`events.decrypted`), the body of our own
//!   messages in encrypted rooms (moved out of the event JSON into `decrypted`),
//!   and every queued send's body (`pending_sends.body`) are stored as
//!   `s1:` + base64(`pigeon_crypto::seal`) under a per-install store key held only
//!   in the platform keystore (`pigeon.store.key.v1`). Each value is bound to its
//!   row (the event id, or the send's txn id), so it can't be swapped onto
//!   another. Plaintext-room messages, room names and state stay as they are: the
//!   server sees them anyway. A sealed check value in `store_meta` detects a
//!   store opened with a different key (e.g. the keystore entry was lost): the
//!   cache is then wiped and rebuilt from the server rather than failing every
//!   read.
//!
//! The store is a plain Rust module (no UniFFI surface of its own yet); the sync
//! loop and the room/timeline FFI records are built on top of it in M2.2–M2.4.

use std::path::Path;
use std::sync::Mutex;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use rusqlite::{Connection, OptionalExtension};
use serde_json::Value;

use crate::CoreError;

/// The current schema version. Bumped when a migration is added; see [`migrate`].
const SCHEMA_VERSION: i64 = 4;

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
    /// A sealed value couldn't be opened with the store key (wrong key, value
    /// moved to another row, or tampered), or a value couldn't be sealed.
    #[error("sealed store value: {reason}")]
    Sealed { reason: String },
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
    /// Client transaction id, reused on every retry of this send — the server
    /// dedups on it, so a retry after a lost response returns the original event.
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
    /// Who sent it — the decrypt pass recognises our own just-sent messages.
    pub sender: String,
    /// The base64 MLS ciphertext (`content.ciphertext`).
    pub ciphertext_b64: String,
}

/// A `p.mls.commit` event awaiting application (`pigeon-crypto` finding C1). An
/// add-member broadcast the group's new epoch to existing members; each applies
/// it in DAG order (before any later `p.room.encrypted` at the new epoch) via
/// `process_commit`, exactly once. `sender` lets the caller skip its **own**
/// commit (already self-merged on add).
#[derive(Debug, Clone)]
pub struct PendingCommit {
    pub event_id: String,
    pub room_id: String,
    pub sender: String,
    /// The base64 MLS commit (`content.commit`).
    pub commit_b64: String,
}

/// One inbound MLS event awaiting processing — a message to decrypt or a commit to
/// apply. [`Store::pending_mls`] yields these in one DAG-ordered stream so the
/// ratchet advances correctly: a commit that changes the epoch is applied before
/// the encrypted messages that follow it (Gotcha #3, finding C1).
#[derive(Debug, Clone)]
pub enum PendingMls {
    Message(PendingDecrypt),
    Commit(PendingCommit),
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
    cipher: Cipher,
}

/// Prefix of a sealed column value; the rest is base64 of `pigeon_crypto::seal`.
const SEALED_PREFIX: &str = "s1:";
/// `store_meta` key of the sealed check value used to detect a key change.
const CANARY_KEY: &str = "canary";
const CANARY_PLAINTEXT: &str = "pigeon-store";

/// Seals and opens the store's at-rest values under the per-install store key.
/// All crypto goes through `pigeon-crypto` (the Cardinal Rule); this only frames
/// the result as text for SQLite.
struct Cipher {
    key: Vec<u8>,
}

impl Cipher {
    /// `s1:` + base64(seal(`plaintext`, bound to `context`)).
    fn seal(&self, plaintext: &str, context: &str) -> Result<String, StoreError> {
        let sealed = pigeon_crypto::seal(&self.key, plaintext.as_bytes(), context.as_bytes())
            .map_err(|e| StoreError::Sealed {
                reason: e.to_string(),
            })?;
        Ok(format!("{SEALED_PREFIX}{}", STANDARD.encode(sealed)))
    }

    /// Open a value written by [`Cipher::seal`] for the same `context`. A value
    /// without the prefix predates sealing and is returned as-is (only reachable
    /// for rows the v4 migration doesn't cover).
    fn open(&self, value: &str, context: &str) -> Result<String, StoreError> {
        let Some(b64) = value.strip_prefix(SEALED_PREFIX) else {
            return Ok(value.to_owned());
        };
        let sealed = STANDARD.decode(b64).map_err(|e| StoreError::Sealed {
            reason: format!("bad encoding: {e}"),
        })?;
        let plain =
            pigeon_crypto::open_sealed(&self.key, &sealed, context.as_bytes()).map_err(|e| {
                StoreError::Sealed {
                    reason: e.to_string(),
                }
            })?;
        String::from_utf8(plain).map_err(|e| StoreError::Sealed {
            reason: format!("not UTF-8: {e}"),
        })
    }
}

impl Store {
    /// Open (creating if absent) the store at `path` under the store `key` (from
    /// the platform keystore; see `session::store_key`) and bring the schema up to
    /// date. Used on device with the host's app-private data dir.
    pub fn open(path: &Path, key: Vec<u8>) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        Self::init(conn, key)
    }

    /// Open an in-memory store under a fresh random key — for host unit tests
    /// (no file, no device, no keystore).
    pub fn open_in_memory() -> Result<Self, StoreError> {
        let conn = Connection::open_in_memory()?;
        let key = pigeon_crypto::new_seal_key().map_err(|e| StoreError::Sealed {
            reason: e.to_string(),
        })?;
        Self::init(conn, key)
    }

    fn init(conn: Connection, key: Vec<u8>) -> Result<Self, StoreError> {
        // WAL for durability + reader/writer concurrency; enforce FKs so a
        // dangling state→event reference can't be written.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", true)?;
        // Overwrite deleted/replaced content with zeros, so text that was sealed
        // or wiped doesn't linger in freed pages of the file.
        conn.pragma_update(None, "secure_delete", true)?;
        let cipher = Cipher { key };
        migrate(&conn, &cipher)?;
        check_key(&conn, &cipher)?;
        Ok(Store {
            conn: Mutex::new(conn),
            cipher,
        })
    }

    /// Delete everything the store holds (sign-out): the event log, room state,
    /// the send queue, the sync token and the key check value, so the next
    /// session starts empty under a fresh key.
    pub fn wipe(&self) -> Result<(), StoreError> {
        wipe_all(&self.lock())
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
        // `p.mls.commit` events are MLS group plumbing (finding C1), not user
        // content — they carry no renderable body, so exclude them from the
        // timeline (and from pagination counts).
        let sql = "SELECT payload, local, decrypted, decrypt_state FROM events
                   WHERE room_id = ?1 AND (?2 IS NULL OR depth < ?2)
                     AND type != 'p.mls.commit'
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
                &self.cipher,
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
            Some(p) => Ok(Some(parse_stored_payload(&self.cipher, &p, 0, None, 0)?)),
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
        // In an encrypted room the body is text the server never sees in the
        // clear: keep it out of the event JSON and seal it in `decrypted` (bound to
        // this row's id). A plaintext room's echo keeps the body inline.
        let encrypted: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM room_state
                            WHERE room_id = ?1 AND type = 'p.room.encryption')",
            [room_id],
            |r| r.get::<_, i64>(0),
        )? != 0;
        let (content, sealed_body) = if encrypted {
            (
                serde_json::json!({ "msgtype": "p.text" }),
                Some(self.cipher.seal(body, &event_id)?),
            )
        } else {
            (
                serde_json::json!({ "body": body, "msgtype": "p.text" }),
                None,
            )
        };
        // Build a full event payload (including `depth`) so a timeline read can
        // round-trip it back through `StoredEvent::from_wire` like any event.
        let payload = serde_json::json!({
            "event_id": event_id,
            "room_id": room_id,
            "sender": sender,
            "type": "p.room.message",
            "origin_server_ts": ts,
            "depth": depth,
            "content": content,
        });
        tx.execute(
            "INSERT INTO events
               (event_id, room_id, sender, type, state_key, origin_server_ts, depth, payload, local, decrypted)
             VALUES (?1, ?2, ?3, 'p.room.message', NULL, ?4, ?5, ?6, 1, ?7)",
            rusqlite::params![event_id, room_id, sender, ts, depth, payload.to_string(), sealed_body],
        )?;
        // The queued body is sealed in every room (bound to the txn id): the queue
        // is only ever read back through `pending_sends`.
        tx.execute(
            "INSERT INTO pending_sends (txn_id, room_id, body, event_id, created_ts)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                txn_id,
                room_id,
                self.cipher.seal(body, &txn_id)?,
                event_id,
                ts
            ],
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
            let mut send = row?;
            send.body = self.cipher.open(&send.body, &send.txn_id)?;
            out.push(send);
        }
        Ok(out)
    }

    /// Whether `room_id` has a queued send not yet acknowledged by the server.
    /// The decrypt pass uses it to leave our own just-sent ciphertext pending
    /// while the ack (which carries its plaintext — see
    /// [`resolve_send`](Self::resolve_send)) is still in flight.
    pub fn has_pending_send(&self, room_id: &str) -> Result<bool, StoreError> {
        let guard = self.lock();
        let pending: i64 = guard.query_row(
            "SELECT EXISTS(SELECT 1 FROM pending_sends WHERE room_id = ?1)",
            [room_id],
            |r| r.get(0),
        )?;
        Ok(pending != 0)
    }

    /// Mark a queued send acknowledged by the server under `real_event_id`.
    /// Promotes the provisional echo to the real id (so the authoritative event
    /// arriving via `/sync` dedups against it — no duplicate, no flicker), or
    /// drops the echo if sync already delivered that event. Clears the queue row.
    ///
    /// When sync raced ahead with our own **encrypted** event, the echo's body is
    /// the only plaintext we will ever have for it — MLS can't decrypt a device's
    /// own messages — so it is carried onto the real event as its cached
    /// plaintext before the echo goes (overriding a failed decrypt attempt the
    /// decrypt pass may already have made).
    pub fn resolve_send(&self, txn_id: &str, real_event_id: &str) -> Result<(), StoreError> {
        let mut guard = self.lock();
        let tx = guard.transaction()?;
        let pending: Option<(String, String)> = tx
            .query_row(
                "SELECT event_id, body FROM pending_sends WHERE txn_id = ?1",
                [txn_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((provisional, sealed_body)) = pending {
            // The plaintext, re-sealed for the row it ends up on (the real id).
            let body = self.cipher.open(&sealed_body, txn_id)?;
            let sealed_for_real = self.cipher.seal(&body, real_event_id)?;
            let real_exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM events WHERE event_id = ?1)",
                [real_event_id],
                |r| r.get::<_, i64>(0),
            )? != 0;
            if real_exists {
                // Sync raced ahead of the ack: keep our plaintext on the real
                // event if it's ciphertext, then drop the now-redundant echo.
                tx.execute(
                    "UPDATE events SET decrypted = ?2, decrypt_state = ?3
                      WHERE event_id = ?1 AND type = 'p.room.encrypted'",
                    rusqlite::params![real_event_id, sealed_for_real, DECRYPT_OK],
                )?;
                tx.execute("DELETE FROM events WHERE event_id = ?1", [&provisional])?;
            } else {
                // Promote the echo to the confirmed event in place — rewrite both
                // the column and the payload's own `event_id` (a timeline read
                // rebuilds the event from the payload), and clear the local flag.
                // An encrypted room's echo carries its sealed body in `decrypted`,
                // bound to the provisional id: re-bind it to the real one.
                tx.execute(
                    "UPDATE events
                        SET event_id = ?1,
                            local = 0,
                            payload = json_set(payload, '$.event_id', ?1),
                            decrypted = CASE WHEN decrypted IS NULL THEN NULL ELSE ?3 END
                      WHERE event_id = ?2",
                    rusqlite::params![real_event_id, provisional, sealed_for_real],
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

    /// Inbound MLS events not yet processed — `p.room.encrypted` to decrypt and
    /// `p.mls.commit` to apply — in one DAG-ordered stream (`depth ASC, event_id
    /// ASC`). Both advance/depend on the MLS ratchet, which is order-sensitive
    /// (Gotcha #3, finding C1): a commit that changes the group epoch must be
    /// applied before the encrypted messages after it, so they share one ordering.
    /// The caller processes each exactly once, marking it done via
    /// [`set_decrypted`](Self::set_decrypted)/[`set_decrypt_failed`](Self::set_decrypt_failed)
    /// (messages) or [`set_commit_processed`](Self::set_commit_processed) (commits).
    /// Events with no `content.ciphertext`/`content.commit` are skipped (malformed).
    pub fn pending_mls(&self) -> Result<Vec<PendingMls>, StoreError> {
        let guard = self.lock();
        let mut stmt = guard.prepare(
            "SELECT event_id, room_id, sender, type,
                    json_extract(payload, '$.content.ciphertext'),
                    json_extract(payload, '$.content.commit')
             FROM events
             WHERE type IN ('p.room.encrypted', 'p.mls.commit') AND decrypt_state = 0
             ORDER BY depth ASC, event_id ASC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<String>>(5)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (event_id, room_id, sender, ty, ciphertext, commit) = row?;
            match ty.as_str() {
                "p.room.encrypted" => {
                    if let Some(ciphertext_b64) = ciphertext {
                        out.push(PendingMls::Message(PendingDecrypt {
                            event_id,
                            room_id,
                            sender,
                            ciphertext_b64,
                        }));
                    }
                }
                "p.mls.commit" => {
                    if let Some(commit_b64) = commit {
                        out.push(PendingMls::Commit(PendingCommit {
                            event_id,
                            room_id,
                            sender,
                            commit_b64,
                        }));
                    }
                }
                _ => {}
            }
        }
        Ok(out)
    }

    /// Cache the decrypted plaintext of an encrypted event (M3.5) and mark it
    /// decrypted, so it renders as a normal message and is never re-decrypted
    /// (the ratchet has already advanced — Gotcha #3).
    pub fn set_decrypted(&self, event_id: &str, plaintext: &str) -> Result<(), StoreError> {
        let sealed = self.cipher.seal(plaintext, event_id)?;
        self.lock().execute(
            "UPDATE events SET decrypted = ?2, decrypt_state = ?3 WHERE event_id = ?1",
            rusqlite::params![event_id, sealed, DECRYPT_OK],
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

    /// Mark a `p.mls.commit` event as processed (finding C1), so it's never
    /// re-applied — the ratchet has advanced (or the commit didn't apply to us and
    /// never will). Commits carry no plaintext and don't render; this only clears
    /// them from [`pending_mls`](Self::pending_mls).
    pub fn set_commit_processed(&self, event_id: &str) -> Result<(), StoreError> {
        self.lock().execute(
            "UPDATE events SET decrypt_state = ?2 WHERE event_id = ?1",
            rusqlite::params![event_id, DECRYPT_OK],
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
    cipher: &Cipher,
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
    let decrypted = decrypted
        .map(|sealed| cipher.open(&sealed, &event.event_id))
        .transpose()?;
    // Our own message in an encrypted room keeps its body sealed in `decrypted`
    // rather than in the event JSON: put it back where the renderer reads it.
    if event.event_type == "p.room.message" && event.content.get("body").is_none() {
        if let (Some(body), Some(content)) = (decrypted.as_ref(), event.content.as_object_mut()) {
            content.insert("body".to_owned(), Value::String(body.clone()));
        }
    }
    event.decrypted = decrypted;
    event.decrypt_failed = decrypt_state == DECRYPT_FAILED;
    Ok(event)
}

/// Verify the store key against the sealed check value in `store_meta`. A store
/// last written under a different key (the keystore entry was lost or replaced)
/// can't be read, so it is wiped and rebuilt from the server by the next sync,
/// the same outcome as reinstalling. A new or just-wiped store gets a fresh check
/// value.
fn check_key(conn: &Connection, cipher: &Cipher) -> Result<(), StoreError> {
    let canary: Option<String> = conn
        .query_row(
            "SELECT value FROM store_meta WHERE key = ?1",
            [CANARY_KEY],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(value) = canary {
        if cipher.open(&value, CANARY_KEY).is_ok() {
            return Ok(());
        }
        crate::emit(
            crate::LogLevel::Warn,
            "store",
            "store key changed (keystore entry lost?): clearing the local cache; it rebuilds from the server",
        );
        wipe_all(conn)?;
    }
    conn.execute(
        "INSERT OR REPLACE INTO store_meta (key, value) VALUES (?1, ?2)",
        rusqlite::params![CANARY_KEY, cipher.seal(CANARY_PLAINTEXT, CANARY_KEY)?],
    )?;
    Ok(())
}

/// Empty every table (see [`Store::wipe`]).
fn wipe_all(conn: &Connection) -> Result<(), StoreError> {
    conn.execute_batch(
        "DELETE FROM pending_sends;
         DELETE FROM room_state;
         DELETE FROM events;
         DELETE FROM sync_token;
         DELETE FROM store_meta;",
    )?;
    Ok(())
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
fn migrate(conn: &Connection, cipher: &Cipher) -> Result<(), StoreError> {
    migrate_to(conn, cipher, SCHEMA_VERSION)
}

/// Run the migrations up to `target` (tests stop early to build an old store).
fn migrate_to(conn: &Connection, cipher: &Cipher, target: i64) -> Result<(), StoreError> {
    let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if version < 1 && target >= 1 {
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
    if version < 2 && target >= 2 {
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
    if version < 3 && target >= 3 {
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
    if version < 4 && target >= 4 {
        migrate_v4_seal(conn, cipher)?;
    }
    conn.pragma_update(None, "user_version", target.max(version))?;
    Ok(())
}

/// Schema v4: seal the text the server never saw in the clear, in one
/// transaction so a crash leaves either the old store or the new one.
/// - `events.decrypted` (cached plaintext of encrypted messages), bound to the
///   event id;
/// - message bodies in encrypted rooms (our own sends and echoes): moved out of
///   the event JSON into a sealed `decrypted`;
/// - `pending_sends.body`, bound to the txn id.
///
/// Also creates `store_meta`, which holds the key check value.
fn migrate_v4_seal(conn: &Connection, cipher: &Cipher) -> Result<(), StoreError> {
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch("CREATE TABLE store_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);")?;
    let rows: Vec<(String, String)> = {
        let mut stmt =
            tx.prepare("SELECT event_id, decrypted FROM events WHERE decrypted IS NOT NULL")?;
        let it = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        it.collect::<Result<_, _>>()?
    };
    for (id, plain) in rows {
        tx.execute(
            "UPDATE events SET decrypted = ?2 WHERE event_id = ?1",
            rusqlite::params![id, cipher.seal(&plain, &id)?],
        )?;
    }
    let bodies: Vec<(String, String)> = {
        let mut stmt = tx.prepare(
            "SELECT event_id, json_extract(payload, '$.content.body') FROM events
              WHERE type = 'p.room.message'
                AND json_extract(payload, '$.content.body') IS NOT NULL
                AND room_id IN (SELECT room_id FROM room_state WHERE type = 'p.room.encryption')",
        )?;
        let it = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        it.collect::<Result<_, _>>()?
    };
    for (id, body) in bodies {
        tx.execute(
            "UPDATE events SET decrypted = ?2, payload = json_remove(payload, '$.content.body')
              WHERE event_id = ?1",
            rusqlite::params![id, cipher.seal(&body, &id)?],
        )?;
    }
    let sends: Vec<(String, String)> = {
        let mut stmt = tx.prepare("SELECT txn_id, body FROM pending_sends")?;
        let it = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        it.collect::<Result<_, _>>()?
    };
    for (txn, body) in sends {
        tx.execute(
            "UPDATE pending_sends SET body = ?2 WHERE txn_id = ?1",
            rusqlite::params![txn, cipher.seal(&body, &txn)?],
        )?;
    }
    tx.pragma_update(None, "user_version", 4)?;
    tx.commit()?;
    // Move the rewritten pages into the main file and truncate the WAL, so the
    // pre-migration plaintext doesn't survive in the log.
    conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A fixed store key for tests that reopen a file-backed store.
    const TEST_KEY: [u8; 16] = [7; 16];

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
            let store = Store::open(&path, TEST_KEY.to_vec()).unwrap();
            store.save_sync_token("7_3").unwrap();
            store
                .apply_events(&[message("$a", "!r:s", "@u:s", 1, 100, "persisted")])
                .unwrap();
        }
        // Reopen: migration is a no-op (already at version) and data survives.
        {
            let store = Store::open(&path, TEST_KEY.to_vec()).unwrap();
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

    fn own_encrypted(id: &str, room: &str, depth: i64) -> Value {
        event(
            id,
            room,
            "@me:s",
            depth,
            depth * 100,
            "p.room.encrypted",
            None,
            json!({ "algorithm": "p.mls.1", "ciphertext": "CT" }),
        )
    }

    // Our own encrypted message synced before the ack: the echo is the only
    // plaintext we'll ever have (MLS can't decrypt our own messages), so it must
    // survive onto the real event rather than be dropped.
    #[test]
    fn resolve_send_keeps_plaintext_when_encrypted_sync_raced_ahead() {
        let store = Store::open_in_memory().unwrap();
        let txn = store.queue_send("!r:s", "@me:s", "secret hi").unwrap();
        store
            .apply_events(&[own_encrypted("$real", "!r:s", 9)])
            .unwrap();
        store.resolve_send(&txn, "$real").unwrap();

        let tl = store.timeline("!r:s", 10, None).unwrap();
        assert_eq!(tl.len(), 1);
        assert_eq!(tl[0].event_id, "$real");
        assert_eq!(tl[0].decrypted.as_deref(), Some("secret hi"));
        assert!(!tl[0].decrypt_failed);
        assert_eq!(tl[0].send_state, SendState::Confirmed);
        // Nothing left for the decrypt pass to (fail to) decrypt.
        assert!(store.pending_mls().unwrap().is_empty());
    }

    // The decrypt pass may already have tried — and failed — on our own event
    // before the ack landed; resolving the send repairs it.
    #[test]
    fn resolve_send_repairs_an_own_event_already_marked_undecryptable() {
        let store = Store::open_in_memory().unwrap();
        let txn = store.queue_send("!r:s", "@me:s", "secret hi").unwrap();
        store
            .apply_events(&[own_encrypted("$real", "!r:s", 9)])
            .unwrap();
        store.set_decrypt_failed("$real").unwrap();
        store.resolve_send(&txn, "$real").unwrap();

        let tl = store.timeline("!r:s", 10, None).unwrap();
        assert_eq!(tl.len(), 1);
        assert_eq!(tl[0].decrypted.as_deref(), Some("secret hi"));
        assert!(!tl[0].decrypt_failed);
    }

    #[test]
    fn has_pending_send_tracks_the_queue_per_room() {
        let store = Store::open_in_memory().unwrap();
        assert!(!store.has_pending_send("!r:s").unwrap());
        let txn = store.queue_send("!r:s", "@me:s", "hi").unwrap();
        assert!(store.has_pending_send("!r:s").unwrap());
        assert!(!store.has_pending_send("!other:s").unwrap());
        store.resolve_send(&txn, "$real").unwrap();
        assert!(!store.has_pending_send("!r:s").unwrap());
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

    fn commit(id: &str, room: &str, sender: &str, depth: i64, commit_b64: &str) -> Value {
        event(
            id,
            room,
            sender,
            depth,
            depth * 100,
            "p.mls.commit",
            None,
            json!({ "commit": commit_b64 }),
        )
    }

    /// The message (decrypt) entries of the unified pending-MLS stream.
    fn pending_msgs(store: &Store) -> Vec<PendingDecrypt> {
        store
            .pending_mls()
            .unwrap()
            .into_iter()
            .filter_map(|p| match p {
                PendingMls::Message(m) => Some(m),
                PendingMls::Commit(_) => None,
            })
            .collect()
    }

    #[test]
    fn pending_mls_returns_encrypted_events_in_dag_order() {
        let store = Store::open_in_memory().unwrap();
        store
            .apply_events(&[
                encrypted("$e2", "!r:s", 2, "CT2"),
                message("$m", "!r:s", "@a:s", 1, 100, "plaintext"),
                encrypted("$e1", "!r:s", 1, "CT1"),
            ])
            .unwrap();

        // Only encrypted events, oldest-first (ratchet order — Gotcha #3).
        let pending = pending_msgs(&store);
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
    fn pending_mls_interleaves_commits_and_messages_in_dag_order() {
        let store = Store::open_in_memory().unwrap();
        // A commit (depth 2) sits between two encrypted messages — it must be
        // yielded in DAG order so the caller applies it before the depth-3 message
        // (finding C1). A plaintext message and a processed commit are excluded.
        store
            .apply_events(&[
                encrypted("$e3", "!r:s", 3, "CT3"),
                commit("$c", "!r:s", "@alice:s", 2, "COMMIT"),
                encrypted("$e1", "!r:s", 1, "CT1"),
                message("$m", "!r:s", "@a:s", 4, 400, "plaintext"),
            ])
            .unwrap();

        let kinds: Vec<(&str, String)> = store
            .pending_mls()
            .unwrap()
            .into_iter()
            .map(|p| match p {
                PendingMls::Message(m) => ("msg", m.event_id),
                PendingMls::Commit(c) => ("commit", c.event_id),
            })
            .collect();
        assert_eq!(
            kinds,
            vec![
                ("msg", "$e1".to_owned()),
                ("commit", "$c".to_owned()),
                ("msg", "$e3".to_owned())
            ]
        );

        // A processed commit drops out of the stream; the commit never renders.
        store.set_commit_processed("$c").unwrap();
        assert!(store
            .pending_mls()
            .unwrap()
            .iter()
            .all(|p| !matches!(p, PendingMls::Commit(_))));
        assert!(store
            .timeline("!r:s", 10, None)
            .unwrap()
            .iter()
            .all(|e| e.event_id != "$c"));
    }

    #[test]
    fn set_decrypted_caches_plaintext_and_clears_pending() {
        let store = Store::open_in_memory().unwrap();
        store
            .apply_events(&[encrypted("$e1", "!r:s", 1, "CT1")])
            .unwrap();

        // Before: pending, no cached plaintext in the timeline read.
        assert_eq!(pending_msgs(&store).len(), 1);
        assert_eq!(store.timeline("!r:s", 10, None).unwrap()[0].decrypted, None);

        store.set_decrypted("$e1", "hello").unwrap();

        // After: plaintext cached, no longer pending (never re-decrypt — Gotcha #3).
        assert!(pending_msgs(&store).is_empty());
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

        assert!(pending_msgs(&store).is_empty());
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
            let store = Store::open(&path, TEST_KEY.to_vec()).unwrap();
            store
                .apply_events(&[encrypted("$e1", "!r:s", 1, "CT1")])
                .unwrap();
            store.set_decrypted("$e1", "persisted plaintext").unwrap();
        }
        {
            // Reopen: the v3 migration is a no-op and the cached plaintext survives
            // (so we never need to re-decrypt after the ratchet moved — Gotcha #3).
            let store = Store::open(&path, TEST_KEY.to_vec()).unwrap();
            let tl = store.timeline("!r:s", 10, None).unwrap();
            assert_eq!(tl[0].decrypted.as_deref(), Some("persisted plaintext"));
            assert!(pending_msgs(&store).is_empty());
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

    // --- Sealed at rest (schema v4) ------------------------------------------

    /// A room marked encrypted (`p.room.encryption` state), so `queue_send` seals.
    fn encrypted_room(store: &Store, room: &str) {
        store
            .apply_events(&[event(
                "$enc",
                room,
                "@alice:s",
                1,
                100,
                "p.room.encryption",
                Some(""),
                json!({ "algorithm": "p.mls.1" }),
            )])
            .unwrap();
    }

    /// The db file plus its WAL, as bytes, for "is this text on disk?" checks.
    fn file_bytes(path: &Path) -> Vec<u8> {
        let mut all = std::fs::read(path).unwrap_or_default();
        let mut wal = path.as_os_str().to_owned();
        wal.push("-wal");
        all.extend(std::fs::read(&wal).unwrap_or_default());
        all
    }

    fn contains(haystack: &[u8], needle: &str) -> bool {
        haystack
            .windows(needle.len())
            .any(|w| w == needle.as_bytes())
    }

    fn temp_store_path(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("pigeon-seal-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("store.sqlite3")
    }

    #[test]
    fn text_the_server_never_saw_is_not_on_disk_in_clear() {
        let path = temp_store_path("disk");
        {
            let store = Store::open(&path, TEST_KEY.to_vec()).unwrap();
            encrypted_room(&store, "!r:s");
            // Our own draft in an encrypted room, and a decrypted incoming message.
            let txn = store
                .queue_send("!r:s", "@alice:s", "my secret draft")
                .unwrap();
            store
                .apply_events(&[encrypted("$e1", "!r:s", 5, "CT1")])
                .unwrap();
            store.set_decrypted("$e1", "their secret reply").unwrap();

            // Both still read back as plaintext through the store.
            let tl = store.timeline("!r:s", 10, None).unwrap();
            let echo = tl
                .iter()
                .find(|e| e.event_type == "p.room.message")
                .unwrap();
            assert_eq!(echo.content["body"], "my secret draft");
            assert_eq!(
                tl.iter()
                    .find(|e| e.event_id == "$e1")
                    .unwrap()
                    .decrypted
                    .as_deref(),
                Some("their secret reply")
            );
            assert_eq!(store.pending_sends().unwrap()[0].body, "my secret draft");
            // Even after the ack re-binds the echo to its real id.
            store.resolve_send(&txn, "$real").unwrap();
            let tl = store.timeline("!r:s", 10, None).unwrap();
            assert_eq!(
                tl.iter().find(|e| e.event_id == "$real").unwrap().content["body"],
                "my secret draft"
            );
        }
        let bytes = file_bytes(&path);
        assert!(!bytes.is_empty());
        assert!(
            !contains(&bytes, "my secret draft"),
            "own draft leaked to disk"
        );
        assert!(
            !contains(&bytes, "their secret reply"),
            "decrypted text leaked to disk"
        );
    }

    #[test]
    fn plaintext_room_echoes_stay_inline() {
        // The server sees plaintext-room messages anyway; only the queue is sealed.
        let store = Store::open_in_memory().unwrap();
        store
            .queue_send("!plain:s", "@alice:s", "hello all")
            .unwrap();
        let tl = store.timeline("!plain:s", 10, None).unwrap();
        assert_eq!(tl[0].content["body"], "hello all");
        assert!(tl[0].decrypted.is_none());
        let raw: String = store
            .lock()
            .query_row("SELECT body FROM pending_sends", [], |r| r.get(0))
            .unwrap();
        assert!(
            raw.starts_with(SEALED_PREFIX),
            "the queue is sealed in every room"
        );
    }

    #[test]
    fn a_sealed_value_moved_or_tampered_fails_with_a_typed_error() {
        let store = Store::open_in_memory().unwrap();
        store
            .apply_events(&[
                encrypted("$e1", "!r:s", 1, "CT1"),
                encrypted("$e2", "!r:s", 2, "CT2"),
            ])
            .unwrap();
        store.set_decrypted("$e1", "for e1 only").unwrap();
        // Move $e1's sealed value onto $e2: it's bound to $e1, so it won't open.
        store
            .lock()
            .execute(
                "UPDATE events SET decrypted = (SELECT decrypted FROM events WHERE event_id = '$e1'),
                                   decrypt_state = 1 WHERE event_id = '$e2'",
                [],
            )
            .unwrap();
        assert!(matches!(
            store.timeline("!r:s", 10, None),
            Err(StoreError::Sealed { .. })
        ));

        // Flip a byte inside a sealed value.
        let store = Store::open_in_memory().unwrap();
        store
            .apply_events(&[encrypted("$e1", "!r:s", 1, "CT1")])
            .unwrap();
        store.set_decrypted("$e1", "secret").unwrap();
        let sealed: String = store
            .lock()
            .query_row(
                "SELECT decrypted FROM events WHERE event_id = '$e1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let mut bytes = STANDARD.decode(&sealed[SEALED_PREFIX.len()..]).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        let tampered = format!("{SEALED_PREFIX}{}", STANDARD.encode(bytes));
        store
            .lock()
            .execute(
                "UPDATE events SET decrypted = ?1 WHERE event_id = '$e1'",
                [tampered],
            )
            .unwrap();
        assert!(matches!(
            store.timeline("!r:s", 10, None),
            Err(StoreError::Sealed { .. })
        ));
    }

    #[test]
    fn reopening_under_a_different_key_clears_the_cache_instead_of_failing() {
        // The keystore entry was lost: the old store can't be read, so it's wiped
        // and rebuilt by sync, rather than failing every read (or panicking).
        let path = temp_store_path("rekey");
        {
            let store = Store::open(&path, TEST_KEY.to_vec()).unwrap();
            store.save_sync_token("9_9").unwrap();
            store
                .apply_events(&[encrypted("$e1", "!r:s", 1, "CT1")])
                .unwrap();
            store.set_decrypted("$e1", "old secret").unwrap();
        }
        let store = Store::open(&path, vec![9u8; 16]).unwrap();
        assert!(store.timeline("!r:s", 10, None).unwrap().is_empty());
        assert_eq!(
            store.load_sync_token().unwrap(),
            None,
            "resync from scratch"
        );
        // The new key is now the store's key: a reopen with it keeps working.
        store.save_sync_token("1_1").unwrap();
        drop(store);
        let store = Store::open(&path, vec![9u8; 16]).unwrap();
        assert_eq!(store.load_sync_token().unwrap().as_deref(), Some("1_1"));
    }

    #[test]
    fn wipe_empties_the_store() {
        let store = Store::open_in_memory().unwrap();
        store.save_sync_token("3_3").unwrap();
        store.queue_send("!r:s", "@alice:s", "draft").unwrap();
        store.wipe().unwrap();
        assert_eq!(store.load_sync_token().unwrap(), None);
        assert!(store.timeline("!r:s", 10, None).unwrap().is_empty());
        assert!(store.pending_sends().unwrap().is_empty());
    }

    #[test]
    fn a_v3_store_migrates_to_sealed_v4() {
        let path = temp_store_path("v3");
        {
            // Build a v3 store the way an older app left it: plaintext everywhere.
            let conn = Connection::open(&path).unwrap();
            conn.pragma_update(None, "journal_mode", "WAL").unwrap();
            let unused = Cipher {
                key: TEST_KEY.to_vec(),
            };
            migrate_to(&conn, &unused, 3).unwrap();
            let enc = json!({"event_id":"$enc","room_id":"!r:s","sender":"@a:s","type":"p.room.encryption",
                             "state_key":"","origin_server_ts":1,"depth":1,"content":{"algorithm":"p.mls.1"}});
            let echo = json!({"event_id":"$local-1","room_id":"!r:s","sender":"@a:s","type":"p.room.message",
                              "origin_server_ts":3,"depth":3,"content":{"body":"my old draft","msgtype":"p.text"}});
            conn.execute_batch(&format!(
                "INSERT INTO events (event_id, room_id, sender, type, state_key, origin_server_ts, depth, payload)
                   VALUES ('$enc','!r:s','@a:s','p.room.encryption','',1,1,'{enc}');
                 INSERT INTO room_state VALUES ('!r:s','p.room.encryption','','$enc',1);
                 INSERT INTO events (event_id, room_id, sender, type, origin_server_ts, depth, payload, decrypted, decrypt_state)
                   VALUES ('$m1','!r:s','@b:s','p.room.encrypted',2,2,
                           '{{\"event_id\":\"$m1\",\"room_id\":\"!r:s\",\"sender\":\"@b:s\",\"type\":\"p.room.encrypted\",\"origin_server_ts\":2,\"depth\":2,\"content\":{{\"ciphertext\":\"CT\"}}}}',
                           'old decrypted text', 1);
                 INSERT INTO events (event_id, room_id, sender, type, origin_server_ts, depth, payload, local)
                   VALUES ('$local-1','!r:s','@a:s','p.room.message',3,3,'{echo}',1);
                 INSERT INTO pending_sends VALUES ('t1','!r:s','my old draft','$local-1',3);"
            ).replace("\\\"", "\""))
            .unwrap();
            assert_eq!(
                conn.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
                    .unwrap(),
                3
            );
        }
        assert!(
            contains(&file_bytes(&path), "old decrypted text"),
            "fixture starts in clear"
        );

        let store = Store::open(&path, TEST_KEY.to_vec()).unwrap();
        let version: i64 = store
            .lock()
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        // Everything still reads back as before…
        let tl = store.timeline("!r:s", 10, None).unwrap();
        assert_eq!(
            tl.iter()
                .find(|e| e.event_id == "$m1")
                .unwrap()
                .decrypted
                .as_deref(),
            Some("old decrypted text")
        );
        assert_eq!(
            tl.iter()
                .find(|e| e.event_id == "$local-1")
                .unwrap()
                .content["body"],
            "my old draft"
        );
        assert_eq!(store.pending_sends().unwrap()[0].body, "my old draft");
        // …but it's sealed at rest, and the old plaintext is gone from the file.
        drop(store);
        let bytes = file_bytes(&path);
        assert!(
            !contains(&bytes, "old decrypted text"),
            "migrated text still on disk"
        );
        assert!(
            !contains(&bytes, "my old draft"),
            "migrated draft still on disk"
        );
    }
}
