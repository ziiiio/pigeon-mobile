# Security register (mobile client)

This is the mobile client's security record, the counterpart of the homeserver's
[`../pigeon/docs/SECURITY_REVIEW.md`](../../pigeon/docs/SECURITY_REVIEW.md). It lists the
security findings and how they were fixed, the properties checked and found clean, the
open items, the weaknesses accepted on purpose, and every `cargo-deny` advisory the core
ignores. It is a register, not a description of the code. **Add** new findings with their
date, commit and test. Keep the living sections (advisories, Clean, Open, Accepted) true.
Never rewrite a dated finding.

**Scope.** The shared Rust core (`core/`) and both native apps (`android/`, `ios/`). The
server's own findings live in the server's register. A server fix that changed what the
client must do appears here only for its client-side half.

**How it was built.** First compiled **2026-11-16**. Two sources: the client's fix history
(the review findings P1–P6 of 2026-07-07, the `../pigeon` CORRECTNESS_REVIEW
reconciliation of 2026-07-16, and the advisory passes), plus a targeted check of each
claim under "Clean" against the code. That check was not a full audit. The external audit
belongs to M6 (see `ROADMAP.md`).

## Fixed

| # | Date | Severity | Finding | Fix | Test |
|---|---|---|---|---|---|
| P1 | 2026-07-07 | **High** | **Plaintext downgrade into an encrypted room.** `flush_pending` picked the plaintext path whenever the device held no MLS group for the room. It never checked the room's encryption marker. A member who had joined an encrypted room but not yet processed its Welcome sent message bodies in the clear as `p.room.message` (Gotcha #1). | `flush_pending` checks `Store::is_room_encrypted` before the plaintext fallback. A send into an encrypted room without a group stays queued and goes out encrypted once the group is joined. It is never sent as plaintext. (`77b1a01`) | `store::is_room_encrypted_reflects_the_marker`, `tests/http_mock.rs::send_into_an_encrypted_room_without_a_group_is_held_not_downgraded` |
| P2 | 2026-07-07 | Medium (E2EE availability) | **Sync token saved before Welcomes were processed.** The composite `next_batch` also acknowledges the to-device position, and the server then deletes those messages. Saving the token before `apply_to_device` processed a Welcome meant a crash in between lost that Welcome. The room then stayed undecryptable for good. | `apply_sync` only folds events. `run_sync` calls `persist_sync_token` last, after Welcomes are processed. Every step before it is idempotent, so a redelivery is safe. (`927d579`) | `sync::apply_sync_folds_events_and_advances_token`, `sync::apply_sync_advances_token_on_empty_batch` (split contract: fold, then persist) |
| P5 | 2026-07-07 | Medium (E2EE availability) | **Fixed KeyPackage ids dropped a republished pool.** Ids were `kp-{index}`, and the server dedups on `(user, device, key_id)`. After an identity change (`restore_backup` mints one), the new packages were silently discarded, peers claimed packages of the discarded identity, and their Welcomes could never be opened. | Each `key_id` is a hash of the package (`key_package_id`). (`9346375`) | `api::key_package_id_is_content_addressed` |
| P6 | 2026-07-16 | Medium (E2EE availability) | **No last-resort KeyPackage.** The client uploaded only 5 one-time packages. After 5 claims the device could no longer be added to new encrypted groups, and invites shipped zero Welcomes. | `publish_device_keys` also uploads a reusable last-resort package (`last_resort: true`). (`85676e9`) | `e2ee::last_resort_key_package_is_usable_for_a_join`, `api::upload_keys_flags_the_last_resort_package` |
| C1 | 2026-07-16 | Medium (integrity of group state) | **MLS commits not broadcast or applied.** After the server-side `pigeon-crypto` hardening, adding a third member advances the group epoch. Earlier members that never applied the commit fell an epoch behind and could not decrypt. | `invite` broadcasts the commit as a `p.mls.commit` room event. The sync loop applies commits and decrypts messages in one DAG-ordered pass (`process_inbound_mls`). (`b536c45`) | `e2ee::third_member_commit_keeps_earlier_member_in_sync`, `store::pending_mls_interleaves_commits_and_messages_in_dag_order`, `rooms::invite_into_encrypted_room_broadcasts_the_commit` |
| — | 2026-10-04 | Medium (integrity) | **Restart-reused transaction ids.** Once the server honoured `/send` txn ids (`../pigeon` `21a0d1a`), the process-local `mob-td-{counter}` ids repeated after an app restart. A repeated commit broadcast would have been swallowed as a duplicate, leaving earlier members an epoch behind. | Ids are `mob-td-{millis}-{n}`. (`0e057d7`) | `rooms::txn_ids_never_repeat_and_carry_a_timestamp` |
| — | 2026-07-17 | Low (test gap) | **No tampered-backup negative test** (the server's C5). | Added: flipping a ciphertext byte must fail even with the correct recovery key. | `e2ee::backup_restores_groups_and_rejects_wrong_recovery_key` |
| — | 2026-11-19 (found 2026-11-16) | Medium (release blocker) | **Cleartext HTTP to any homeserver, on both platforms.** Android set `usesCleartextTraffic="true"`. Fixing that alone would not have helped: Android's cleartext policy is enforced by the platform HTTP stacks, not raw sockets, and the core's `reqwest` client opens its own, so a release build would still have sent the password and token over `http://`. iOS had the same gap (App Transport Security governs URLSession only). | Android: a network-security-config (release: no cleartext; debug override: `10.0.2.2`, `localhost`), and the auth view-model asks that same policy (`NetworkSecurityPolicy.isCleartextTrafficPermitted`) via `cleartextRefusal` before the address reaches the core. iOS: the same `cleartextRefusal` check with a `#if DEBUG` allowlist of `localhost`. (`de40915`) | Android `CleartextPolicyTest` (5) and iOS `CleartextPolicyTests` (5 mirrored + the Debug allowlist + exact wording); `aapt2 dump xmltree` of the debug APK shows the override and the manifest reference; the release APK's config is `cleartextTrafficPermitted=false` with no dev hosts. On the emulator (2026-11-19, fresh install): `http://10.0.2.2:8008` reaches the server; `http://example.com` is refused before any request |
| — | 2026-11-23 (found 2026-11-16) | Medium | **Decrypted message text sat unencrypted on disk.** The store was plain SQLite, protected only by the OS sandbox and file protection, so a rooted or jailbroken device or a forensic image could read decrypted history, our own sends in encrypted rooms and queued drafts. | Option 1 of the 2026-11-19 review (Element X's approach): `pigeon-crypto` gained `seal`/`open_sealed` (`../pigeon` `b960c07`); the store (schema v4) seals `events.decrypted`, own message bodies in encrypted rooms and `pending_sends.body` under a per-install key held only in the platform keystore (`pigeon.store.key.v1`), each bound to its row. The v4 migration seals existing stores in one transaction; `secure_delete` + a WAL checkpoint scrub the old plaintext; a sealed check value detects a lost key and rebuilds the cache; sign-out wipes the store and deletes the key. Limits: protects copies of the files, not code running on an unlocked device (it can use the keystore); metadata (room names, members, timestamps) is not sealed. (commit: `fix: seal the store's at-rest text (schema v4)`) | store: `text_the_server_never_saw_is_not_on_disk_in_clear` (reads the db + WAL bytes), `a_v3_store_migrates_to_sealed_v4` (and the old plaintext is gone from the file), `a_sealed_value_moved_or_tampered_fails_with_a_typed_error`, `reopening_under_a_different_key_clears_the_cache_instead_of_failing`, `wipe_empties_the_store`, `plaintext_room_echoes_stay_inline`; session: `logout_deletes_the_store_key`; e2e 3/3 |
| — | 2026-11-19 (found 2026-11-16) | Medium | **iOS store included in device and iCloud backups.** `pigeon.sqlite3` lived in Application Support with default protection and no backup exclusion, and it caches the **decrypted text** of encrypted messages (Gotcha #3), so that text could leave the device in a backup. | `StoreLocation.prepare` puts the store in `Application Support/PigeonStore/` with `isExcludedFromBackup` and `FileProtectionType.completeUnlessOpen` (an open database keeps working while the device is locked), and moves an older install's store and `-wal`/`-shm` in as a set, falling back to the old location on failure. (`2e49567`) | `StoreLocationTests` (5: created + excluded, legacy set moved, idempotent, live copy wins, fallback); on `pigeon-sim`: a seeded legacy store moved with its data and the directory carries `com.apple.metadata:com_apple_backup_excludeItem` |

Server-side hardening that needed **no** client change (C4 media key/nonce checks, strict
ed25519 verification) is recorded in `CLAUDE.md`'s server-drift reconciliation.

## Dependency scanning (`cargo-deny`)

`core/deny.toml` mirrors the server's policy: RUSTSEC advisories, a permissive-license
allowlist (the "no surprise deps" rule), and source restrictions. The `deny` CI job
(added 2026-11-13, `6f2bbb8`) fails on any **new** advisory. Locally, run
`cd core && cargo deny check`. *Last run 2026-11-20: advisories, bans, licenses and
sources all ok.*

Four advisories are accepted, each with its reason in `deny.toml`:

- **RUSTSEC-2026-0207 / -0208 `libcrux-sha3`**, **RUSTSEC-2026-0212 `libcrux-secrets`**:
  pulled by `pigeon-crypto` → `openmls_rust_crypto` 0.5.1 → `hpke-rs` 0.6.1. These are
  **unreachable**: libcrux is used only for the post-quantum KEMs, and Pigeon's only
  ciphersuite is `MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519`. These are the same
  entries as the server's.
- **RUSTSEC-2026-0387 `crypto-bigint`** 0.5.5: comes in only through the P-256/P-384/
  secp256k1 curves, so it is unreachable for X25519 + Ed25519. Same entry as the server's.
All four share one fix: the breaking **`openmls_rust_crypto` 0.6 / `hpke-rs` 0.7**
MLS-stack bump. That bump is shared with the server and tracked there.

**No longer accepted:** RUSTSEC-2026-0459 (bundled SQLite < 3.54, WAL page-size bug),
accepted 2026-11-13 because a *test* held the app's `rusqlite` at 0.31: the `e2e/` crate
linked the homeserver in-process, and Cargo allows one SQLite-linking package per binary,
which the server's (unused) `sqlx` SQLite backend capped below the fix. Fixed 2026-11-20
by running e2e's server out of process (no shared graph) and moving to **rusqlite 0.43 /
libsqlite3-sys 0.40 (SQLite 3.54.1)**. This is the precedent for CLAUDE.md's "tests never
pin dependencies" rule.

**Patched rather than accepted:** `url` 2.5.8 → 2.5.9 (RUSTSEC-2026-0592, stack overflow
in `Url::origin()` on nested `blob:` URLs; 2026-11-23, both lockfiles; same as the server's).
Earlier: `rustls`, `rustls-webpki`, `h2` and `crossbeam-epoch`
(2026-10-04, `df10024`); `rustls`, `rustls-webpki`, `webpki-roots` and `tokio`
(RUSTSEC-2026-0346..0349, -0381, -0382, -0386; 2026-11-13, `6f2bbb8`).

## Clean (checked: no action needed)

Checked against the code on 2026-11-16:

- **Secrets stay in the core and the platform keystore.** The access token and the MLS
  device state (`pigeon.session.v1`, `pigeon.mls.state.v1`) and the store key
  (`pigeon.store.key.v1`) are written only through the
  host `KeyStore` callback. Android stores them with `EncryptedSharedPreferences`
  (`pigeon_secure_prefs`, AES256-SIV keys / AES256-GCM values, under an Android Keystore
  master key). iOS stores them in the Keychain (`com.pigeon.mobile.keystore`,
  `kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly`: never synced, never restored to
  another device). Neither reaches the SQLite store or native view-model state. The UI only
  gets the non-secret `Session` record.
- **No crypto or protocol code in Kotlin/Swift.** Encryption, decryption, KeyPackages,
  Welcomes, commits, media keys and backup all run in the core through `pigeon-crypto`.
  The per-file media key goes from core to core inside the encrypted message, and
  `download_image` decrypts in the core.
- **The recovery key never goes to the server.** `backup()` uploads only the encrypted
  blob and returns the key once for the user to save. The Copy button flags the clip as
  sensitive on Android (hidden from the clipboard preview on API 33+). On iOS it is
  `localOnly` (no Universal Clipboard) and expires after 10 minutes.
- **No plaintext downgrade.** See P1. A room's encryption marker, not group presence,
  decides the path.
- **Log lines carry no message bodies, tokens or keys.** Every `crate::emit` call site
  logs only fixed text, counts, room and event ids, and error descriptions. See the
  "Accepted" entry on error text.
- **Android:** `android:allowBackup="false"`, so neither the store nor the encrypted prefs
  go into cloud or adb backups.
- **iOS:** the store directory (`Application Support/PigeonStore/`) is excluded from iCloud
  and device backups and uses `completeUnlessOpen` file protection (`StoreLocation`, fixed
  2026-11-19); the Keychain entries are `ThisDeviceOnly`.
- **TLS:** `reqwest` with `rustls` and its built-in roots. There is no certificate pinning
  and no way to turn verification off.

## Open (known, not yet fixed)

These are real gaps, recorded here so they aren't lost. Each is a hardening item for M6:

| Found | Severity | Gap | Direction |
|---|---|---|---|
| — | — | — | *(nothing open)* |

## Accepted (documented, not fixed)

- **History is lost after a restore (MLS forward secrecy).** A key backup restores the
  device's identity and group membership, not past message keys. Messages from before the
  backup show "⚠ Unable to decrypt". This is by design: MLS deletes each message key after
  use. The apps say this in the backup and restore dialogs.
- **A device can't decrypt its own messages.** The sender's readable copy is the local
  echo copied onto the confirmed event (`resolve_send`). Wiping the app loses it. This is
  an MLS property, not a bug.
- **Error descriptions go into logs, and on iOS they are logged `.public`.** Log lines
  include `{err}` (transport errors, `P_*` codes with the server's reason text, `openmls`
  errors). No call path logs a user id or message body today, but that depends on
  discipline, not structure. `OsLogSink` marks lines `.public` so they are readable in
  sysdiagnose. A new log call that puts a user id or body into an error would leak it, so
  review new `emit` call sites against Gotcha #2.
- **The Android clipboard has no expiry.** Android offers apps no expiring clip. The
  recovery key is flagged sensitive, and Android 13+ clears the clipboard on its own
  schedule.
