//! `pigeon-mobile-core` — the shared Rust core for the Pigeon mobile client.
//!
//! Everything that is not UI lives here and is called from Android (Kotlin) and
//! later iOS (Swift) through UniFFI-generated bindings. See [`../../CLAUDE.md`]
//! for the rules and [`../../docs/ARCHITECTURE.md`] for the structure.
//!
//! Phase M0: this file is the toolchain smoke target only — a trivial value
//! computed in Rust plus a check that the reused `pigeon-crypto` MLS engine
//! links and runs. Real protocol surface (session, sync, rooms, e2ee, …)
//! lands in M1+ per `ROADMAP.md`.

uniffi::setup_scaffolding!();

use std::sync::RwLock;

use pigeon_crypto::Device;

/// The Client–Server API HTTP client (M1.1).
pub mod api;
/// Session lifecycle — register/login and the logged-in client object (M1.2).
pub mod session;
/// Local SQLite persistence — rooms, timeline, membership, sync token (M2.1).
pub mod store;

use api::{ApiError, ErrorCode};

/// Errors surfaced across the FFI boundary.
///
/// M0 carries a single placeholder variant; the real typed error set (mapping
/// the server's `P_*` codes and crypto/IO failures) grows from M1 onward.
// NB: error-variant fields are named `reason`, not `message` — UniFFI maps an
// error variant to a Kotlin `Throwable` subclass, and a field named `message`
// collides with `Throwable.message` (generates uncompilable bindings). Keep
// error fields off that reserved name.
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum CoreError {
    #[error("crypto error: {reason}")]
    Crypto { reason: String },
    /// The server rejected the request with a typed `P_*` code — the UI branches
    /// on `code` (e.g. `UserInUse` → "username taken").
    #[error("server rejected the request [{code}]: {reason}")]
    Api { code: ErrorCode, reason: String },
    /// Transport failure (offline, DNS/TLS, timeout). Offline-first: retryable.
    #[error("network error: {reason}")]
    Network { reason: String },
    /// The server's response wasn't the shape the protocol promised.
    #[error("protocol error: {reason}")]
    Protocol { reason: String },
    /// The platform keystore failed (persist/restore of the session). Mapped
    /// from `session::KeyStoreError`. (M1.3.)
    #[error("storage error: {reason}")]
    Storage { reason: String },
}

/// Map the HTTP layer's typed failure onto the FFI-visible error. The `P_*` code
/// is preserved (never string-matched) so the UI can branch on it.
impl From<ApiError> for CoreError {
    fn from(err: ApiError) -> Self {
        match err {
            ApiError::Server { code, message, .. } => CoreError::Api {
                code,
                reason: message,
            },
            ApiError::Network { reason } => CoreError::Network { reason },
            ApiError::Malformed { reason } => CoreError::Protocol { reason },
        }
    }
}

// --- Logging (M0.7) ----------------------------------------------------------
//
// The core never assumes a platform logger. The host installs a sink (Logcat on
// Android, os_log on iOS) via `set_log_sink`; the core emits structured records
// to it. Keep message content free of PII/plaintext (CLAUDE.md Gotcha #2).

/// Severity of a log record, mirroring `tracing` levels.
#[derive(Debug, Clone, Copy, uniffi::Enum)]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

/// A host-provided log sink. The native layer implements this and forwards to
/// the platform logger.
#[uniffi::export(callback_interface)]
pub trait LogSink: Send + Sync {
    fn log(&self, level: LogLevel, target: String, message: String);
}

static LOG_SINK: RwLock<Option<Box<dyn LogSink>>> = RwLock::new(None);

/// Install (or replace) the host log sink. Call once at startup.
#[uniffi::export]
pub fn set_log_sink(sink: Box<dyn LogSink>) {
    *LOG_SINK.write().expect("log sink lock poisoned") = Some(sink);
}

/// Emit a record to the installed sink, if any. Internal helper — the real API
/// will route `tracing` here in M1+; for now it backs `emit_test_log` and the
/// session layer's diagnostics. Never pass secrets/plaintext (Gotcha #2).
pub(crate) fn emit(level: LogLevel, target: &str, message: &str) {
    if let Some(sink) = LOG_SINK.read().expect("log sink lock poisoned").as_ref() {
        sink.log(level, target.to_string(), message.to_string());
    }
}

/// Emit one INFO record through the installed sink. Lets the Hello-core app
/// prove the host log callback round-trips end to end. (M0 verification only.)
#[uniffi::export]
pub fn emit_test_log(message: String) {
    emit(LogLevel::Info, "pigeon_mobile_core", &message);
}

/// The core's version string.
///
/// The M0 success gate: a value computed in Rust, rendered by the native UI
/// through the generated bindings (the Hello-core app calls this).
#[uniffi::export]
pub fn core_version() -> String {
    format!("pigeon-mobile-core {}", env!("CARGO_PKG_VERSION"))
}

/// Proves the reused `pigeon-crypto` MLS engine links and runs inside the
/// mobile core: creates an ephemeral device and returns its signature public
/// key length (Ed25519 → 32 bytes). Toolchain validation only — not real API.
#[uniffi::export]
pub fn self_test_crypto(user_id: String) -> Result<u32, CoreError> {
    let device = Device::new(&user_id).map_err(|e| CoreError::Crypto {
        reason: e.to_string(),
    })?;
    Ok(device.signature_public_key().len() as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_reported() {
        assert!(core_version().starts_with("pigeon-mobile-core"));
    }

    #[test]
    fn crypto_engine_links_and_runs() {
        // An Ed25519 signature public key is 32 bytes — proves the whole
        // pigeon-crypto / openmls dependency chain compiles and executes here.
        let len = self_test_crypto("@m0:test.example".to_string()).unwrap();
        assert_eq!(len, 32);
    }

    #[test]
    fn log_sink_callback_round_trips() {
        use std::sync::{Arc, Mutex};

        #[derive(Clone)]
        struct Capturing(Arc<Mutex<Vec<String>>>);
        impl LogSink for Capturing {
            fn log(&self, _level: LogLevel, target: String, message: String) {
                self.0.lock().unwrap().push(format!("{target}: {message}"));
            }
        }

        let captured = Arc::new(Mutex::new(Vec::new()));
        set_log_sink(Box::new(Capturing(captured.clone())));
        emit_test_log("hello from rust".to_string());

        let lines = captured.lock().unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], "pigeon_mobile_core: hello from rust");
    }
}
