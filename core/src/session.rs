//! Session lifecycle (M1.2): register / login and the logged-in client.
//!
//! `register` and `login` are the app's first authenticated surface. Each
//! performs the HTTP flow (via [`crate::api`], exactly as the reference CLI
//! does) and returns a [`PigeonClient`] — a core-side object that owns the
//! bearer token and drives every later authenticated flow (sync, rooms, …).
//!
//! **The access token never crosses the FFI** (CLAUDE.md Gotcha #1): it lives
//! inside the `PigeonClient`'s HTTP client, and the UI only ever receives the
//! non-secret [`Session`] identity. M1.3 will persist the token to the platform
//! keystore and restore it on launch; today it lives only in memory, so a
//! session does not yet survive an app restart.

use std::sync::Arc;

use crate::api::{Api, AuthResponse};
use crate::CoreError;

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
    // The token-bearing HTTP client for this session. Held now, read from M1.3+
    // (session restore/whoami) and M2 (sync) — the authenticated flows that hang
    // off the client. `allow(dead_code)` until then; removing it would mean
    // dropping the token, which is the whole point of the object.
    #[allow(dead_code)]
    api: Api,
    session: Session,
}

impl PigeonClient {
    /// Assemble a client from a completed auth flow: retain the token on the API
    /// (for subsequent authenticated calls) and expose only the identity.
    fn from_auth(server: String, mut api: Api, auth: AuthResponse) -> Self {
        api.set_token(Some(auth.access_token));
        Self {
            api,
            session: Session {
                user_id: auth.user_id,
                device_id: auth.device_id,
                server,
            },
        }
    }
}

#[uniffi::export]
impl PigeonClient {
    /// The non-secret session identity for this client.
    pub fn session(&self) -> Session {
        self.session.clone()
    }
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
    Ok(Arc::new(PigeonClient::from_auth(server, api, auth)))
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
    Ok(Arc::new(PigeonClient::from_auth(server, api, auth)))
}
