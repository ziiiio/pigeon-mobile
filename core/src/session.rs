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

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use crate::api::{Api, ApiError, AuthResponse, ErrorCode};
use crate::e2ee::{E2ee, KEY_PACKAGE_COUNT};
use crate::store::Store;
use crate::{CoreError, LogLevel};

/// Keystore entry under which the session blob is stored. Versioned so a future
/// format change can migrate rather than misread an old blob.
const SESSION_KEY: &str = "pigeon.session.v1";

/// Reserved key-backup slot for the encrypted MLS device-state blob (M4.3) — the
/// same `(room_id, session_id)` the reference CLI uses, so backups interoperate.
const BACKUP_ROOM: &str = "!e2ee-backup";
const BACKUP_SESSION: &str = "mls-device-state";

/// The SQLite file name inside the host's configured data dir (single-account
/// client — one store per app; multi-account would key this per user).
const STORE_FILE: &str = "pigeon.sqlite3";

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
    // the client (sync/rooms/send — M2). The token stays inside — never returned
    // across the FFI. `pub(crate)` so the `sync`/`rooms` modules' `impl`s reach it.
    pub(crate) api: Api,
    session: Session,
    /// The local store this session reads from and the sync loop writes into
    /// (M2). Shared (`Arc`) so a spawned sync task and the UI-facing reads use
    /// the one connection. Dropped with the client on logout.
    pub(crate) store: Arc<Store>,
    /// The MLS end-to-end encryption engine for this session (M3). Owns the
    /// device identity + group state (all secrets stay inside it); the `e2ee`,
    /// `keys`, and `rooms` modules drive it. `None` only if the identity could
    /// not be created/restored (E2EE unavailable — plaintext still works).
    pub(crate) e2ee: Option<E2ee>,
}

#[uniffi::export]
impl PigeonClient {
    /// The non-secret session identity for this client.
    pub fn session(&self) -> Session {
        self.session.clone()
    }
}

impl PigeonClient {
    /// The session's MLS engine, or a typed error if E2EE couldn't be set up for
    /// this session. Crypto-touching flows call this before operating.
    pub(crate) fn e2ee(&self) -> Result<&E2ee, CoreError> {
        self.e2ee.as_ref().ok_or_else(|| CoreError::Crypto {
            reason: "E2EE is not available for this session".to_owned(),
        })
    }

    /// Publish this device's MLS identity key + a pool of KeyPackages via
    /// `/keys/upload` (M3.1), so peers can claim a package to add us to encrypted
    /// groups. **Best-effort**, exactly as the reference CLI does: a failure
    /// (offline, or no MLS identity) is logged, not fatal — plaintext rooms keep
    /// working, and keys can be re-published on the next login. Never logs key
    /// material (Gotcha #2).
    pub(crate) async fn publish_device_keys(&self) {
        let Some(e2ee) = self.e2ee.as_ref() else {
            return;
        };
        let kps = match e2ee.key_packages(KEY_PACKAGE_COUNT) {
            Ok(kps) => kps,
            Err(err) => {
                crate::emit(
                    LogLevel::Warn,
                    "e2ee",
                    &format!("could not generate KeyPackages: {err}"),
                );
                return;
            }
        };
        // The reusable last-resort package (server finding P6) keeps the device
        // addable to new groups once the one-time pool is claimed dry.
        // Best-effort like the rest of this publish: on failure, upload the
        // pool without it.
        let last_resort = match e2ee.last_resort_key_package() {
            Ok(lr) => Some(lr),
            Err(err) => {
                crate::emit(
                    LogLevel::Warn,
                    "e2ee",
                    &format!("could not generate the last-resort KeyPackage: {err}"),
                );
                None
            }
        };
        let pubkey = e2ee.signature_public_key_b64();
        match self
            .api
            .upload_keys(
                &self.session.device_id,
                &pubkey,
                &kps,
                last_resort.as_deref(),
            )
            .await
        {
            Ok(count) => crate::emit(
                LogLevel::Info,
                "e2ee",
                &format!("published device keys ({count} KeyPackages available)"),
            ),
            Err(err) => crate::emit(
                LogLevel::Warn,
                "e2ee",
                &format!("device key upload failed (E2EE limited until next login): {err}"),
            ),
        }
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
        // Wipe the MLS device state too — a new login mints a fresh identity, so
        // the old private keys/groups must not linger in the keystore (Gotcha #1).
        E2ee::clear()?;
        Ok(())
    }

    /// Back up this device's encryption keys (M4.3): create an encrypted backup of
    /// the MLS state and store the opaque blob on the server, returning the
    /// **recovery key** for the user to save. The recovery key is the only secret
    /// that can restore the backup and **never leaves the device via the server**
    /// (the server stores only ciphertext — Gotcha #1). Show it once; if lost, the
    /// backup is unrecoverable. Errors if E2EE isn't set up for this session.
    pub async fn backup(&self) -> Result<String, CoreError> {
        let (recovery_key, blob) = self.e2ee()?.create_backup()?;
        self.api
            .put_room_key(BACKUP_ROOM, BACKUP_SESSION, &blob)
            .await?;
        Ok(recovery_key)
    }

    /// Restore this device's encryption keys from the server-side backup (M4.3)
    /// using the user's `recovery_key`. Fetches the encrypted blob, decrypts it,
    /// **replaces** this session's freshly-minted identity with the recovered one,
    /// and re-publishes its keys so peers can reach it. Errors if there's no backup
    /// on the server or the recovery key is wrong (AEAD decryption fails cleanly).
    pub async fn restore_backup(&self, recovery_key: String) -> Result<(), CoreError> {
        let blob = self
            .api
            .get_room_key(BACKUP_ROOM, BACKUP_SESSION)
            .await?
            .ok_or_else(|| CoreError::Crypto {
                reason: "no encryption-key backup found on the server".to_owned(),
            })?;
        self.e2ee()?.restore_from_backup(&recovery_key, &blob)?;
        // The recovered identity differs from the fresh login one — re-publish so
        // peers can claim its KeyPackages for new invites.
        self.publish_device_keys().await;
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

// --- The local store location (M2.1/M2.2) -----------------------------------
//
// The host tells the core where its app-private data dir is (Android `filesDir`
// / iOS Application Support) once at startup, like the log sink and key store.
// The session opens the SQLite store there. Secrets never land here — only
// rooms/timeline/state (Gotcha #1); the token stays in the keystore.

static STORE_DIR: RwLock<Option<PathBuf>> = RwLock::new(None);

/// Set the directory the local store lives in. Call once at startup, before any
/// session op (register/login/restore) that should persist across restarts.
#[uniffi::export]
pub fn set_store_dir(dir: String) {
    *STORE_DIR.write().expect("store dir lock poisoned") = Some(PathBuf::from(dir));
}

/// Open the session's local store: a file under the configured data dir, or an
/// in-memory store when the host hasn't set one (unit tests, or a host that
/// opted out — non-persistent this run, warned).
fn open_store() -> Result<Arc<Store>, CoreError> {
    let dir = STORE_DIR.read().expect("store dir lock poisoned").clone();
    let store = match dir {
        Some(dir) => Store::open(&dir.join(STORE_FILE))?,
        None => {
            crate::emit(
                LogLevel::Warn,
                "session",
                "no store dir set; using in-memory store (data will not survive restart)",
            );
            Store::open_in_memory()?
        }
    };
    Ok(Arc::new(store))
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
/// `pub(crate)` so the e2ee engine can persist MLS state through the same store.
pub(crate) fn ks_put(key: &str, value: Vec<u8>) -> Result<bool, CoreError> {
    match KEY_STORE.read().expect("key store lock poisoned").as_ref() {
        Some(store) => {
            store.put(key.to_owned(), value)?;
            Ok(true)
        }
        None => Ok(false),
    }
}

/// Read a value; `None` if absent or if no key store is installed.
pub(crate) fn ks_get(key: &str) -> Result<Option<Vec<u8>>, CoreError> {
    match KEY_STORE.read().expect("key store lock poisoned").as_ref() {
        Some(store) => Ok(store.get(key.to_owned())?),
        None => Ok(None),
    }
}

/// Delete a key (no-op if absent or if no key store is installed).
pub(crate) fn ks_delete(key: &str) -> Result<(), CoreError> {
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
    let store = open_store()?;
    // Mint a fresh MLS device identity for this session (M3.1). Best-effort: if
    // it fails, E2EE is unavailable but plaintext rooms still work — so we log
    // and carry on rather than failing the login.
    let e2ee = match E2ee::create(&session.user_id) {
        Ok(engine) => Some(engine),
        Err(err) => {
            crate::emit(
                LogLevel::Warn,
                "e2ee",
                &format!("could not create MLS identity; E2EE disabled this session: {err}"),
            );
            None
        }
    };
    Ok(Arc::new(PigeonClient {
        api,
        session,
        store,
        e2ee,
    }))
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
    let client = finish_login(server, api, auth)?;
    client.publish_device_keys().await;
    Ok(client)
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
    let client = finish_login(server, api, auth)?;
    client.publish_device_keys().await;
    Ok(client)
}

/// Restore the session's MLS engine on launch, or mint a fresh identity if none
/// is persisted (a session predating E2EE, or corrupt state). Returns the engine
/// and whether it was freshly minted (⇒ its keys must be published). All failures
/// are best-effort: E2EE degrades to unavailable (`None`), never blocking launch.
fn restore_or_mint_e2ee(user_id: &str) -> (Option<E2ee>, bool) {
    match E2ee::restore(user_id) {
        Ok(Some(engine)) => (Some(engine), false),
        Ok(None) => mint_e2ee(user_id),
        Err(err) => {
            // Corrupt/unreadable state — mint a fresh identity rather than
            // stranding the session without E2EE.
            crate::emit(
                LogLevel::Warn,
                "e2ee",
                &format!("stored MLS state unusable; minting a fresh identity: {err}"),
            );
            mint_e2ee(user_id)
        }
    }
}

fn mint_e2ee(user_id: &str) -> (Option<E2ee>, bool) {
    match E2ee::create(user_id) {
        Ok(engine) => (Some(engine), true),
        Err(err) => {
            crate::emit(
                LogLevel::Warn,
                "e2ee",
                &format!("could not create MLS identity; E2EE disabled this session: {err}"),
            );
            (None, false)
        }
    }
}

/// Restore a persisted session on launch, if one exists.
///
/// Returns the logged-in client, or `None` when there is no stored session (or
/// the stored token has been revoked). The token is validated against
/// `/account/whoami`, but restore stays usable **offline**: a transport failure
/// restores the session optimistically, and a genuinely stale token will surface
/// on the next authenticated call rather than blocking launch.
///
/// The MLS engine is restored from the keystore alongside the session (M3.1) so
/// existing encrypted groups survive a relaunch; if it had to be freshly minted
/// (e.g. an older session), its keys are (re)published when online.
#[uniffi::export(async_runtime = "tokio")]
pub async fn restore_session() -> Result<Option<Arc<PigeonClient>>, CoreError> {
    let Some(bytes) = ks_get(SESSION_KEY)? else {
        return Ok(None);
    };
    let (session, token) = decode_session(&bytes)?;
    let api = Api::new(&session.server, Some(token))?;

    match api.whoami().await {
        // Token accepted — a live session.
        Ok(_) => {
            let (e2ee, minted) = restore_or_mint_e2ee(&session.user_id);
            let client = Arc::new(PigeonClient {
                api,
                session,
                store: open_store()?,
                e2ee,
            });
            // Only publish if we minted a new identity (online path); a restored
            // identity's keys are already on the server.
            if minted {
                client.publish_device_keys().await;
            }
            Ok(Some(client))
        }
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
        // the user out just because the network is down (offline-first). The MLS
        // engine restores from the local keystore regardless; if it had to be
        // minted fresh, key publishing waits for the next online session.
        Err(ApiError::Network { .. }) => {
            crate::emit(
                LogLevel::Info,
                "session",
                "restored session without server validation (offline)",
            );
            let (e2ee, _minted) = restore_or_mint_e2ee(&session.user_id);
            Ok(Some(Arc::new(PigeonClient {
                api,
                session,
                store: open_store()?,
                e2ee,
            })))
        }
        // Any other server/protocol error is unexpected during a token check.
        Err(other) => Err(other.into()),
    }
}
