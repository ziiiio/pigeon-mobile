//! MLS end-to-end encryption engine (M3) — wraps [`pigeon_crypto::Device`].
//!
//! This is the *only* place crypto happens in the client (CLAUDE.md Cardinal
//! Rule); it reuses `pigeon-crypto` (MLS via `openmls`) and adds nothing of its
//! own. It mirrors the reference CLI's engine
//! (`../../pigeon/clients/cli/src/e2ee.rs`) — the canonical implementation — with
//! one deliberate difference: **state is persisted through the host
//! [`KeyStore`](crate::session::KeyStore), not a file**, because the MLS state
//! blob contains private key material and must live under the platform keystore
//! (CLAUDE.md Gotcha #1), never in the app DB or a world-readable file.
//!
//! ## What's persisted
//!
//! `pigeon-crypto` keeps all device state (the private signer, KeyPackage private
//! parts, and every group's ratchet state) in an in-memory provider; it has no
//! pluggable storage backend. So after **every** state-mutating operation
//! (generating KeyPackages, creating/joining a group, encrypting, decrypting) we
//! call [`Device::export_storage`] and persist the blob, and on launch we
//! [`Device::restore`] from it. The persisted blob is `{ pubkey, blob }` (both
//! base64); `restore` needs the user id (from the session), the signature public
//! key, and the storage blob.
//!
//! ## Wire encoding
//!
//! Everything crossing this boundary to `pigeon-crypto` is raw bytes; everything
//! crossing to the Client–Server API is base64 (public key, KeyPackages, Welcome,
//! ciphertext — the `p.mls.1` convention the server + CLI use). This module owns
//! that encode/decode so the rest of the core deals in base64 strings.

use std::sync::Mutex;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use pigeon_crypto::Device;
use serde_json::Value;

use crate::CoreError;

/// Keystore entry under which the device's MLS state blob is persisted.
/// Versioned so a future format change can migrate rather than misread.
const MLS_STATE_KEY: &str = "pigeon.mls.state.v1";

/// How many KeyPackages to publish per device. Each claim by a peer that adds us
/// to a group consumes one; a small add-mostly pool matches the reference CLI.
pub(crate) const KEY_PACKAGE_COUNT: usize = 5;

/// The client's MLS engine for one session. Owns the `pigeon-crypto` [`Device`]
/// (all secrets) behind a mutex so its ratcheting ops serialise, and persists the
/// device state through the host keystore after each mutation.
///
/// Held inside [`PigeonClient`](crate::session::PigeonClient); never handed across
/// the FFI — only base64 wire material and decrypted plaintext leave it.
pub struct E2ee {
    device: Mutex<Device>,
}

impl E2ee {
    /// Create a **fresh** device identity for `user_id` and persist it. Called on
    /// register/login — each authenticated session mints a new MLS identity
    /// (matching the reference CLI), then publishes its keys.
    pub fn create(user_id: &str) -> Result<Self, CoreError> {
        let device = Device::new(user_id)?;
        persist(&device)?;
        Ok(Self {
            device: Mutex::new(device),
        })
    }

    /// Restore a previously-persisted device for `user_id` from the keystore, or
    /// `None` if none is stored (a session predating E2EE, or a fresh install).
    /// Used on `restore_session` so relaunching keeps existing groups.
    pub fn restore(user_id: &str) -> Result<Option<Self>, CoreError> {
        let Some(bytes) = crate::session::ks_get(MLS_STATE_KEY)? else {
            return Ok(None);
        };
        let stored: Value = serde_json::from_slice(&bytes).map_err(|e| CoreError::Storage {
            reason: format!("corrupt stored MLS state: {e}"),
        })?;
        let pubkey = decode_field(&stored, "pubkey")?;
        let blob = decode_field(&stored, "blob")?;
        let device = Device::restore(user_id, &pubkey, &blob)?;
        Ok(Some(Self {
            device: Mutex::new(device),
        }))
    }

    /// Remove the persisted MLS state (called on logout, alongside the session
    /// blob wipe). A no-op if nothing is stored.
    pub fn clear() -> Result<(), CoreError> {
        crate::session::ks_delete(MLS_STATE_KEY)
    }

    /// The device's base64 ed25519 signature public key — the identity key
    /// published via `/keys/upload`.
    pub fn signature_public_key_b64(&self) -> String {
        STANDARD.encode(
            self.device
                .lock()
                .expect("e2ee mutex poisoned")
                .signature_public_key(),
        )
    }

    /// Generate `count` fresh KeyPackages (base64) to seed the device's pool for
    /// `/keys/upload`. Their private parts are written into device state, so this
    /// **persists** — otherwise a claimed KeyPackage couldn't be used after a
    /// relaunch.
    pub fn key_packages(&self, count: usize) -> Result<Vec<String>, CoreError> {
        let device = self.device.lock().expect("e2ee mutex poisoned");
        let packages = device.new_key_packages(count)?;
        persist(&device)?;
        Ok(packages.iter().map(|kp| STANDARD.encode(kp)).collect())
    }
}

/// Persist the device's whole state through the host keystore. Best-effort if no
/// keystore is installed (in-memory this run — warned by the session layer's
/// `ks_put`). Contains private key material → keystore only, never the app DB.
fn persist(device: &Device) -> Result<(), CoreError> {
    let blob = device.export_storage()?;
    let stored = serde_json::json!({
        "pubkey": STANDARD.encode(device.signature_public_key()),
        "blob": STANDARD.encode(&blob),
    });
    crate::session::ks_put(MLS_STATE_KEY, stored.to_string().into_bytes())?;
    Ok(())
}

/// Decode a base64 string field from the persisted state blob.
fn decode_field(stored: &Value, key: &str) -> Result<Vec<u8>, CoreError> {
    let s = stored[key].as_str().ok_or_else(|| CoreError::Storage {
        reason: format!("stored MLS state missing `{key}`"),
    })?;
    STANDARD.decode(s).map_err(|e| CoreError::Storage {
        reason: format!("stored MLS `{key}` is not valid base64: {e}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex as StdMutex};

    use crate::session::{set_key_store, KeyStore, KeyStoreError};

    /// An in-memory keystore for the persistence tests.
    #[derive(Clone, Default)]
    struct MemStore {
        map: Arc<StdMutex<HashMap<String, Vec<u8>>>>,
    }
    impl KeyStore for MemStore {
        fn put(&self, key: String, value: Vec<u8>) -> Result<(), KeyStoreError> {
            self.map.lock().unwrap().insert(key, value);
            Ok(())
        }
        fn get(&self, key: String) -> Result<Option<Vec<u8>>, KeyStoreError> {
            Ok(self.map.lock().unwrap().get(&key).cloned())
        }
        fn delete(&self, key: String) -> Result<(), KeyStoreError> {
            self.map.lock().unwrap().remove(&key);
            Ok(())
        }
    }

    #[test]
    #[serial]
    fn create_persists_and_restore_recovers_the_same_identity() {
        set_key_store(Box::new(MemStore::default()));

        let engine = E2ee::create("@alice:test.example").unwrap();
        let pubkey = engine.signature_public_key_b64();
        assert!(!pubkey.is_empty());

        // A relaunch restores the same identity from the keystore.
        let restored = E2ee::restore("@alice:test.example")
            .unwrap()
            .expect("state was persisted");
        assert_eq!(restored.signature_public_key_b64(), pubkey);
    }

    #[test]
    #[serial]
    fn restore_with_no_state_is_none() {
        set_key_store(Box::new(MemStore::default()));
        assert!(E2ee::restore("@nobody:test.example").unwrap().is_none());
    }

    #[test]
    #[serial]
    fn key_packages_are_distinct_and_persist() {
        set_key_store(Box::new(MemStore::default()));
        let engine = E2ee::create("@alice:test.example").unwrap();

        let kps = engine.key_packages(3).unwrap();
        assert_eq!(kps.len(), 3);
        // Each KeyPackage is fresh/distinct.
        assert_ne!(kps[0], kps[1]);
        assert_ne!(kps[1], kps[2]);

        // The generated private parts survived persistence: restore round-trips.
        let restored = E2ee::restore("@alice:test.example").unwrap().unwrap();
        assert_eq!(
            restored.signature_public_key_b64(),
            engine.signature_public_key_b64()
        );
    }

    #[test]
    #[serial]
    fn restore_rejects_corrupt_state() {
        let store = MemStore::default();
        set_key_store(Box::new(store.clone()));
        store
            .map
            .lock()
            .unwrap()
            .insert(MLS_STATE_KEY.into(), b"not json".to_vec());
        match E2ee::restore("@alice:test.example") {
            Err(CoreError::Storage { .. }) => {}
            Ok(_) => panic!("expected Storage error, got Ok"),
            Err(other) => panic!("expected Storage error, got {other:?}"),
        }
    }

    #[test]
    #[serial]
    fn clear_removes_persisted_state() {
        set_key_store(Box::new(MemStore::default()));
        E2ee::create("@alice:test.example").unwrap();
        assert!(E2ee::restore("@alice:test.example").unwrap().is_some());
        E2ee::clear().unwrap();
        assert!(E2ee::restore("@alice:test.example").unwrap().is_none());
    }
}
