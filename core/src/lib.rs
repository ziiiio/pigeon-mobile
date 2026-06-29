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

use pigeon_crypto::Device;

/// Errors surfaced across the FFI boundary.
///
/// M0 carries a single placeholder variant; the real typed error set (mapping
/// the server's `P_*` codes and crypto/IO failures) grows from M1 onward.
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum CoreError {
    #[error("crypto error: {message}")]
    Crypto { message: String },
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
        message: e.to_string(),
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
}
