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
