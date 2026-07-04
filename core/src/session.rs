//! Session lifecycle (M1.2 + M1.3): register / login, the logged-in client, and
//! keystore-backed persistence + restore.
//!
//! `register` and `login` are the app's first authenticated surface. Each
//! performs the HTTP flow (via [`crate::api`], exactly as the reference CLI
//! does) and returns a [`PigeonClient`] — a core-side object that owns the
//! bearer token and drives every later authenticated flow (sync, rooms, …).
//!
//! **The access token never crosses the FFI** (CLAUDE.md Gotcha #1): it lives
//! inside the `PigeonClient`'s HTTP client, and the UI only ever receives the
//! non-secret [`Session`] identity.
//!
//! **Persistence (M1.3):** on login the whole session blob (identity + token) is
//! written through a host-installed [`KeyStore`], backed by the platform
//! keystore (Android Keystore / iOS Keychain) — never the app DB in clear.
//! [`restore_session`] reloads it on launch and validates the token against
//! `/account/whoami`, while staying usable offline.

use std::sync::{Arc, RwLock};

use crate::api::{Api, ApiError, AuthResponse, ErrorCode};
use crate::{CoreError, LogLevel};

/// Keystore entry under which the session blob is stored. Versioned so a future
/// format change can migrate rather than misread an old blob.
const SESSION_KEY: &str = "pigeon.session.v1";

/// The non-secret identity of a logged-in session — safe to hold in native UI
/// state. The access token is deliberately absent (it stays in the core).
#[derive(Debug, Clone, uniffi::Record)]
pub struct Session {
    /// Full user id, e.g. `@alice:pigeon.example`.
    pub user_id: String,
    /// This login's device id (server-assigned unless one was supplied).
    pub device_id: String,
    /// The homeserver base URL this session is bound to.
    pub server: String,
}

/// A logged-in client. Owns the token-bearing HTTP client and the session
/// identity; handed to the UI as an opaque handle (`Arc`) that later phases hang
/// sync, rooms, and e2ee off. Secrets stay inside — nothing here is returned by
/// value across the FFI except [`Session`].
#[derive(uniffi::Object)]
pub struct PigeonClient {
    // The token-bearing HTTP client for this session. Read by `logout` (M1.5) to
    // revoke the token server-side, and by the authenticated flows that hang off
    // the client in later phases (sync, M2). The token stays inside — never
    // returned across the FFI.
    api: Api,
    session: Session,
}

#[uniffi::export]
impl PigeonClient {
    /// The non-secret session identity for this client.
    pub fn session(&self) -> Session {
        self.session.clone()
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl PigeonClient {
    /// Log out (M1.5): revoke this session's token server-side, then clear the
    /// persisted session from the keystore. After this the handle is spent — the
    /// UI drops it and returns to the signed-out state.
    ///
    /// The server-side revoke is **best-effort**, mirroring the reference CLI
    /// (`../../pigeon/clients/cli/src/main.rs::logout`): an unreachable server or
    /// an already-dead token must not strand the user holding a session they
    /// can't clear, so the local keystore is wiped regardless. A genuine keystore
    /// fault *is* surfaced ([`CoreError::Storage`]) — the blob would otherwise
    /// linger and silently restore on the next launch.
    pub async fn logout(&self) -> Result<(), CoreError> {
        // Best-effort server revoke; its outcome doesn't gate the local wipe.
        if let Err(err) = self.api.logout().await {
            crate::emit(
                LogLevel::Info,
                "session",
                &format!("server logout failed; clearing local session anyway: {err}"),
            );
        }
        ks_delete(SESSION_KEY)?;
        Ok(())
    }
}

// --- The host key store (M1.3) ----------------------------------------------
//
// A secure key–value store the native layer implements over the platform
// keystore. The core persists the session blob here; secrets never touch the
// app DB in clear (Gotcha #1). Installed once at startup, like the log sink.

/// A host-provided secure key–value store. Backed by the Android Keystore /
/// iOS Keychain on device.
#[uniffi::export(callback_interface)]
pub trait KeyStore: Send + Sync {
    /// Store `value` under `key`, replacing any existing entry.
    fn put(&self, key: String, value: Vec<u8>) -> Result<(), KeyStoreError>;
    /// Fetch the value for `key`, or `None` if absent.
    fn get(&self, key: String) -> Result<Option<Vec<u8>>, KeyStoreError>;
    /// Remove `key` if present (a no-op if absent).
    fn delete(&self, key: String) -> Result<(), KeyStoreError>;
}

/// A failure from the host keystore backend.
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum KeyStoreError {
    #[error("key store backend error: {reason}")]
    Backend { reason: String },
}

impl From<KeyStoreError> for CoreError {
    fn from(err: KeyStoreError) -> Self {
        let KeyStoreError::Backend { reason } = err;
        CoreError::Storage { reason }
    }
}

static KEY_STORE: RwLock<Option<Box<dyn KeyStore>>> = RwLock::new(None);

/// Install (or replace) the host key store. Call once at startup, before any
/// session op that should persist (register/login/restore_session).
#[uniffi::export]
pub fn set_key_store(store: Box<dyn KeyStore>) {
    *KEY_STORE.write().expect("key store lock poisoned") = Some(store);
}

// The `ks_*` helpers acquire and release the lock *within* the call, so a guard
// is never held across an `.await` (that would risk a deadlock and isn't Send).

/// Persist `value`; returns `false` if no key store is installed (nothing done).
fn ks_put(key: &str, value: Vec<u8>) -> Result<bool, CoreError> {
    match KEY_STORE.read().expect("key store lock poisoned").as_ref() {
        Some(store) => {
            store.put(key.to_owned(), value)?;
            Ok(true)
        }
        None => Ok(false),
    }
}

/// Read a value; `None` if absent or if no key store is installed.
fn ks_get(key: &str) -> Result<Option<Vec<u8>>, CoreError> {
    match KEY_STORE.read().expect("key store lock poisoned").as_ref() {
        Some(store) => Ok(store.get(key.to_owned())?),
        None => Ok(None),
    }
}

/// Delete a key (no-op if absent or if no key store is installed).
fn ks_delete(key: &str) -> Result<(), CoreError> {
    if let Some(store) = KEY_STORE.read().expect("key store lock poisoned").as_ref() {
        store.delete(key.to_owned())?;
    }
    Ok(())
}

/// Serialise identity + token to a JSON blob. The whole blob is protected at
/// rest by the platform keystore, so the token rides along with the identity.
fn encode_session(session: &Session, token: &str) -> Vec<u8> {
    serde_json::json!({
        "user_id": session.user_id,
        "device_id": session.device_id,
        "server": session.server,
        "access_token": token,
    })
    .to_string()
    .into_bytes()
}

/// Parse a stored blob back into identity + token. A malformed blob is a storage
/// fault, not a protocol one.
fn decode_session(bytes: &[u8]) -> Result<(Session, String), CoreError> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|e| CoreError::Storage {
            reason: format!("corrupt stored session: {e}"),
        })?;
    let field = |key: &str| -> Result<String, CoreError> {
        value[key]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| CoreError::Storage {
                reason: format!("stored session missing `{key}`"),
            })
    };
    let session = Session {
        user_id: field("user_id")?,
        device_id: field("device_id")?,
        server: field("server")?,
    };
    Ok((session, field("access_token")?))
}

/// Persist the session and assemble the client. Persisting first means a crash
/// immediately after login still leaves a restorable session. Best-effort if no
/// key store is installed (in-memory only this run — warned, not an error).
fn finish_login(
    server: String,
    mut api: Api,
    auth: AuthResponse,
) -> Result<Arc<PigeonClient>, CoreError> {
    let session = Session {
        user_id: auth.user_id,
        device_id: auth.device_id,
        server,
    };
    if !ks_put(SESSION_KEY, encode_session(&session, &auth.access_token))? {
        crate::emit(
            LogLevel::Warn,
            "session",
            "no key store installed; session will not survive restart",
        );
    }
    api.set_token(Some(auth.access_token));
    Ok(Arc::new(PigeonClient { api, session }))
}

/// Register a new account on `server` and return a logged-in client.
///
/// `server` is a full homeserver base URL (`https://pigeon.example`); `username`
/// is the localpart (the server forms the full `@username:server` id).
#[uniffi::export(async_runtime = "tokio")]
pub async fn register(
    server: String,
    username: String,
    password: String,
) -> Result<Arc<PigeonClient>, CoreError> {
    let api = Api::new(&server, None)?;
    let auth = api.register(&username, &password).await?;
    finish_login(server, api, auth)
}

/// Log into an existing account on `server` and return a logged-in client.
///
/// `user` may be a bare localpart or a full `@user:server` id (the server
/// resolves it).
#[uniffi::export(async_runtime = "tokio")]
pub async fn login(
    server: String,
    user: String,
    password: String,
) -> Result<Arc<PigeonClient>, CoreError> {
    let api = Api::new(&server, None)?;
    let auth = api.login(&user, &password).await?;
    finish_login(server, api, auth)
}

/// Restore a persisted session on launch, if one exists.
///
/// Returns the logged-in client, or `None` when there is no stored session (or
/// the stored token has been revoked). The token is validated against
/// `/account/whoami`, but restore stays usable **offline**: a transport failure
/// restores the session optimistically, and a genuinely stale token will surface
/// on the next authenticated call rather than blocking launch.
#[uniffi::export(async_runtime = "tokio")]
pub async fn restore_session() -> Result<Option<Arc<PigeonClient>>, CoreError> {
    let Some(bytes) = ks_get(SESSION_KEY)? else {
        return Ok(None);
    };
    let (session, token) = decode_session(&bytes)?;
    let api = Api::new(&session.server, Some(token))?;

    match api.whoami().await {
        // Token accepted — a live session.
        Ok(_) => Ok(Some(Arc::new(PigeonClient { api, session }))),
        // Token is definitively dead: drop it so we don't loop on a revoked
        // session. `None` reads to the UI as "logged out".
        Err(ApiError::Server {
            code: ErrorCode::UnknownToken | ErrorCode::MissingToken,
            ..
        }) => {
            ks_delete(SESSION_KEY)?;
            Ok(None)
        }
        // Offline / unreachable: trust the stored token and restore. Do NOT log
        // the user out just because the network is down (offline-first).
        Err(ApiError::Network { .. }) => {
            crate::emit(
                LogLevel::Info,
                "session",
                "restored session without server validation (offline)",
            );
            Ok(Some(Arc::new(PigeonClient { api, session })))
        }
        // Any other server/protocol error is unexpected during a token check.
        Err(other) => Err(other.into()),
    }
}
