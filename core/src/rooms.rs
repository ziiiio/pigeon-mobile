//! Rooms: the FFI surface over the store's room list and the create/join flows
//! (M2.3), plus timeline reads (M2.4) and plaintext send (M2.5).
//!
//! Reads (`list_rooms`, `timeline`) are synchronous — they come straight from
//! the local store (offline-first; no network on the read path). Writes
//! (`create_room`, `join_room`, `invite`, `send_message`) are async HTTP calls;
//! their *effects* land in the store on the next `/sync` and reach the UI through
//! [`crate::sync::SyncObserver`], so the view-models re-read rather than trusting
//! a write's return value for state.

use crate::api::ErrorCode;
use crate::session::PigeonClient;
use crate::store::{RoomSummary, SendState, StoredEvent};
use crate::CoreError;

/// An image attachment on a timeline message (M4.1/M4.2). The UI resolves the
/// bytes via [`PigeonClient::download_image`] (which decrypts when `key` is set).
/// `width`/`height`/`size` are `0` when unknown.
#[derive(Debug, Clone, uniffi::Record)]
pub struct ImageContent {
    /// `pigeon://` content URI of the (possibly encrypted) bytes in the store.
    pub uri: String,
    pub mimetype: String,
    pub width: u32,
    pub height: u32,
    pub size: u64,
    /// For an **encrypted** image (M4.2): the base64 per-file key needed to
    /// decrypt the downloaded bytes. It arrived inside the E2EE'd message, so the
    /// server never saw it. `None` for a plaintext-room image. The UI passes the
    /// whole record to [`PigeonClient::download_image`] — the key never leaves the
    /// core to do the actual decryption (Cardinal Rule).
    pub key: Option<String>,
}

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
    /// An image attachment (M4.1), for a `p.image` message — the UI downloads and
    /// renders it, with `body` as the caption. `None` for non-image events.
    pub image: Option<ImageContent>,
}

impl From<StoredEvent> for TimelineEvent {
    fn from(ev: StoredEvent) -> Self {
        // An image message (`msgtype: p.image`) carries an attachment; `body` is
        // the caption. Two sources: a plaintext-room `p.room.message` (M4.1), or a
        // decrypted `p.room.encrypted` whose plaintext is a `p.image` content JSON
        // (M4.2 — the per-file key rides in `file.key`). `decrypted_image` holds
        // the parsed encrypted-image content when applicable.
        let decrypted_image = if ev.event_type == "p.room.encrypted" {
            ev.decrypted
                .as_deref()
                .and_then(|pt| serde_json::from_str::<serde_json::Value>(pt).ok())
                .filter(|v| v.get("msgtype").and_then(|m| m.as_str()) == Some("p.image"))
        } else {
            None
        };
        let image = if ev.event_type == "p.room.message"
            && ev.content.get("msgtype").and_then(|v| v.as_str()) == Some("p.image")
        {
            parse_image(&ev.content)
        } else {
            decrypted_image.as_ref().and_then(parse_image)
        };
        let body = match ev.event_type.as_str() {
            "p.room.message" => ev
                .content
                .get("body")
                .and_then(|v| v.as_str())
                .map(str::to_owned),
            // A decrypted encrypted image uses its content's caption as the body;
            // a decrypted encrypted *text* message uses the plaintext as-is
            // (handled below where `image` is None).
            "p.room.encrypted" if decrypted_image.is_some() => decrypted_image
                .as_ref()
                .and_then(|v| v.get("body").and_then(|b| b.as_str()))
                .map(str::to_owned),
            // An encrypted event renders as a normal message once decrypted; its
            // plaintext is cached in the store (M3.5, Gotcha #3). Our own sent
            // encrypted messages appear via the plaintext local echo instead
            // (the authoritative ciphertext event dedups by event_id).
            "p.room.encrypted" => ev.decrypted.clone(),
            _ => None,
        };
        // With no body to show: a decryption failure gets an explicit placeholder;
        // an encrypted event still pending decryption stays hidden (the sync loop
        // decrypts before signalling a change, so this is only briefly hit); any
        // other event renders its state/membership system line.
        let system_text = if body.is_some() {
            None
        } else if ev.event_type == "p.room.encrypted" {
            ev.decrypt_failed
                .then(|| "\u{26a0} Unable to decrypt this message".to_owned())
        } else {
            render_system(&ev)
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
            image,
        }
    }
}

/// Extract an [`ImageContent`] from a `p.image` message's content, or `None` if
/// there's no media URL. Handles both shapes: a **plaintext** image carries
/// `url` + an `info` object (M4.1); an **encrypted** image carries a `file`
/// object `{ url, key, mimetype, w, h, size }` (M4.2) — the `key` is the base64
/// per-file decryption key the UI hands back to [`PigeonClient::download_image`].
/// Unknown dims/size default to 0.
fn parse_image(content: &serde_json::Value) -> Option<ImageContent> {
    // Encrypted-image `file` object takes precedence; else the plaintext `url`.
    let file = content.get("file").filter(|f| f.is_object());
    let (holder, key) = match &file {
        // Encrypted: url + key live inside `file`.
        Some(f) => (*f, f.get("key").and_then(|v| v.as_str()).map(str::to_owned)),
        // Plaintext: url at the top level, metadata under `info`.
        None => (content, None),
    };
    let uri = holder.get("url").and_then(|v| v.as_str())?.to_owned();
    // Metadata sits alongside the url for encrypted (`file`), or under `info` for
    // plaintext. Look in `info` first, then the holder itself.
    let info = content.get("info");
    let meta_str = |k: &str| {
        info.and_then(|i| i.get(k))
            .or_else(|| holder.get(k))
            .and_then(|v| v.as_str())
    };
    let meta_u64 = |k: &str| {
        info.and_then(|i| i.get(k))
            .or_else(|| holder.get(k))
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
    };
    Some(ImageContent {
        uri,
        mimetype: meta_str("mimetype")
            .unwrap_or("application/octet-stream")
            .to_owned(),
        width: meta_u64("w") as u32,
        height: meta_u64("h") as u32,
        size: meta_u64("size"),
        key,
    })
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
    /// Create a plaintext room and return its id. Optional `name`/`topic`. The
    /// room appears in `list_rooms` once the running sync loop folds its state in.
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

    /// Create an **encrypted** room and return its id (M3.4). Posts
    /// `createRoom {encryption:true}` (the server materialises the
    /// `p.room.encryption` marker) and creates the MLS group locally (group id =
    /// room id bytes), so we host it and can add members. Requires an MLS identity
    /// for this session — errors if E2EE is unavailable rather than creating an
    /// encrypted room we could never use.
    pub async fn create_encrypted_room(
        &self,
        name: Option<String>,
        topic: Option<String>,
    ) -> Result<String, CoreError> {
        let e2ee = self.e2ee.as_ref().ok_or_else(|| CoreError::Crypto {
            reason: "E2EE is unavailable for this session; cannot create an encrypted room"
                .to_owned(),
        })?;
        let room_id = self
            .api
            .create_room(name.as_deref(), topic.as_deref(), true)
            .await?;
        e2ee.create_group(&room_id)?;
        Ok(room_id)
    }

    /// Upload raw media bytes (M4.1) and return the `pigeon://` content URI. The
    /// caller supplies the `content_type` (e.g. `image/jpeg`). Oversize uploads
    /// are rejected client-side with a typed limit error (the server caps at
    /// 50 MiB). For encrypted rooms the bytes should be client-encrypted first
    /// (M4.2); the server stores whatever it's given, opaquely.
    pub async fn upload_media(
        &self,
        bytes: Vec<u8>,
        content_type: String,
    ) -> Result<String, CoreError> {
        if bytes.len() > crate::media::MAX_UPLOAD_BYTES {
            return Err(CoreError::Api {
                code: ErrorCode::LimitExceeded,
                reason: format!(
                    "file exceeds the {} MiB media upload limit",
                    crate::media::MAX_UPLOAD_BYTES / (1024 * 1024)
                ),
            });
        }
        Ok(self.api.upload_media(bytes, &content_type).await?)
    }

    /// Download media bytes by `pigeon://` content URI (M4.1). Returns the raw
    /// stored bytes — for encrypted media that's ciphertext, so prefer
    /// [`download_image`](PigeonClient::download_image), which decrypts.
    pub async fn download_media(&self, uri: String) -> Result<Vec<u8>, CoreError> {
        let (server, media_id) = crate::media::parse_content_uri(&uri)?;
        Ok(self.api.download_media(&server, &media_id).await?)
    }

    /// Download an image's displayable bytes (M4.1/M4.2): fetch the stored bytes
    /// and, if the image is encrypted (`image.key` set), decrypt them **in the
    /// core** with `pigeon-crypto` (the key never leaves the core to do the AEAD —
    /// Cardinal Rule). Plaintext images are returned as-is.
    pub async fn download_image(&self, image: ImageContent) -> Result<Vec<u8>, CoreError> {
        let bytes = self.download_media(image.uri).await?;
        match image.key {
            Some(key) => self.e2ee()?.decrypt_media(&key, &bytes),
            None => Ok(bytes),
        }
    }

    /// Send an image (M4.1/M4.2). Takes the **raw file bytes** and does everything
    /// in the core: for a **plaintext** room it uploads the bytes and sends a
    /// `p.image` message referencing the `pigeon://` URL; for an **encrypted**
    /// room it encrypts the bytes under a fresh per-file key, uploads the
    /// *ciphertext*, and sends a `p.image` content — carrying the URL **and the
    /// per-file key** — inside an E2EE'd `p.room.encrypted` message, so the server
    /// only ever stores ciphertext (Gotcha #1). `caption` is optional display text.
    /// Oversize files are rejected client-side (typed limit error).
    pub async fn send_image(
        &self,
        room_id: String,
        bytes: Vec<u8>,
        mimetype: String,
        width: u32,
        height: u32,
        caption: String,
    ) -> Result<(), CoreError> {
        let size = bytes.len() as u64;
        // Encrypt-and-send iff this is a room whose MLS group we hold.
        let encrypt = match self.e2ee.as_ref() {
            Some(e2ee) => e2ee.has_group(&room_id)?,
            None => false,
        };

        if encrypt {
            let e2ee = self.e2ee()?;
            let (key_b64, ciphertext) = e2ee.encrypt_media(&bytes)?;
            // Upload the ciphertext (opaque); its content-type is generic.
            let uri = self
                .upload_media(ciphertext, "application/octet-stream".to_owned())
                .await?;
            // The p.image content carries the URL + the per-file key in a `file`
            // object; this whole content is then MLS-encrypted, so the key is only
            // visible to the room's members.
            let content = serde_json::json!({
                "msgtype": "p.image",
                "body": caption,
                "file": {
                    "url": uri,
                    "key": key_b64,
                    "mimetype": mimetype,
                    "w": width,
                    "h": height,
                    "size": size,
                },
            });
            let ciphertext_b64 = e2ee.encrypt(&room_id, &content.to_string())?;
            let event = serde_json::json!({ "algorithm": "p.mls.1", "ciphertext": ciphertext_b64 });
            self.api
                .send_event(&room_id, "p.room.encrypted", &next_txn_id(), &event)
                .await?;
        } else {
            let uri = self.upload_media(bytes, mimetype.clone()).await?;
            let content = serde_json::json!({
                "msgtype": "p.image",
                "body": caption,
                "url": uri,
                "info": { "mimetype": mimetype, "w": width, "h": height, "size": size },
            });
            self.api
                .send_event(&room_id, "p.room.message", &next_txn_id(), &content)
                .await?;
        }
        Ok(())
    }

    /// Join a room by id. The membership + timeline arrive on the next sync.
    /// This is also how an invitee **accepts** an invite (M2.6): they join by the
    /// room's id. (The server exposes no invite list in `/sync`, so a pending
    /// invite is learned out-of-band; and there is no client leave/decline
    /// endpoint yet — declining is blocked server-side, not implemented here.)
    pub async fn join_room(&self, room_id: String) -> Result<(), CoreError> {
        self.api.join_room(&room_id).await?;
        Ok(())
    }

    /// Invite `user_id` to `room_id` (M2.6). The invite is a `p.room.member`
    /// (membership `invite`) event; it renders in the room's timeline via the
    /// core's system-line rendering, and the invitee accepts by joining the id.
    ///
    /// **Encrypted rooms (M3.4):** if we host the room's MLS group, this also runs
    /// the group-membership dance so the invitee can decrypt — mirroring the
    /// reference CLI's ordering: server invite first (that's what lets them
    /// receive the timeline), then claim a KeyPackage from each of the invitee's
    /// devices, `add_member`, and ship each device its `Welcome` over
    /// `/sendToDevice` (`p.mls.welcome`). Plaintext rooms skip all of this.
    pub async fn invite(&self, room_id: String, user_id: String) -> Result<(), CoreError> {
        // Room membership first — this is what lets the invitee receive the
        // timeline (and, later, the to-device Welcome).
        self.api.invite(&room_id, &user_id).await?;

        // If this is an encrypted room we host, add the invitee to the group.
        if let Some(e2ee) = self.e2ee.as_ref() {
            if e2ee.has_group(&room_id)? {
                self.welcome_to_group(e2ee, &room_id, &user_id).await?;
            }
        }
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
        let inserted = self.store.apply_events(&chunk)?;
        // Process any inbound MLS events this backfill just stored (M3.5 + C1):
        // apply commits and decrypt messages, so the opened timeline shows
        // plaintext rather than pending ciphertext.
        self.process_inbound_mls()?;
        Ok(inserted as u32)
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
            // Encrypt for a room whose MLS group we hold (M3.5): the message goes
            // out as `p.room.encrypted` ciphertext; the server only ever sees the
            // ciphertext. Plaintext rooms send `p.room.message`. This is
            // transparent — the caller (send_message) doesn't branch on it.
            let (event_type, content) = match self.encrypt_for_send(&send.room_id, &send.body) {
                Ok(Some(ciphertext)) => (
                    "p.room.encrypted",
                    serde_json::json!({ "algorithm": "p.mls.1", "ciphertext": ciphertext }),
                ),
                Ok(None) => {
                    // No MLS group for this room. If the room is *encrypted* we
                    // must NOT downgrade to plaintext — leave the send queued so
                    // it goes out encrypted once the group's Welcome arrives (a
                    // later sync). Deciding on group-presence alone (the old
                    // behaviour) leaked the body into an E2EE room when we were
                    // joined but hadn't processed the Welcome yet. Never logs the
                    // body (Gotcha #2). Only a genuinely plaintext room falls
                    // through to `p.room.message`.
                    if self.store.is_room_encrypted(&send.room_id)? {
                        crate::emit(
                            crate::LogLevel::Warn,
                            "e2ee",
                            &format!(
                                "holding a queued message for encrypted room {} — \
                                 no MLS group yet; will send once the group is joined",
                                send.room_id
                            ),
                        );
                        continue;
                    }
                    (
                        "p.room.message",
                        serde_json::json!({ "body": send.body, "msgtype": "p.text" }),
                    )
                }
                // Encryption failed (shouldn't happen if we hold the group): fail
                // this send terminally so it doesn't wedge the queue, and move on.
                Err(err) => {
                    crate::emit(
                        crate::LogLevel::Warn,
                        "e2ee",
                        &format!("could not encrypt outbound message; marking failed: {err}"),
                    );
                    self.store.fail_send(&send.txn_id)?;
                    changed = true;
                    continue;
                }
            };
            match self
                .api
                .send_event(&send.room_id, event_type, &send.txn_id, &content)
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

impl PigeonClient {
    /// If `room_id` is an encrypted room we hold the group for, encrypt `body` and
    /// return the base64 ciphertext; `None` for a plaintext room. Used by
    /// [`flush_pending`](PigeonClient::flush_pending) on the send path (M3.5).
    fn encrypt_for_send(&self, room_id: &str, body: &str) -> Result<Option<String>, CoreError> {
        let Some(e2ee) = self.e2ee.as_ref() else {
            return Ok(None);
        };
        if e2ee.has_group(room_id)? {
            Ok(Some(e2ee.encrypt(room_id, body)?))
        } else {
            Ok(None)
        }
    }

    /// Process every not-yet-handled inbound MLS event in the store — apply
    /// `p.mls.commit` group changes and decrypt `p.room.encrypted` messages — in
    /// one DAG-ordered, exactly-once pass (M3.5 + finding C1). Both advance/depend
    /// on the MLS ratchet, which is order-sensitive (Gotcha #3): a commit that
    /// changes the epoch is applied *before* the encrypted messages that follow it,
    /// so they share one ordering (see [`crate::store::Store::pending_mls`]).
    ///
    /// - **Commit** — skip our **own** (we self-merged on add); otherwise apply it.
    ///   A commit that doesn't apply to our current epoch (the one that *added us*,
    ///   or a replay) errors harmlessly and is marked processed, never retried. A
    ///   room we don't hold the group for yet leaves the commit pending.
    /// - **Message** — decrypt and cache the plaintext; a room whose group we don't
    ///   hold *yet* is left pending (its Welcome may arrive on a later sync); a
    ///   genuine failure (tampered / wrong epoch / not a member) is marked
    ///   terminally undecryptable.
    ///
    /// Returns whether anything changed (so the caller can refresh). Never logs
    /// plaintext or key material (Gotcha #2).
    pub(crate) fn process_inbound_mls(&self) -> Result<bool, CoreError> {
        let Some(e2ee) = self.e2ee.as_ref() else {
            return Ok(false);
        };
        let self_user = self.session().user_id;
        let mut changed = false;
        for pending in self.store.pending_mls()? {
            match pending {
                crate::store::PendingMls::Commit(commit) => {
                    // No group yet — leave it pending; a Welcome may still arrive
                    // (then a later pass applies this, if it's still ahead of us).
                    if !e2ee.has_group(&commit.room_id)? {
                        continue;
                    }
                    // Our own commit is already self-merged; consume it without
                    // re-applying (which would error).
                    if commit.sender != self_user {
                        if let Err(err) = e2ee.process_commit(&commit.room_id, &commit.commit_b64) {
                            // Expected when the commit doesn't advance *us* — the
                            // commit that added us (we're already at its epoch via
                            // the Welcome), or a replay. Not fatal: consume it.
                            crate::emit(
                                crate::LogLevel::Info,
                                "e2ee",
                                &format!("commit {} not applied (skipped): {err}", commit.event_id),
                            );
                        }
                    }
                    self.store.set_commit_processed(&commit.event_id)?;
                    changed = true;
                }
                crate::store::PendingMls::Message(msg) => {
                    // No group yet — leave it pending; a Welcome may still arrive.
                    if !e2ee.has_group(&msg.room_id)? {
                        continue;
                    }
                    match e2ee.decrypt(&msg.room_id, &msg.ciphertext_b64) {
                        Ok(plaintext) => {
                            self.store.set_decrypted(&msg.event_id, &plaintext)?;
                            changed = true;
                        }
                        Err(err) => {
                            crate::emit(
                                crate::LogLevel::Warn,
                                "e2ee",
                                &format!("failed to decrypt {}: {err}", msg.event_id),
                            );
                            self.store.set_decrypt_failed(&msg.event_id)?;
                            changed = true;
                        }
                    }
                }
            }
        }
        Ok(changed)
    }
}

impl PigeonClient {
    /// Add `user_id`'s devices to `room_id`'s MLS group (M3.4), delivering to each
    /// the `Welcome` and broadcasting the resulting `commit` to the existing
    /// members (finding C1). Per device: claim a KeyPackage, `add_member` (which
    /// self-merges our commit and yields the Welcome + commit), then
    /// 1. ship `p.mls.welcome` `{welcome, room_id}` to *that device* over
    ///    `/sendToDevice` so it can join, and
    /// 2. broadcast `p.mls.commit` `{commit}` as a room event so the *earlier*
    ///    members advance to the new epoch and keep decrypting.
    ///
    /// Without step 2 a third+ member's addition would strand the earlier members a
    /// ratchet epoch behind. The commit rides the same `/send` route as any room
    /// event; the invitee (already at the post-commit epoch via the Welcome) and we
    /// (self-merged) both skip it on receipt. Never logs key material (Gotcha #2).
    async fn welcome_to_group(
        &self,
        e2ee: &crate::e2ee::E2ee,
        room_id: &str,
        user_id: &str,
    ) -> Result<(), CoreError> {
        let claimed = crate::keys::claim_all_devices(&self.api, user_id).await?;
        for kp in claimed {
            let outcome = e2ee.add_member(room_id, &kp.key_package_b64)?;
            let messages = serde_json::json!({
                user_id: { kp.device_id: { "welcome": outcome.welcome, "room_id": room_id } }
            });
            self.api
                .send_to_device("p.mls.welcome", &next_txn_id(), &messages)
                .await?;
            // Broadcast the commit to existing members (finding C1). Each device
            // added advances the epoch and yields its own commit, sent in order.
            let commit = serde_json::json!({ "commit": outcome.commit });
            self.api
                .send_event(room_id, "p.mls.commit", &next_txn_id(), &commit)
                .await?;
        }
        Ok(())
    }
}

/// A unique client transaction id for a to-device send. The server ignores it
/// (it just identifies the attempt), so a process-local counter suffices.
fn next_txn_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    format!("mob-td-{}", COUNTER.fetch_add(1, Ordering::Relaxed))
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
            decrypted: None,
            decrypt_failed: false,
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

    // --- Image messages (M4.1) -----------------------------------------------

    #[test]
    fn image_message_populates_image_content_and_caption() {
        let ev = stored(
            "p.room.message",
            None,
            json!({
                "msgtype": "p.image", "body": "cat.jpg", "url": "pigeon://s/abc",
                "info": { "mimetype": "image/jpeg", "w": 800, "h": 600, "size": 12345 }
            }),
        );
        let te = TimelineEvent::from(ev);
        assert_eq!(te.body.as_deref(), Some("cat.jpg")); // caption
        let img = te.image.expect("an image attachment");
        assert_eq!(img.uri, "pigeon://s/abc");
        assert_eq!(img.mimetype, "image/jpeg");
        assert_eq!(img.width, 800);
        assert_eq!(img.height, 600);
        assert_eq!(img.size, 12345);
    }

    #[test]
    fn plain_text_message_has_no_image() {
        let te = TimelineEvent::from(stored(
            "p.room.message",
            None,
            json!({ "body": "hi", "msgtype": "p.text" }),
        ));
        assert!(te.image.is_none());
        assert_eq!(te.body.as_deref(), Some("hi"));
    }

    #[test]
    fn decrypted_encrypted_image_renders_with_its_file_key() {
        // A p.room.encrypted event whose decrypted plaintext is a p.image content
        // with a `file` object → an image attachment carrying the per-file key.
        let mut ev = stored(
            "p.room.encrypted",
            None,
            json!({ "algorithm": "p.mls.1", "ciphertext": "CT" }),
        );
        ev.decrypted = Some(
            json!({
                "msgtype": "p.image", "body": "secret.png",
                "file": { "url": "pigeon://s/ct1", "key": "a2V5", "mimetype": "image/png", "w": 4, "h": 3, "size": 9 }
            })
            .to_string(),
        );
        let te = TimelineEvent::from(ev);
        assert_eq!(te.body.as_deref(), Some("secret.png")); // caption
        let img = te.image.expect("an image attachment");
        assert_eq!(img.uri, "pigeon://s/ct1");
        assert_eq!(img.key.as_deref(), Some("a2V5")); // the per-file decryption key
        assert_eq!(img.mimetype, "image/png");
        assert_eq!(img.width, 4);
    }

    #[test]
    fn decrypted_encrypted_text_is_not_mistaken_for_an_image() {
        // A decrypted text message stays a plain body (no `image`), even though the
        // encrypted-image detection parses the plaintext as JSON.
        let mut ev = stored(
            "p.room.encrypted",
            None,
            json!({ "algorithm": "p.mls.1", "ciphertext": "CT" }),
        );
        ev.decrypted = Some("just a normal message".to_owned());
        let te = TimelineEvent::from(ev);
        assert!(te.image.is_none());
        assert_eq!(te.body.as_deref(), Some("just a normal message"));
    }

    // Full encrypted-image path: send into an encrypted room (encrypt+upload+send
    // via mock HTTP), then confirm the round-trip through decrypt+download. Uses
    // two real MLS engines so the crypto is genuine, not mocked.
    #[tokio::test]
    #[serial_test::serial]
    async fn encrypted_image_round_trips_send_to_display() {
        use crate::e2ee::E2ee;
        use wiremock::matchers::{method, path, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/_pigeon/client/v1/login"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "user_id": "@alice:s", "device_id": "D", "access_token": "tok"
            })))
            .mount(&server)
            .await;
        let alice = crate::session::login(server.uri(), "alice".into(), "p".into())
            .await
            .unwrap();

        // Alice hosts an encrypted group; Bob joins so he can decrypt.
        alice.e2ee().unwrap().create_group("!enc:s").unwrap();
        let bob = E2ee::create("@bob:s").unwrap();
        let bob_kp = bob.key_packages(1).unwrap().remove(0);
        let welcome = alice
            .e2ee()
            .unwrap()
            .add_member("!enc:s", &bob_kp)
            .unwrap()
            .welcome;
        bob.join_from_welcome(&welcome).unwrap();

        // Capture the ciphertext the upload receives so we can serve it back.
        Mock::given(method("POST"))
            .and(path("/_pigeon/media/v1/upload"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "content_uri": "pigeon://s/ct1" })),
            )
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path_regex(r".*/send/p\.room\.encrypted/.+$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "event_id": "$e" })))
            .mount(&server)
            .await;

        let photo = b"\x89PNG the actual pixels".to_vec();
        alice
            .send_image(
                "!enc:s".into(),
                photo.clone(),
                "image/png".into(),
                4,
                3,
                "pic".into(),
            )
            .await
            .expect("send encrypted image");

        // Pull the encrypted event + the uploaded ciphertext back out of the mock.
        let reqs = server.received_requests().await.unwrap();
        let uploaded = reqs
            .iter()
            .find(|r| r.url.path() == "/_pigeon/media/v1/upload")
            .unwrap()
            .body
            .clone();
        let event_body: serde_json::Value = serde_json::from_slice(
            &reqs
                .iter()
                .find(|r| r.url.path().contains("/send/p.room.encrypted/"))
                .unwrap()
                .body,
        )
        .unwrap();
        let ciphertext_b64 = event_body["ciphertext"].as_str().unwrap();

        // Bob decrypts the message → a p.image content with url + per-file key.
        let plaintext = bob.decrypt("!enc:s", ciphertext_b64).unwrap();
        let content: serde_json::Value = serde_json::from_str(&plaintext).unwrap();
        assert_eq!(content["msgtype"], "p.image");
        let img = parse_image(&content).expect("image content");
        assert_eq!(img.uri, "pigeon://s/ct1");
        let file_key = img.key.expect("encrypted image carries a file key");

        // Bob decrypts the uploaded ciphertext with that key → the original photo.
        let recovered = bob.decrypt_media(&file_key, &uploaded).unwrap();
        assert_eq!(recovered, photo, "the image round-trips through E2EE media");
    }

    // Inviting into an encrypted room we host broadcasts the MLS commit as a
    // `p.mls.commit` room event (finding C1) in addition to the per-device
    // Welcome, so existing members can advance to the new epoch. Uses a real MLS
    // KeyPackage so the Welcome + commit are genuine.
    #[tokio::test]
    #[serial_test::serial]
    async fn invite_into_encrypted_room_broadcasts_the_commit() {
        use crate::e2ee::E2ee;
        use wiremock::matchers::{method, path, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/_pigeon/client/v1/login"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "user_id": "@alice:s", "device_id": "D", "access_token": "tok"
            })))
            .mount(&server)
            .await;
        let alice = crate::session::login(server.uri(), "alice".into(), "p".into())
            .await
            .unwrap();
        alice.e2ee().unwrap().create_group("!enc:s").unwrap();

        // Bob is a real second device with a published KeyPackage to claim.
        let bob = E2ee::create("@bob:s").unwrap();
        let bob_kp = bob.key_packages(1).unwrap().remove(0);

        Mock::given(method("POST"))
            .and(path("/_pigeon/client/v1/rooms/!enc:s/invite"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "event_id": "$inv" })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/_pigeon/client/v1/keys/query"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "device_keys": { "@bob:s": {
                    "D2": { "user_id": "@bob:s", "device_id": "D2",
                            "algorithms": ["p.mls.1"], "keys": {}, "signatures": {} }
                } }
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/_pigeon/client/v1/keys/claim"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "one_time_keys": { "@bob:s": { "D2": { "key_id": "kp-x", "package": bob_kp } } }
            })))
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path_regex(r".*/sendToDevice/p\.mls\.welcome/.+$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path_regex(r".*/send/p\.mls\.commit/.+$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "event_id": "$c" })))
            .mount(&server)
            .await;

        alice
            .invite("!enc:s".into(), "@bob:s".into())
            .await
            .expect("invite with Welcome + commit");

        let reqs = server.received_requests().await.unwrap();
        // The commit was broadcast as a room event with a non-empty base64 commit.
        let commit_body: serde_json::Value = serde_json::from_slice(
            &reqs
                .iter()
                .find(|r| r.url.path().contains("/send/p.mls.commit/"))
                .expect("a p.mls.commit was broadcast")
                .body,
        )
        .unwrap();
        assert!(!commit_body["commit"].as_str().unwrap().is_empty());

        // The Welcome shipped to Bob's device is a genuine, joinable Welcome.
        let welcome_body: serde_json::Value = serde_json::from_slice(
            &reqs
                .iter()
                .find(|r| r.url.path().contains("/sendToDevice/p.mls.welcome/"))
                .expect("a Welcome was sent to-device")
                .body,
        )
        .unwrap();
        let welcome = welcome_body["messages"]["@bob:s"]["D2"]["welcome"]
            .as_str()
            .unwrap();
        bob.join_from_welcome(welcome).unwrap();
        assert!(bob.has_group("!enc:s").unwrap());
    }

    // --- Encrypted event rendering (M3.5) ------------------------------------

    #[test]
    fn encrypted_event_renders_decrypted_plaintext_as_a_message() {
        let mut ev = stored(
            "p.room.encrypted",
            None,
            json!({ "algorithm": "p.mls.1", "ciphertext": "CT" }),
        );
        ev.decrypted = Some("the secret".to_owned());
        let te = TimelineEvent::from(ev);
        assert_eq!(te.body.as_deref(), Some("the secret"));
        assert_eq!(te.system_text, None);
    }

    #[test]
    fn encrypted_event_pending_decrypt_is_hidden() {
        // Not yet decrypted (transient — the sync loop decrypts before signalling):
        // no body, no placeholder, so it doesn't flicker.
        let ev = stored(
            "p.room.encrypted",
            None,
            json!({ "algorithm": "p.mls.1", "ciphertext": "CT" }),
        );
        let te = TimelineEvent::from(ev);
        assert_eq!(te.body, None);
        assert_eq!(te.system_text, None);
    }

    #[test]
    fn encrypted_event_decrypt_failure_shows_placeholder() {
        let mut ev = stored(
            "p.room.encrypted",
            None,
            json!({ "algorithm": "p.mls.1", "ciphertext": "CT" }),
        );
        ev.decrypt_failed = true;
        let te = TimelineEvent::from(ev);
        assert_eq!(te.body, None);
        assert!(te.system_text.unwrap().contains("Unable to decrypt"));
    }

    // A full receive path: an encrypted event lands in the store, the decrypt pass
    // caches its plaintext, and the timeline then renders it. Uses two real MLS
    // engines (alice sends, bob is the client) through a wiremock-backed login.
    #[tokio::test]
    #[serial_test::serial]
    async fn process_inbound_mls_decrypts_inbound_and_caches_plaintext() {
        use crate::e2ee::E2ee;

        let bob = login_client().await;
        let alice = E2ee::create("@alice:s").unwrap();
        let bob_kp = bob
            .e2ee
            .as_ref()
            .unwrap()
            .key_packages(1)
            .unwrap()
            .remove(0);
        alice.create_group("!enc:s").unwrap();
        let welcome = alice.add_member("!enc:s", &bob_kp).unwrap().welcome;
        bob.e2ee
            .as_ref()
            .unwrap()
            .join_from_welcome(&welcome)
            .unwrap();

        // Alice encrypts a message; it arrives in bob's store as p.room.encrypted.
        let ct = alice.encrypt("!enc:s", "secret hi").unwrap();
        bob.store
            .apply_events(&[json!({
                "event_id": "$e1", "room_id": "!enc:s", "sender": "@alice:s",
                "type": "p.room.encrypted", "origin_server_ts": 100, "depth": 1,
                "content": { "algorithm": "p.mls.1", "ciphertext": ct }
            })])
            .unwrap();

        // Before decrypt: hidden (pending). After: renders the plaintext body.
        assert_eq!(
            bob.timeline("!enc:s".into(), 10, None).unwrap()[0].body,
            None
        );
        assert!(bob.process_inbound_mls().unwrap());
        let tl = bob.timeline("!enc:s".into(), 10, None).unwrap();
        assert_eq!(tl[0].body.as_deref(), Some("secret hi"));
        // Idempotent: the ratchet already advanced, so no re-decrypt (Gotcha #3).
        assert!(!bob.process_inbound_mls().unwrap());
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn process_inbound_mls_marks_undecryptable_events_failed() {
        use crate::e2ee::E2ee;

        // Bob holds a group but is fed a ciphertext he can't decrypt (garbage).
        let bob = login_client().await;
        let alice = E2ee::create("@alice:s").unwrap();
        let bob_kp = bob
            .e2ee
            .as_ref()
            .unwrap()
            .key_packages(1)
            .unwrap()
            .remove(0);
        alice.create_group("!enc:s").unwrap();
        let welcome = alice.add_member("!enc:s", &bob_kp).unwrap().welcome;
        bob.e2ee
            .as_ref()
            .unwrap()
            .join_from_welcome(&welcome)
            .unwrap();

        bob.store
            .apply_events(&[json!({
                "event_id": "$bad", "room_id": "!enc:s", "sender": "@alice:s",
                "type": "p.room.encrypted", "origin_server_ts": 100, "depth": 1,
                "content": { "algorithm": "p.mls.1", "ciphertext": "AAAA" }
            })])
            .unwrap();

        assert!(bob.process_inbound_mls().unwrap());
        let tl = bob.timeline("!enc:s".into(), 10, None).unwrap();
        assert_eq!(tl[0].body, None);
        // Rendered as the unable-to-decrypt placeholder (terminal failure).
        assert!(tl[0]
            .system_text
            .as_deref()
            .unwrap()
            .contains("Unable to decrypt"));
    }

    /// Build a logged-in client via a wiremock server that only backs `/login`.
    /// (Login also mints the client's MLS engine; no key store is installed here,
    /// so its persistence is a harmless no-op.)
    async fn login_client() -> std::sync::Arc<PigeonClient> {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/_pigeon/client/v1/login"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "user_id": "@bob:s", "device_id": "D", "access_token": "tok"
            })))
            .mount(&server)
            .await;
        // These tests drive the store/engine directly and make no HTTP calls after
        // login, so it's fine for the mock server to drop when this returns.
        crate::session::login(server.uri(), "bob".into(), "p".into())
            .await
            .unwrap()
    }
}
