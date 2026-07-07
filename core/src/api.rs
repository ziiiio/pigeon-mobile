//! The Client–Server API HTTP client (M1.1).
//!
//! A thin `reqwest` (rustls) wrapper around `/_pigeon/client/v1/*`: it owns the
//! homeserver base URL, injects the bearer token, and maps the server's `P_*`
//! error bodies into a typed [`ApiError`]. It mirrors the reference CLI's
//! `api.rs` (`../../pigeon/clients/cli/src/api.rs`) — the canonical call
//! sequence for every flow this app needs. **Read that file before adding an
//! endpoint here.**
//!
//! Scope: M1.1 provides the transport primitives (`get`/`post`/`put`) and the
//! error mapping only. Named endpoints (`register`, `login`, `whoami`, …) are
//! thin wrappers added in M1.2, and the FFI-visible surface (mapping [`ApiError`]
//! → `CoreError`) lands with them.
//!
//! **Server discovery is deferred.** `.well-known/pigeon/server` resolution is
//! out of scope for M1.1 — the caller passes a full homeserver base URL. Revisit
//! when a real "pick your homeserver" UI needs it (documented in ROADMAP M1.1).

use std::time::Duration;

use reqwest::{Client, Method, RequestBuilder};
use serde_json::{json, Value};

/// A typed Pigeon error code — the server's `P_*` set
/// (`../../pigeon/crates/client-api/src/error.rs`).
///
/// Match on the stable `errcode`, never on the human `error` text (CLAUDE.md).
/// `Other` keeps the client forward-compatible: the wire contract may add codes
/// on a server version bump, and an unknown code must degrade gracefully rather
/// than panic — it carries the raw string so the UI can still show *something*.
///
/// Exposed over the FFI (`uniffi::Enum`) so the native UI can branch on the code
/// (e.g. show a "username taken" message for `UserInUse`) — the typed-error rule
/// (CLAUDE.md).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum ErrorCode {
    Forbidden,
    UnknownToken,
    MissingToken,
    BadJson,
    NotJson,
    NotFound,
    LimitExceeded,
    BadSignature,
    Unrecognized,
    UserInUse,
    InvalidUsername,
    Unknown,
    /// A code this client build doesn't recognise (newer server). Named field so
    /// it survives the `uniffi::Enum` mapping (tuple variants are avoided).
    Other {
        code: String,
    },
}

impl ErrorCode {
    /// Map a wire `errcode` string to a typed code.
    pub fn from_wire(code: &str) -> Self {
        match code {
            "P_FORBIDDEN" => Self::Forbidden,
            "P_UNKNOWN_TOKEN" => Self::UnknownToken,
            "P_MISSING_TOKEN" => Self::MissingToken,
            "P_BAD_JSON" => Self::BadJson,
            "P_NOT_JSON" => Self::NotJson,
            "P_NOT_FOUND" => Self::NotFound,
            "P_LIMIT_EXCEEDED" => Self::LimitExceeded,
            "P_BAD_SIGNATURE" => Self::BadSignature,
            "P_UNRECOGNIZED" => Self::Unrecognized,
            "P_USER_IN_USE" => Self::UserInUse,
            "P_INVALID_USERNAME" => Self::InvalidUsername,
            "P_UNKNOWN" => Self::Unknown,
            other => Self::Other {
                code: other.to_owned(),
            },
        }
    }

    /// The wire `errcode` string for this code.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Forbidden => "P_FORBIDDEN",
            Self::UnknownToken => "P_UNKNOWN_TOKEN",
            Self::MissingToken => "P_MISSING_TOKEN",
            Self::BadJson => "P_BAD_JSON",
            Self::NotJson => "P_NOT_JSON",
            Self::NotFound => "P_NOT_FOUND",
            Self::LimitExceeded => "P_LIMIT_EXCEEDED",
            Self::BadSignature => "P_BAD_SIGNATURE",
            Self::Unrecognized => "P_UNRECOGNIZED",
            Self::UserInUse => "P_USER_IN_USE",
            Self::InvalidUsername => "P_INVALID_USERNAME",
            Self::Unknown => "P_UNKNOWN",
            Self::Other { code } => code,
        }
    }
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A Client–Server API failure. M1.2 maps this into the FFI-visible `CoreError`
/// so the UI can branch on the typed code (e.g. show a "username taken" message
/// for [`ErrorCode::UserInUse`]).
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// The server returned a structured `P_*` error with an HTTP status.
    #[error("server error {status} {code}: {message}")]
    Server {
        status: u16,
        code: ErrorCode,
        message: String,
    },
    /// Transport failure — DNS/TLS/connection/timeout, i.e. no HTTP response.
    /// Offline-first callers treat this as retryable (CLAUDE.md).
    #[error("network error: {reason}")]
    Network { reason: String },
    /// A response whose body wasn't the JSON shape we expected.
    #[error("malformed response: {reason}")]
    Malformed { reason: String },
}

/// The server's response to `register`/`login` — the raw `AuthResponse`
/// (`../../pigeon/crates/client-api/src/handlers/auth.rs`). Holds the
/// `access_token`, so it stays *inside* the core (Gotcha #1) — `session.rs`
/// keeps the token and hands the UI only the non-secret identity.
#[derive(Debug, Clone)]
pub struct AuthResponse {
    pub user_id: String,
    pub device_id: String,
    pub access_token: String,
}

/// The Client–Server API client: one per session, reused across requests
/// (reqwest pools connections — do not build per-call).
pub struct Api {
    client: Client,
    base: String,
    token: Option<String>,
}

impl Api {
    /// Build a client for `base` (a full homeserver URL, e.g.
    /// `https://pigeon.example`). `token` is the bearer token, if already known.
    pub fn new(base: impl Into<String>, token: Option<String>) -> Result<Self, ApiError> {
        // rustls-tls to match the server's client stack (CLAUDE.md). A
        // connect_timeout guards against dead hosts without capping request
        // duration — the `/sync` long-poll must be allowed to run for a while,
        // so per-request timeouts are the caller's job, not a global one here.
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| ApiError::Network {
                reason: e.to_string(),
            })?;
        Ok(Self {
            client,
            base: normalize_base(&base.into()),
            token,
        })
    }

    /// Replace the bearer token (e.g. after login, or clear it on logout).
    pub fn set_token(&mut self, token: Option<String>) {
        self.token = token;
    }

    /// GET `path`, returning the parsed JSON body on 2xx.
    pub async fn get(&self, path: &str) -> Result<Value, ApiError> {
        self.send(self.req(Method::GET, path)).await
    }

    /// POST `body` as JSON to `path`.
    pub async fn post(&self, path: &str, body: &Value) -> Result<Value, ApiError> {
        self.send(self.req(Method::POST, path).json(body)).await
    }

    /// PUT `body` as JSON to `path` (used for `/send/...`, `/sendToDevice/...`).
    pub async fn put(&self, path: &str, body: &Value) -> Result<Value, ApiError> {
        self.send(self.req(Method::PUT, path).json(body)).await
    }

    /// Build a request with the base URL prefixed and the bearer token attached.
    fn req(&self, method: Method, path: &str) -> RequestBuilder {
        let mut b = self.client.request(method, join_url(&self.base, path));
        if let Some(t) = &self.token {
            b = b.bearer_auth(t);
        }
        b
    }

    // --- Named endpoints (M1.2) ---------------------------------------------
    // Thin wrappers over the verb helpers, mirroring the reference CLI's call
    // sequence exactly (../../pigeon/clients/cli/src/api.rs).

    /// `POST /register` → a new account + first session.
    pub async fn register(&self, username: &str, password: &str) -> Result<AuthResponse, ApiError> {
        let body = self
            .post(
                "/_pigeon/client/v1/register",
                &json!({ "username": username, "password": password }),
            )
            .await?;
        parse_auth(&body)
    }

    /// `POST /login` (password flow) → a session for an existing account.
    /// `user` may be a bare localpart or a full `@user:server` id.
    pub async fn login(&self, user: &str, password: &str) -> Result<AuthResponse, ApiError> {
        let body = self
            .post(
                "/_pigeon/client/v1/login",
                &json!({ "type": "p.login.password", "user": user, "password": password }),
            )
            .await?;
        parse_auth(&body)
    }

    /// `POST /logout` → revoke the current bearer token server-side. (The FFI
    /// logout surface is M1.5; this is just the HTTP primitive.)
    pub async fn logout(&self) -> Result<(), ApiError> {
        self.post("/_pigeon/client/v1/logout", &json!({})).await?;
        Ok(())
    }

    /// `GET /account/whoami` → `{ user_id, device_id }`. Used to validate a
    /// restored token on launch (M1.3).
    pub async fn whoami(&self) -> Result<Value, ApiError> {
        self.get("/_pigeon/client/v1/account/whoami").await
    }

    // --- Sync + rooms (M2) ---------------------------------------------------
    // Mirror the reference CLI's call sequence
    // (../../pigeon/clients/cli/src/api.rs). Sync tokens are opaque composites —
    // passed through verbatim, never parsed (CLAUDE.md Gotcha #5).

    /// `GET /sync` — the long-poll. `since` is the opaque token from a prior
    /// sync (omit on first sync); `timeout_ms` is how long the server may hold
    /// the request open waiting for events (0 = return immediately); `limit`
    /// caps events per room. Returns the raw sync JSON (`next_batch` + `rooms`).
    ///
    /// A request timeout is set just above the poll window so a wedged connection
    /// can't hang the loop forever, while still letting the long-poll run its
    /// course (the base client has no global request timeout — that's deliberate
    /// for exactly this call).
    pub async fn sync(
        &self,
        since: Option<&str>,
        timeout_ms: u64,
        limit: u32,
    ) -> Result<Value, ApiError> {
        let timeout = timeout_ms.to_string();
        let limit = limit.to_string();
        let mut query: Vec<(&str, &str)> = vec![("timeout", &timeout), ("limit", &limit)];
        if let Some(s) = since {
            query.push(("since", s));
        }
        let req = self
            .req(Method::GET, "/_pigeon/client/v1/sync")
            .query(&query)
            .timeout(Duration::from_millis(timeout_ms) + Duration::from_secs(15));
        self.send(req).await
    }

    /// `POST /createRoom` → the new room's id. Optional `name`/`topic`; set
    /// `encryption` to create an E2EE room (unused until M3 — plaintext for M2).
    pub async fn create_room(
        &self,
        name: Option<&str>,
        topic: Option<&str>,
        encryption: bool,
    ) -> Result<String, ApiError> {
        let mut body = json!({});
        if let Some(n) = name {
            body["name"] = json!(n);
        }
        if let Some(t) = topic {
            body["topic"] = json!(t);
        }
        if encryption {
            body["encryption"] = json!(true);
        }
        let resp = self.post("/_pigeon/client/v1/createRoom", &body).await?;
        json_string(&resp, "room_id")
    }

    /// `POST /rooms/{room_id}/join` → the joined room's id. Body is empty (the
    /// room id is in the path), mirroring the reference CLI.
    pub async fn join_room(&self, room_id: &str) -> Result<String, ApiError> {
        let path = format!("/_pigeon/client/v1/rooms/{room_id}/join");
        let resp = self.post(&path, &json!({})).await?;
        json_string(&resp, "room_id")
    }

    // --- E2EE key directory (M3) --------------------------------------------
    // Mirror the reference CLI (../../pigeon/clients/cli/src/api.rs). All key
    // material is opaque base64 the server stores/forwards verbatim (it's an MLS
    // delivery service — it never parses MLS bytes). `user_id`/`device_id` are
    // stamped server-side from the token, so they're absent from the body.

    /// `POST /keys/upload` — publish this device's ed25519 identity key and a
    /// pool of base64 KeyPackages (M3.1). Returns the device's remaining
    /// KeyPackage count (each peer that adds us to a group claims one).
    pub async fn upload_keys(
        &self,
        device_id: &str,
        public_key_b64: &str,
        key_packages_b64: &[String],
    ) -> Result<u32, ApiError> {
        let packages: Vec<Value> = key_packages_b64
            .iter()
            .enumerate()
            .map(|(i, pkg)| json!({ "key_id": key_package_id(pkg, i), "package": pkg }))
            .collect();
        let body = json!({
            "device_keys": {
                "algorithms": ["p.mls.1"],
                "keys": { format!("ed25519:{device_id}"): public_key_b64 },
                "signatures": {}
            },
            "key_packages": packages,
        });
        let resp = self.post("/_pigeon/client/v1/keys/upload", &body).await?;
        Ok(resp["key_package_count"].as_u64().unwrap_or(0) as u32)
    }

    /// `POST /keys/query` — list a user's published devices (M3.2). Sends an
    /// empty device list (= all devices). Returns the `{ device_id -> DeviceKeys }`
    /// map for `user_id` (empty if the user has published nothing).
    pub async fn query_keys(&self, user_id: &str) -> Result<Value, ApiError> {
        let body = json!({ "device_keys": { user_id: [] } });
        let resp = self.post("/_pigeon/client/v1/keys/query", &body).await?;
        Ok(resp["device_keys"][user_id].clone())
    }

    /// `POST /keys/claim` — claim one KeyPackage from a specific device (M3.2),
    /// consuming a one-time package (or the reusable last-resort one). Returns the
    /// base64 KeyPackage, or `None` if that device has none left to give.
    pub async fn claim_keys(
        &self,
        user_id: &str,
        device_id: &str,
    ) -> Result<Option<String>, ApiError> {
        let body = json!({ "one_time_keys": { user_id: [device_id] } });
        let resp = self.post("/_pigeon/client/v1/keys/claim", &body).await?;
        Ok(resp["one_time_keys"][user_id][device_id]["package"]
            .as_str()
            .map(str::to_owned))
    }

    /// `PUT /sendToDevice/{event_type}/{txn_id}` — deliver a to-device message
    /// (M3.3), e.g. an MLS `Welcome` (`event_type = "p.mls.welcome"`). `messages`
    /// is `{ user_id -> { device_id -> content } }` (content stored verbatim).
    pub async fn send_to_device(
        &self,
        event_type: &str,
        txn_id: &str,
        messages: &Value,
    ) -> Result<(), ApiError> {
        let path = format!("/_pigeon/client/v1/sendToDevice/{event_type}/{txn_id}");
        self.put(&path, &json!({ "messages": messages })).await?;
        Ok(())
    }

    /// `POST /rooms/{room_id}/invite` (`{ user_id }`) → the invite event id.
    pub async fn invite(&self, room_id: &str, user_id: &str) -> Result<String, ApiError> {
        let path = format!("/_pigeon/client/v1/rooms/{room_id}/invite");
        let resp = self.post(&path, &json!({ "user_id": user_id })).await?;
        json_string(&resp, "event_id")
    }

    /// `GET /rooms/{room_id}/messages?limit=N` → `{ chunk: [event, …] }`, the
    /// latest `limit` events (the server has no older-than cursor yet — M2 note).
    pub async fn messages(&self, room_id: &str, limit: u32) -> Result<Value, ApiError> {
        let limit = limit.to_string();
        let req = self
            .req(
                Method::GET,
                &format!("/_pigeon/client/v1/rooms/{room_id}/messages"),
            )
            .query(&[("limit", limit.as_str())]);
        self.send(req).await
    }

    // --- Encrypted key backup (M4.3) ----------------------------------------
    // The server stores an opaque JSON object keyed by (user, room_id, session_id)
    // and never interprets it. We use the reserved slot the reference CLI uses to
    // stash the encrypted MLS device-state blob.

    /// `PUT /room_keys/key/{room_id}/{session_id}` — store the opaque encrypted
    /// backup blob (base64) under `{ "blob": ... }` (M4.3).
    pub async fn put_room_key(
        &self,
        room_id: &str,
        session_id: &str,
        blob_b64: &str,
    ) -> Result<(), ApiError> {
        let path = format!("/_pigeon/client/v1/room_keys/key/{room_id}/{session_id}");
        self.put(&path, &json!({ "blob": blob_b64 })).await?;
        Ok(())
    }

    /// `GET /room_keys/key/{room_id}/{session_id}` — fetch the stored backup blob
    /// (base64), or `None` if nothing is backed up (server `404`/`P_NOT_FOUND`).
    pub async fn get_room_key(
        &self,
        room_id: &str,
        session_id: &str,
    ) -> Result<Option<String>, ApiError> {
        let path = format!("/_pigeon/client/v1/room_keys/key/{room_id}/{session_id}");
        match self.get(&path).await {
            Ok(resp) => Ok(resp["blob"].as_str().map(str::to_owned)),
            Err(ApiError::Server {
                code: ErrorCode::NotFound,
                ..
            }) => Ok(None),
            Err(err) => Err(err),
        }
    }

    /// `PUT /rooms/{room_id}/send/{event_type}/{txn_id}` → the event id. `content`
    /// is the raw event content (`{ body, msgtype }` for `p.room.message`, or
    /// `{ algorithm, ciphertext }` for `p.room.encrypted` — M3.5). The server
    /// ignores `txn_id` (no server-side dedup — CLAUDE.md M2 note), so the client
    /// dedups its own sends; the id still identifies the attempt in the path.
    pub async fn send_event(
        &self,
        room_id: &str,
        event_type: &str,
        txn_id: &str,
        content: &Value,
    ) -> Result<String, ApiError> {
        let path = format!("/_pigeon/client/v1/rooms/{room_id}/send/{event_type}/{txn_id}");
        let resp = self.put(&path, content).await?;
        json_string(&resp, "event_id")
    }

    /// Send a plaintext `p.room.message` — a thin wrapper over [`send_event`].
    pub async fn send_message(
        &self,
        room_id: &str,
        txn_id: &str,
        content: &Value,
    ) -> Result<String, ApiError> {
        self.send_event(room_id, "p.room.message", txn_id, content)
            .await
    }

    // --- Media (M4.1) --------------------------------------------------------
    // Raw-body transfers (not JSON): the server stores opaque bytes and echoes
    // the Content-Type back on download. Encrypted media reuses this path.

    /// `POST /media/v1/upload` — upload raw `bytes` with `content_type`, returning
    /// the `pigeon://{server}/{media_id}` content URI. The caller should reject
    /// oversize uploads before calling (the server's cap yields a bare `413`).
    pub async fn upload_media(
        &self,
        bytes: Vec<u8>,
        content_type: &str,
    ) -> Result<String, ApiError> {
        let req = self
            .req(Method::POST, "/_pigeon/media/v1/upload")
            .header(reqwest::header::CONTENT_TYPE, content_type)
            .body(bytes);
        let resp = self.send(req).await?;
        json_string(&resp, "content_uri")
    }

    /// `GET /media/v1/download/{server}/{media_id}` — fetch media bytes verbatim.
    /// Returns the raw body (the caller decrypts it for encrypted media, M4.2).
    pub async fn download_media(&self, server: &str, media_id: &str) -> Result<Vec<u8>, ApiError> {
        let path = format!("/_pigeon/media/v1/download/{server}/{media_id}");
        let resp = self
            .req(Method::GET, &path)
            .send()
            .await
            .map_err(|e| ApiError::Network {
                reason: e.to_string(),
            })?;
        let status = resp.status();
        if !status.is_success() {
            // Error bodies are JSON; a media 404 carries the P_* envelope.
            let body: Value = resp.json().await.unwrap_or(Value::Null);
            return Err(parse_error(status.as_u16(), &body));
        }
        let bytes = resp.bytes().await.map_err(|e| ApiError::Network {
            reason: e.to_string(),
        })?;
        Ok(bytes.to_vec())
    }

    /// Send a built request: parse JSON on 2xx, else map the `P_*` error body.
    async fn send(&self, req: RequestBuilder) -> Result<Value, ApiError> {
        let resp = req.send().await.map_err(|e| ApiError::Network {
            reason: e.to_string(),
        })?;
        let status = resp.status();
        // The server sends JSON on both success and error. A body that won't
        // parse is only a problem on success; on error we still synthesise a
        // typed error from the status (see `parse_error`).
        let body: Value = resp.json().await.unwrap_or(Value::Null);
        if status.is_success() {
            Ok(body)
        } else {
            Err(parse_error(status.as_u16(), &body))
        }
    }
}

/// Trim a trailing slash so `join_url` produces a clean `base + path`.
fn normalize_base(base: &str) -> String {
    base.trim_end_matches('/').to_owned()
}

/// Join a normalized base URL with an absolute `/_pigeon/...` path.
fn join_url(base: &str, path: &str) -> String {
    format!("{base}{path}")
}

/// Turn a non-2xx response into a typed [`ApiError::Server`]. Pure — unit-tested
/// without a live server. Reads the `{ "errcode", "error" }` body the server
/// documents (`PigeonError`); tolerates a missing/garbled body by falling back
/// to [`ErrorCode::Unknown`].
fn parse_error(status: u16, body: &Value) -> ApiError {
    let code = body["errcode"]
        .as_str()
        .map(ErrorCode::from_wire)
        .unwrap_or(ErrorCode::Unknown);
    let message = body["error"]
        .as_str()
        .unwrap_or("(no error message)")
        .to_owned();
    ApiError::Server {
        status,
        code,
        message,
    }
}

/// Pull a required string field from a 2xx body, or [`ApiError::Malformed`] if
/// absent — shared by the M2 endpoints that return a single id (`room_id`,
/// `event_id`).
fn json_string(body: &Value, key: &str) -> Result<String, ApiError> {
    body[key]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| ApiError::Malformed {
            reason: format!("response missing string field `{key}`"),
        })
}

/// A content-addressed `key_id` for an uploaded KeyPackage (finding P5). The
/// server dedups uploads on `(user, device, key_id)` with `ON CONFLICT DO
/// NOTHING`, so a fixed `kp-{index}` scheme meant that *republishing* after an
/// identity change (e.g. `restore_backup` mints a fresh device identity) reused
/// `kp-0..` and was silently dropped in favour of the stale packages — peers
/// then claimed KeyPackages of the discarded identity, whose Welcome could never
/// be joined. Hashing the package makes re-uploading the same bytes idempotent
/// while every *new* package gets a fresh id. `index` is only a fallback if
/// hashing somehow fails (it doesn't, for a string).
fn key_package_id(package_b64: &str, index: usize) -> String {
    pigeon_core::hash::content_hash(&json!(package_b64))
        .map(|h| {
            let hex: String = h.iter().take(8).map(|b| format!("{b:02x}")).collect();
            format!("kp-{hex}")
        })
        .unwrap_or_else(|_| format!("kp-{index}"))
}

/// Extract the three `AuthResponse` fields from a `register`/`login` 2xx body.
/// Pure — unit-tested without a server. A missing field is a protocol mismatch
/// (`Malformed`), not a server error.
fn parse_auth(body: &Value) -> Result<AuthResponse, ApiError> {
    let field = |key: &str| -> Result<String, ApiError> {
        body[key]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| ApiError::Malformed {
                reason: format!("auth response missing string field `{key}`"),
            })
    };
    Ok(AuthResponse {
        user_id: field("user_id")?,
        device_id: field("device_id")?,
        access_token: field("access_token")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn key_package_id_is_content_addressed() {
        // Same bytes → same id (a re-upload dedups); different bytes → different
        // id (a fresh identity's packages aren't dropped as duplicates — P5).
        assert_eq!(key_package_id("AAAApkg", 0), key_package_id("AAAApkg", 3));
        assert_ne!(key_package_id("pkg-one", 0), key_package_id("pkg-two", 0));
        assert!(key_package_id("anything", 0).starts_with("kp-"));
    }

    #[test]
    fn known_error_codes_round_trip() {
        // Every documented code maps to itself through wire → typed → wire.
        for wire in [
            "P_FORBIDDEN",
            "P_UNKNOWN_TOKEN",
            "P_MISSING_TOKEN",
            "P_BAD_JSON",
            "P_NOT_JSON",
            "P_NOT_FOUND",
            "P_LIMIT_EXCEEDED",
            "P_BAD_SIGNATURE",
            "P_UNRECOGNIZED",
            "P_USER_IN_USE",
            "P_INVALID_USERNAME",
            "P_UNKNOWN",
        ] {
            assert_eq!(ErrorCode::from_wire(wire).as_str(), wire);
        }
    }

    #[test]
    fn unknown_error_code_is_preserved_verbatim() {
        // A code from a newer server must not panic or be swallowed.
        let code = ErrorCode::from_wire("P_SOME_FUTURE_CODE");
        assert_eq!(
            code,
            ErrorCode::Other {
                code: "P_SOME_FUTURE_CODE".to_owned()
            }
        );
        assert_eq!(code.as_str(), "P_SOME_FUTURE_CODE");
    }

    #[test]
    fn parse_error_extracts_typed_code_and_message() {
        let body = json!({ "errcode": "P_USER_IN_USE", "error": "user already exists: @a:x" });
        match parse_error(403, &body) {
            ApiError::Server {
                status,
                code,
                message,
            } => {
                assert_eq!(status, 403);
                assert_eq!(code, ErrorCode::UserInUse);
                assert_eq!(message, "user already exists: @a:x");
            }
            other => panic!("expected Server error, got {other:?}"),
        }
    }

    #[test]
    fn parse_error_tolerates_missing_body_fields() {
        // A non-JSON / empty error body still yields a usable typed error.
        match parse_error(500, &Value::Null) {
            ApiError::Server {
                status,
                code,
                message,
            } => {
                assert_eq!(status, 500);
                assert_eq!(code, ErrorCode::Unknown);
                assert_eq!(message, "(no error message)");
            }
            other => panic!("expected Server error, got {other:?}"),
        }
    }

    #[test]
    fn parse_auth_reads_all_three_fields() {
        let body = json!({
            "user_id": "@alice:pigeon.example",
            "device_id": "ABCD1234",
            "access_token": "secret-token"
        });
        let auth = parse_auth(&body).expect("valid auth body");
        assert_eq!(auth.user_id, "@alice:pigeon.example");
        assert_eq!(auth.device_id, "ABCD1234");
        assert_eq!(auth.access_token, "secret-token");
    }

    #[test]
    fn parse_auth_rejects_missing_field() {
        // No access_token → a protocol mismatch, surfaced as Malformed.
        let body = json!({ "user_id": "@a:x", "device_id": "D1" });
        match parse_auth(&body) {
            Err(ApiError::Malformed { reason }) => assert!(reason.contains("access_token")),
            other => panic!("expected Malformed, got {other:?}"),
        }
    }

    #[test]
    fn base_url_trailing_slash_is_normalized() {
        assert_eq!(
            normalize_base("https://pigeon.example/"),
            "https://pigeon.example"
        );
        assert_eq!(
            normalize_base("https://pigeon.example"),
            "https://pigeon.example"
        );
        assert_eq!(
            join_url(
                &normalize_base("https://pigeon.example/"),
                "/_pigeon/client/v1/sync"
            ),
            "https://pigeon.example/_pigeon/client/v1/sync"
        );
    }
}
