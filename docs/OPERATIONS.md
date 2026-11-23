# Operating the Pigeon mobile apps

A practical guide to what the apps keep on a device, how they behave on a real network,
how to read their logs, how they upgrade, and how CI builds them. It is the mobile
counterpart of the homeserver's
[`../pigeon/docs/OPERATIONS.md`](../../pigeon/docs/OPERATIONS.md). For what the apps
*are*, see [`README.md`](../README.md). For the internals, see
[`ARCHITECTURE.md`](ARCHITECTURE.md). For running and testing them on a Mac, see
[`LOCAL_TESTING.md`](LOCAL_TESTING.md).

## What you're running

Two native apps over one Rust library:

| | Android | iOS |
|---|---|---|
| App id | `com.pigeon.mobile` | `com.pigeon.mobile` |
| Version | `versionName 0.1.0` / `versionCode 1` | `MARKETING_VERSION 1.0` / build `1` |
| Minimum OS | API 24 (`targetSdk` 36) | iOS 15.0 |
| Core packaging | `libpigeon_mobile_core.so` per ABI (`arm64-v8a`, `x86_64`) | `PigeonCoreFFI.xcframework` (static library) |
| UI | Jetpack Compose | SwiftUI |

The apps talk to **one homeserver**, the one the user signs in to, over the
Client–Server API (`/_pigeon/client/v1/*`, `/_pigeon/media/v1/*`). They never speak
federation. There is **no push**: new messages arrive through the `/sync` long-poll while
the app runs, because the server has no push contract yet (ROADMAP M4.4).

## Configuration

The apps have no config file or settings screen. The only thing a user supplies is the
**homeserver URL** on the sign-in screen. The field is pre-filled:

| Where | Pre-filled URL | Why |
|---|---|---|
| Android (any build) | `http://10.0.2.2:8008` | the emulator's alias for the host's localhost |
| iOS simulator | `http://localhost:8008` | the simulator shares the Mac's network |
| iOS device | `https://` | a real phone has no local dev server |

Pigeon has no client-side server discovery (no `.well-known` for clients). Users type the
server's base URL directly.

Everything else is a compile-time constant in the core:

| Constant | Value | Where | Effect |
|---|---|---|---|
| `SYNC_TIMEOUT_MS` | 30 000 | `core/src/sync.rs` | how long one `/sync` long-poll waits on the server |
| `SYNC_LIMIT` | 100 | `core/src/sync.rs` | timeline events per room per sync batch |
| `BACKOFF_START` → `BACKOFF_MAX` | 1 s → 30 s (doubling) | `core/src/sync.rs` | retry delay after a transient sync error |
| `KEY_PACKAGE_COUNT` | 5 (+1 last-resort) | `core/src/e2ee.rs` | one-time MLS KeyPackages published per device |
| `MAX_UPLOAD_BYTES` | 50 MiB | `core/src/media.rs` | client-side media cap, matching the server's upload limit |

## Data on the device

| What | Android | iOS | Contains |
|---|---|---|---|
| Local store | `filesDir/pigeon.sqlite3` | `Application Support/PigeonStore/pigeon.sqlite3`, excluded from backup, `completeUnlessOpen` protection (moved from `Application Support/` on first launch of a newer build) | the event log, folded room state, the sync token, the send queue, **decrypted message text** (Gotcha #3). Text the server never saw in the clear (decrypted messages, our own sends in encrypted rooms, the send queue) is **sealed** under the store key (schema v4) |
| Session | `EncryptedSharedPreferences` `pigeon_secure_prefs`, key `pigeon.session.v1` | Keychain service `com.pigeon.mobile.keystore`, account `pigeon.session.v1` | user id, device id, homeserver URL, access token |
| MLS device state | same store, key `pigeon.mls.state.v1` | same service, account `pigeon.mls.state.v1` | the device's signing identity and all group secrets |
| Store key | same store, key `pigeon.store.key.v1` | same service, account `pigeon.store.key.v1` | the 128-bit key that seals the local store's at-rest text (created on first store open; deleted on sign-out) |

The host app passes the store directory in at launch (`set_store_dir`). If it is unset,
the core falls back to an in-memory store and logs a warning. The three secret entries go
only through the host `KeyStore` callback, never into the store. If the store key is lost
while the store file remains (for example the keystore was reset), the core detects it on
open, clears the local cache and rebuilds it from the server; previously decrypted history
is then gone, as after a reinstall. See [`SECURITY_REVIEW.md`](SECURITY_REVIEW.md) for how
each platform protects these entries.

**Wiping an app's data** works like losing the phone: the session and the MLS keys are
gone, and the user must sign in again and restore from a key backup. The exact commands
for both simulators are in the server repo's
[`LOCAL_TESTING.md`](../../pigeon/docs/LOCAL_TESTING.md) (§6). **Signing out** also
deletes the MLS state, clears the local store and deletes the store key: the next sign-in
is a new device with an empty store.

## Upgrades

- **Store migrations run automatically** when the store opens. The schema version lives in
  SQLite's `user_version` (currently **4**: v1 events/state/token, v2 local echo + send
  queue, v3 decrypted-text cache, v4 at-rest sealing). The v4 migration seals an existing
  store's plaintext in one transaction, then checkpoints the WAL; with SQLite's
  `secure_delete` on, the old plaintext doesn't linger in the file. Migrations are append-only, so a newer app upgrades an
  older store in place. A downgrade to an app with a lower version is not supported.
- **Keystore entries are versioned by name** (`…v1`). A format change must use a new key
  name and migrate, never reinterpret the old bytes.
- **MLS state survives an app update** because it is stored as an opaque `pigeon-crypto`
  export. A `pigeon-crypto` change that alters that format needs a migration path, or every
  user loses their groups.
- **Wire protocol:** the core speaks `/_pigeon/client/v1`. A server `v2` would be a
  deliberate client change (see `CLAUDE.md`, "Protocol Coupling").

## Network behaviour

The core is offline-first. Reads come from the local store, and the network catches up.

- **HTTPS only, except dev hosts in debug builds.** Sign-in and registration refuse an
  `http://` homeserver unless the build allows it: release builds allow none; debug builds
  allow `10.0.2.2` and `localhost` on Android (`src/debug/res/xml/network_security_config.xml`)
  and `localhost` on iOS. The app checks this itself (`cleartextRefusal`) because the
  core's `reqwest` client opens its own sockets, which Android's network-security-config
  and iOS's App Transport Security don't govern.

- **Sync loop.** `run_sync` long-polls `/sync` while the signed-in screen is open, and
  leaving the screen cancels it (Gotcha #6). The server answers a parked poll as soon as
  something arrives **for this user** (a message in one of their rooms, a membership change
  naming them, a to-device message), whether it was written locally or came from another
  server. Before server commit `b3ba1dd` (2026-11-18), messages from users on other
  servers waited out the full `SYNC_TIMEOUT_MS` poll. Transient errors back off from 1 s to 30 s and
  retry: network failures and **`429 P_LIMIT_EXCEEDED`** both count as transient. Only a
  fatal error (such as a revoked token) ends the loop. The room list then shows
  "offline" with a session error, and the user signs in again.
- **Sends.** Every message goes into a durable queue (`pending_sends`) and shows as a
  local echo ("Sending…"). The queue is flushed on send and on every sync cycle. A 429
  leaves the message queued. A server rejection marks it "Not sent". A message for an
  encrypted room whose group isn't joined yet stays queued until it can go out encrypted
  (never as plaintext). Each queued message keeps a restart-unique txn id, so the server's
  `/send` dedup makes retries safe.
- **Server rate limits** that apply to the apps (server defaults): 100 burst / 50 req/s per
  IP in general, and **5 burst / 0.2 req/s for `login` + `register`**. Repeated sign-in
  attempts hit the second limit quickly. Behind a proxy, the server needs
  `trusted_proxies`, or every client shares one bucket (server OPERATIONS).
- **TLS.** `reqwest` with `rustls` and its built-in roots. The OS trust store is not
  used, so a server whose certificate chains to a private CA won't connect.

## Logs

The core never writes logs itself. It sends structured lines through the `LogSink`
callback that the host installs at launch:

| Platform | Sink | Where to read it |
|---|---|---|
| Android | `LogcatSink` | logcat, tag `pigeon/<target>` (for example `pigeon/sync`, `pigeon/e2ee`, `pigeon/pigeon_mobile_core`): `adb logcat | grep pigeon/` (logcat filters don't accept tag wildcards) |
| iOS | `OsLogSink` | unified logging, subsystem `com.pigeon.mobile`, category `core`: `xcrun simctl spawn booted log stream --level info --predicate 'subsystem == "com.pigeon.mobile"'` |

Both apps log `PigeonApp: core callbacks installed` at startup (`ios/run-app.sh` checks
for it). Log lines contain room and event ids and error descriptions, never message
bodies, tokens or keys (Gotcha #2). The iOS sink marks lines `.public`, so keep that rule
strict.

**Telling problems apart from the logs** (targets `sync`, `e2ee`, `session`):

| Line | Meaning |
|---|---|
| `sync retryable error, backing off …` | offline or rate-limited. It recovers by itself. |
| `holding a queued message for encrypted room …` | the group's Welcome hasn't been processed yet. The message goes out once it is. |
| `failed to decrypt <event>` | expected for history from before a restore, and for the sender's own messages after a wipe (forward secrecy) |
| `device key upload failed (E2EE limited until next login)` | the device can't be added to new encrypted rooms until keys are republished |
| `no store dir set; using in-memory store` | the host forgot `set_store_dir`. Nothing survives a restart. |

## CI

`.github/workflows/ci.yml` runs five jobs on every push and PR. The homeserver repo is
private, so every job checks out `../pigeon` (for the path dependencies) with the
**`PIGEON_DEPLOY_KEY`** secret: the private half of a **read-only deploy key** on
`ziiiio/pigeon` (it can read that one repo and nothing else; revoke it under the pigeon
repo's Settings → Deploy keys). It replaced a fine-grained token, `PIGEON_REPO_TOKEN`,
which was never actually set, so the jobs that need `../pigeon` had failed at checkout
since 2026-10-04. To recreate it:

```bash
ssh-keygen -t ed25519 -N "" -C "pigeon-mobile CI (read-only)" -f /tmp/pigeon_ci_key
(cd ../pigeon && gh repo deploy-key add /tmp/pigeon_ci_key.pub --title "pigeon-mobile CI (read-only)")
gh secret set PIGEON_DEPLOY_KEY < /tmp/pigeon_ci_key && rm /tmp/pigeon_ci_key /tmp/pigeon_ci_key.pub
```

| Job | Runner | Runs |
|---|---|---|
| `core` | ubuntu | `cargo fmt --check`, `cargo clippy --all-targets -D warnings`, `cargo test`, plus a Swift-bindings generation smoke check |
| `deny` | ubuntu | `cargo deny check` over `core/` (register: [`SECURITY_REVIEW.md`](SECURITY_REVIEW.md)) |
| `android` | ubuntu | `./gradlew assembleDebug`, then `./gradlew testDebugUnitTest` (JDK 25, SDK 36, NDK r28c) |
| `e2e` | ubuntu | `cargo test` in `e2e/`: builds `pigeon-server` from `../pigeon` (`SQLX_OFFLINE=true`, against its committed `.sqlx` cache) and runs it out of process on a `postgres:16` container (the runner's Docker) |
| `ios` | macos-15 | `ios/build-core.sh`, `ios/run-tests.sh`, `ios/run-app.sh` |

Not in CI: running the apps on an emulator or simulator against a live homeserver (the
server repo's [`LOCAL_TESTING.md`](../../pigeon/docs/LOCAL_TESTING.md)), and
`assembleRelease`.

## Release readiness

The apps are built and run as **debug builds** only. Shipping is M6 work
(`ROADMAP.md`). Known gaps before a store release:

- **Android release builds are unsigned.** `assembleRelease` packages an optimized core
  (`cargoNdkBuildRelease`, `--release`; 8.0 MiB arm64 `.so`, a 24.1 MiB APK, see
  [`BENCHMARKS.md`](BENCHMARKS.md)), but produces `app-release-unsigned.apk`.
- **No signing configuration** on either platform (the iOS scripts ad-hoc sign for the
  simulator only). There is no R8/minify on Android.
- **ABIs:** Android builds `arm64-v8a` and `x86_64` only. `armeabi-v7a` (32-bit phones)
  is not built.
