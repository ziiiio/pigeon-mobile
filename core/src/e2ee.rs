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

/// The result of [`E2ee::add_member`] (`pigeon-crypto` finding C1): both halves of
/// an add, base64-encoded for the wire.
///
/// - `welcome` goes to the **new** member out of band (`/sendToDevice`,
///   `p.mls.welcome`) so they can join the group.
/// - `commit` must be broadcast to the **existing** members as a room event
///   (`p.mls.commit`); each applies it via [`E2ee::process_commit`] to advance to
///   the new epoch. Without it, a *third* member's addition strands the earlier
///   members a ratchet epoch behind and they can no longer decrypt. (The author
///   self-merges its own commit, so it must not re-apply this one.)
pub struct AddOutcome {
    pub welcome: String,
    pub commit: String,
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

    /// Whether this device holds the MLS group for `room_id` — i.e. the room is
    /// encrypted *and we're a member*. The group id is the room id's bytes.
    pub fn has_group(&self, room_id: &str) -> Result<bool, CoreError> {
        let device = self.device.lock().expect("e2ee mutex poisoned");
        Ok(device.load_group(room_id.as_bytes())?.is_some())
    }

    /// Create the MLS group for `room_id` (we host it — used when creating an
    /// encrypted room, M3.4). Persists. The group id is the room id's bytes.
    pub fn create_group(&self, room_id: &str) -> Result<(), CoreError> {
        let device = self.device.lock().expect("e2ee mutex poisoned");
        device.create_group(room_id.as_bytes())?;
        persist(&device)
    }

    /// Add a member to `room_id`'s group from their base64 KeyPackage (M3.4);
    /// returns the [`AddOutcome`] — the `Welcome` for the new member **and** the
    /// `commit` the existing members must apply (`pigeon-crypto` finding C1). The
    /// engine self-merges our own commit locally (we advance immediately), so the
    /// caller ships the Welcome to the invitee and broadcasts the commit to the
    /// room; we must not re-process our own commit. Persists (group state advances).
    pub fn add_member(
        &self,
        room_id: &str,
        key_package_b64: &str,
    ) -> Result<AddOutcome, CoreError> {
        let device = self.device.lock().expect("e2ee mutex poisoned");
        let mut group =
            device
                .load_group(room_id.as_bytes())?
                .ok_or_else(|| CoreError::Crypto {
                    reason: format!("no MLS group for {room_id}"),
                })?;
        let key_package = decode_wire(key_package_b64, "KeyPackage")?;
        let outcome = device.add_member(&mut group, &key_package)?;
        persist(&device)?;
        Ok(AddOutcome {
            welcome: STANDARD.encode(outcome.welcome),
            commit: STANDARD.encode(outcome.commit),
        })
    }

    /// Apply a base64 `p.mls.commit` broadcast by another member's `add_member`
    /// (finding C1), advancing our copy of `room_id`'s group to the new epoch so we
    /// can keep decrypting. Persists (the ratchet advances). Idempotent-safe for
    /// the caller: a commit that doesn't apply to our current epoch — our **own**
    /// commit (already self-merged), or the one that *added us* (we joined at the
    /// post-commit epoch via the Welcome), or a replay — surfaces as an error the
    /// sync loop swallows; only a commit that legitimately advances us returns
    /// `Ok`. Errors if we don't hold the group. Never logs key material (Gotcha #2).
    pub fn process_commit(&self, room_id: &str, commit_b64: &str) -> Result<(), CoreError> {
        let device = self.device.lock().expect("e2ee mutex poisoned");
        let mut group = load_group(&device, room_id)?;
        let commit = decode_wire(commit_b64, "commit")?;
        device.process_commit(&mut group, &commit)?;
        persist(&device)
    }

    /// Join a group from a base64 `Welcome` (pulled from a `p.mls.welcome`
    /// to-device event, M3.3). Persists. Idempotency for at-least-once delivery
    /// (Gotcha #8) is the caller's job — check [`has_group`](Self::has_group)
    /// first, since the Welcome carries the room id out-of-band.
    pub fn join_from_welcome(&self, welcome_b64: &str) -> Result<(), CoreError> {
        let device = self.device.lock().expect("e2ee mutex poisoned");
        let welcome = decode_wire(welcome_b64, "Welcome")?;
        device.join_from_welcome(&welcome)?;
        persist(&device)
    }

    /// Encrypt `plaintext` for `room_id`'s group (M3.5); returns base64 ciphertext
    /// for a `p.room.encrypted` event's `content.ciphertext`. Persists (the
    /// sender ratchet advances). Errors if we don't hold the room's group.
    pub fn encrypt(&self, room_id: &str, plaintext: &str) -> Result<String, CoreError> {
        let device = self.device.lock().expect("e2ee mutex poisoned");
        let mut group = load_group(&device, room_id)?;
        let ciphertext = device.encrypt(&mut group, plaintext.as_bytes())?;
        persist(&device)?;
        Ok(STANDARD.encode(ciphertext))
    }

    /// Decrypt a base64 `p.room.encrypted` ciphertext for `room_id`'s group
    /// (M3.5); returns the plaintext. Persists (the ratchet advances and is
    /// persisted — the caller must cache the plaintext, since re-decrypting the
    /// same message later fails, Gotcha #3). Errors if we don't hold the group,
    /// the ciphertext is malformed/tampered, or it's not decryptable for us.
    pub fn decrypt(&self, room_id: &str, ciphertext_b64: &str) -> Result<String, CoreError> {
        let device = self.device.lock().expect("e2ee mutex poisoned");
        let mut group = load_group(&device, room_id)?;
        let ciphertext = decode_wire(ciphertext_b64, "ciphertext")?;
        let plaintext = device.decrypt(&mut group, &ciphertext)?;
        persist(&device)?;
        String::from_utf8(plaintext).map_err(|e| CoreError::Crypto {
            reason: format!("decrypted bytes are not UTF-8: {e}"),
        })
    }
}

impl E2ee {
    /// Encrypt media bytes under a fresh random key (M4.2). Returns
    /// `(key_b64, ciphertext)`: the base64 key to embed in the E2EE message event
    /// (never uploaded), and the ciphertext blob to upload to the opaque media
    /// store. Pure symmetric AEAD via `pigeon-crypto` — independent of the group,
    /// so it needs no device state and can't fail on "no group".
    pub fn encrypt_media(&self, plaintext: &[u8]) -> Result<(String, Vec<u8>), CoreError> {
        let enc = pigeon_crypto::encrypt_media(plaintext)?;
        Ok((STANDARD.encode(&enc.key), enc.ciphertext))
    }

    /// Decrypt media `ciphertext` (downloaded from the store) with the base64
    /// `key_b64` carried in the E2EE event (M4.2). Wrong key / tampered bytes fail
    /// cleanly.
    pub fn decrypt_media(&self, key_b64: &str, ciphertext: &[u8]) -> Result<Vec<u8>, CoreError> {
        let key = decode_wire(key_b64, "media key")?;
        Ok(pigeon_crypto::decrypt_media(&key, ciphertext)?)
    }

    /// Create an encrypted backup of this device's whole MLS state (M4.3), for
    /// upload to the server's key-backup store. Returns `(recovery_key, blob)`,
    /// both base64: the **recovery key** is the only restore secret (show it to
    /// the user to save; it never touches the server), and the **blob** is the
    /// AEAD-encrypted state to store server-side (opaque — the server can't read
    /// it, Gotcha #1). Delegates entirely to `pigeon-crypto` (no crypto here).
    pub fn create_backup(&self) -> Result<(String, String), CoreError> {
        let device = self.device.lock().expect("e2ee mutex poisoned");
        let backup = device.create_backup()?;
        Ok((
            STANDARD.encode(&backup.recovery_key),
            STANDARD.encode(&backup.blob),
        ))
    }

    /// Restore this session's device from an encrypted backup (M4.3): decrypt the
    /// server-fetched `blob` with the user's `recovery_key` and **replace** the
    /// current in-memory device (a fresh identity minted at login) with the
    /// recovered one, then persist it. A wrong recovery key fails AEAD decryption
    /// cleanly. After this, the device holds the backed-up identity + groups.
    pub fn restore_from_backup(
        &self,
        recovery_key_b64: &str,
        blob_b64: &str,
    ) -> Result<(), CoreError> {
        let recovery_key = decode_wire(recovery_key_b64, "recovery key")?;
        let blob = decode_wire(blob_b64, "backup blob")?;
        let restored = Device::restore_from_backup(&recovery_key, &blob)?;
        let mut device = self.device.lock().expect("e2ee mutex poisoned");
        *device = restored;
        persist(&device)
    }
}

/// Load `room_id`'s MLS group from device storage, or a typed error if we don't
/// hold it (not an encrypted room we're a member of).
fn load_group(device: &Device, room_id: &str) -> Result<pigeon_crypto::Group, CoreError> {
    device
        .load_group(room_id.as_bytes())?
        .ok_or_else(|| CoreError::Crypto {
            reason: format!("no MLS group for {room_id}"),
        })
}

/// Decode a base64 wire blob (KeyPackage/Welcome/ciphertext); a bad encoding is a
/// crypto/protocol fault, not a storage one.
fn decode_wire(b64: &str, what: &str) -> Result<Vec<u8>, CoreError> {
    STANDARD.decode(b64).map_err(|e| CoreError::Crypto {
        reason: format!("{what} is not valid base64: {e}"),
    })
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
    fn group_welcome_round_trip_establishes_shared_membership() {
        set_key_store(Box::new(MemStore::default()));
        let alice = E2ee::create("@alice:test.example").unwrap();
        let bob = E2ee::create("@bob:test.example").unwrap();

        // Bob publishes a KeyPackage; Alice hosts the group and adds him.
        let bob_kp = bob.key_packages(1).unwrap().remove(0);
        alice.create_group("!room:test.example").unwrap();
        assert!(alice.has_group("!room:test.example").unwrap());
        assert!(!bob.has_group("!room:test.example").unwrap());

        let welcome = alice
            .add_member("!room:test.example", &bob_kp)
            .unwrap()
            .welcome;
        bob.join_from_welcome(&welcome).unwrap();

        // Both now hold the group for the room (group id = room id bytes).
        assert!(bob.has_group("!room:test.example").unwrap());
    }

    #[test]
    #[serial]
    fn encrypt_decrypt_round_trip_and_negatives() {
        set_key_store(Box::new(MemStore::default()));
        let alice = E2ee::create("@alice:test.example").unwrap();
        let bob = E2ee::create("@bob:test.example").unwrap();
        let carol = E2ee::create("@carol:test.example").unwrap();

        // Establish a shared group: alice hosts, bob joins via Welcome.
        let bob_kp = bob.key_packages(1).unwrap().remove(0);
        alice.create_group("!room:test.example").unwrap();
        let welcome = alice
            .add_member("!room:test.example", &bob_kp)
            .unwrap()
            .welcome;
        bob.join_from_welcome(&welcome).unwrap();

        // Alice → Bob round-trip.
        let ct = alice.encrypt("!room:test.example", "hello bob").unwrap();
        assert_eq!(bob.decrypt("!room:test.example", &ct).unwrap(), "hello bob");

        // Negative: an outsider (carol, no group) cannot decrypt — and can't even
        // encrypt (she doesn't hold the group).
        assert!(carol.decrypt("!room:test.example", &ct).is_err());
        assert!(carol.encrypt("!room:test.example", "sneak").is_err());

        // Negative: a tampered ciphertext fails cleanly (no panic).
        let mut tampered = ct.clone();
        tampered.insert(0, 'A'); // corrupt the base64/MLS bytes
        assert!(bob.decrypt("!room:test.example", &tampered).is_err());

        // Negative: valid base64 that isn't an MLS message.
        assert!(bob
            .decrypt("!room:test.example", &STANDARD.encode(b"garbage"))
            .is_err());
    }

    #[test]
    #[serial]
    fn third_member_commit_keeps_earlier_member_in_sync() {
        // finding C1: adding a *third* member advances the group epoch. The
        // earlier member (bob) must apply the broadcast commit or he falls a
        // ratchet epoch behind and can no longer decrypt.
        set_key_store(Box::new(MemStore::default()));
        let alice = E2ee::create("@alice:test.example").unwrap();
        let bob = E2ee::create("@bob:test.example").unwrap();
        let carol = E2ee::create("@carol:test.example").unwrap();

        // Alice hosts; bob joins (two-party — no existing members to notify).
        let bob_kp = bob.key_packages(1).unwrap().remove(0);
        alice.create_group("!room:test.example").unwrap();
        let add_bob = alice.add_member("!room:test.example", &bob_kp).unwrap();
        bob.join_from_welcome(&add_bob.welcome).unwrap();

        // Alice adds carol → a Welcome for carol AND a commit for the existing
        // member (bob).
        let carol_kp = carol.key_packages(1).unwrap().remove(0);
        let add_carol = alice.add_member("!room:test.example", &carol_kp).unwrap();
        carol.join_from_welcome(&add_carol.welcome).unwrap();

        // The commit does NOT apply to the author (already self-merged) nor to the
        // just-added member (already at the post-commit epoch via the Welcome) —
        // both error, which the sync loop swallows.
        assert!(
            alice
                .process_commit("!room:test.example", &add_carol.commit)
                .is_err(),
            "author must not re-apply its own commit"
        );
        assert!(
            carol
                .process_commit("!room:test.example", &add_carol.commit)
                .is_err(),
            "the added member is already at the new epoch"
        );

        // Alice encrypts at the new epoch. Bob, still a ratchet epoch behind,
        // cannot yet decrypt it (the negative that motivates C1).
        let ct = alice.encrypt("!room:test.example", "hi all").unwrap();
        assert!(
            bob.decrypt("!room:test.example", &ct).is_err(),
            "bob is an epoch behind until he applies the commit"
        );

        // Bob applies the broadcast commit → advances → now decrypts.
        bob.process_commit("!room:test.example", &add_carol.commit)
            .unwrap();
        assert_eq!(bob.decrypt("!room:test.example", &ct).unwrap(), "hi all");
        // Carol (correctly at the new epoch) also decrypts.
        assert_eq!(carol.decrypt("!room:test.example", &ct).unwrap(), "hi all");

        // A garbage commit fails cleanly (never panics / wedges).
        assert!(bob
            .process_commit("!room:test.example", &STANDARD.encode(b"garbage"))
            .is_err());
    }

    #[test]
    #[serial]
    fn join_from_garbage_welcome_fails_cleanly() {
        set_key_store(Box::new(MemStore::default()));
        let bob = E2ee::create("@bob:test.example").unwrap();
        // Not base64.
        match bob.join_from_welcome("!!!not base64!!!") {
            Err(CoreError::Crypto { .. }) => {}
            other => panic!("expected Crypto error, got {other:?}"),
        }
        // Valid base64 but not an MLS Welcome.
        match bob.join_from_welcome(&STANDARD.encode(b"garbage")) {
            Err(CoreError::Crypto { .. }) => {}
            other => panic!("expected Crypto error, got {other:?}"),
        }
    }

    #[test]
    #[serial]
    fn backup_restores_groups_and_rejects_wrong_recovery_key() {
        set_key_store(Box::new(MemStore::default()));
        let alice = E2ee::create("@alice:test.example").unwrap();
        alice.create_group("!room:test.example").unwrap();
        let (recovery_key, blob) = alice.create_backup().unwrap();

        // A fresh device (new identity) restores from the backup and recovers the
        // group — proving the encrypted blob round-trips the whole MLS state.
        let restored = E2ee::create("@alice:test.example").unwrap();
        assert!(!restored.has_group("!room:test.example").unwrap());
        restored.restore_from_backup(&recovery_key, &blob).unwrap();
        assert!(restored.has_group("!room:test.example").unwrap());

        // Negative (required for crypto): a wrong recovery key fails cleanly.
        let other = E2ee::create("@alice:test.example").unwrap();
        let wrong_key = STANDARD.encode([0u8; 32]);
        assert!(other.restore_from_backup(&wrong_key, &blob).is_err());
        // Garbage blob also fails cleanly.
        assert!(restored
            .restore_from_backup(&recovery_key, &STANDARD.encode(b"nope"))
            .is_err());
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
