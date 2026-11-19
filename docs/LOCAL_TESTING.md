# Local testing: the mobile test lanes

How to test the mobile client on one Mac, from the fast core tests up to the apps running
on a simulator and an emulator. Each lane says what it proves, what it needs, and the exact
command.

**Hands-on testing with a real server** (starting the docker-compose homeserver, two
devices holding a conversation, wiping app data, backup/restore) is the server repo's
runbook: [`../pigeon/docs/LOCAL_TESTING.md`](../../pigeon/docs/LOCAL_TESTING.md). It walks
through these apps' screens by name. It isn't repeated here; keep it current from this
repo (see `CLAUDE.md`, "Documentation Sync Rule").

---

## 0. Prerequisites

| What | Needed for | Check |
|---|---|---|
| `../pigeon` checked out as a sibling | everything (the core path-depends on `pigeon-core` / `pigeon-crypto`) | `ls ../pigeon/crates/crypto` |
| Rust (rustup) | the core, e2e, iOS | `cargo --version` |
| Docker Desktop | the `e2e/` lane and the Android build container | `docker info` |
| Xcode + an iOS simulator runtime | the iOS lanes | `xcrun simctl list runtimes` |
| Android SDK + emulator (`~/Library/Android/sdk`) | running the Android app | `~/Library/Android/sdk/emulator/emulator -list-avds` |
| `cargo-deny` | the supply-chain lane | `cargo deny --version` |

There is no JDK on this Mac, so the Android **build** runs in the repo's `android` Docker
container (§3). The Android **emulator** runs on the host.

**Once per clone, enable the docs pre-commit hook** (`.githooks/pre-commit`, see
CLAUDE.md's Documentation Sync Rule for what it checks):

```bash
git config core.hooksPath .githooks
# a genuine false positive (the change really needs no doc update):
SKIP_DOCS_CHECK=1 git commit ...
```

## Which lane for which change

| You changed… | Run |
|---|---|
| core logic (`core/src/*`) | §1, plus §2 if it touches a protocol flow |
| a dependency or `core/deny.toml` | §1 and `cargo deny check` (§1), then update [`SECURITY_REVIEW.md`](SECURITY_REVIEW.md) |
| the FFI surface | §1, §3 (`assembleDebug` regenerates the Kotlin bindings), §4 (`build-core.sh` regenerates the Swift ones) |
| Android UI / view models | §3 |
| iOS UI / view models | §4 |
| a view model, state field or message on one platform | both §3 and §4. The platforms change together (`CLAUDE.md`, "The UI Rule") |

## 1. The core (no device, no Docker)

```bash
cd core
cargo test                                   # unit + mock-HTTP tests
cargo clippy --all-targets -- -D warnings    # CI gates on this
cargo fmt --check
cargo deny check                             # advisories, licenses, sources
```

`cargo test` runs the unit tests in `src/` and the two integration files in `tests/`
(`http_mock.rs`: request shapes and error mapping against a `wiremock` server;
`session_persist.rs`: keystore persistence). A warm run takes about 1.5 s
([`BENCHMARKS.md`](BENCHMARKS.md)). This is the lane CI's `core` and `deny` jobs run.

## 2. End to end against a real homeserver (needs Docker)

```bash
cd e2e
cargo test
```

The `e2e/` crate starts the real `pigeon` server in-process with a real Postgres
(`testcontainers`) and drives the core's FFI functions the same way the apps do:

- `register_login_restore_against_real_homeserver`: session, restore, logout and
  server-side token revocation
- `two_clients_hold_a_plaintext_conversation`: create, invite, join and a round trip over
  `/sync` (the M2 gate)
- `two_clients_exchange_encrypted_messages`: an MLS invite with a Welcome, decryption, and
  a check that the server holds only ciphertext (the M3 gate)

It lives outside `core/` so the fast lane never needs Docker, and it is **not in CI**. Run
it after any change to a protocol flow, and after pulling server changes into `../pigeon`.
Docker Desktop must be running. The first build compiles the whole homeserver.

## 3. Android

**Build** in the container (the host has no JDK):

```bash
docker compose up -d android
docker compose exec android bash -c 'cd android && ./gradlew assembleDebug'           # core → .so, bindings, APK
docker compose exec android bash -c 'cd android && ./gradlew testDebugUnitTest'       # JVM view-model / wording tests
```

`assembleDebug` cross-compiles the core for `arm64-v8a` and `x86_64`, regenerates the
UniFFI Kotlin bindings, and packages
`android/app/build/outputs/apk/debug/app-debug.apk`. `testDebugUnitTest` runs the pure
wording and merge functions (`AuthErrorTest`, `ChatErrorTest`, `BackupFeedbackTest`,
`TimelineMergeTest`). CI runs only `assembleDebug`, so run the JVM tests yourself. The
container runs as `linux/amd64` under Rosetta: a cold build is slow, and warm caches live
in named volumes.

**Run** on the host emulator (AVD `Medium_Phone_API_36`, arm64):

```bash
SDK=~/Library/Android/sdk
$SDK/emulator/emulator -avd Medium_Phone_API_36 -netdelay none -netspeed full &
$SDK/platform-tools/adb wait-for-device shell 'while [ "$(getprop sys.boot_completed)" != "1" ]; do sleep 1; done'
$SDK/platform-tools/adb install -r android/app/build/outputs/apk/debug/app-debug.apk
$SDK/platform-tools/adb shell am start -n com.pigeon.mobile/.MainActivity
$SDK/platform-tools/adb logcat | grep pigeon/            # the core's log lines
```

The app should open on "Sign in to Pigeon" with `http://10.0.2.2:8008` filled in, and
logcat should show `pigeon/pigeon_mobile_core: PigeonApp: core callbacks installed`. If you
sign in and land on a bare "Signed in as …" screen, the APK is stale. Rebuild it.

## 4. iOS (macOS + Xcode)

```bash
ios/build-core.sh      # core → PigeonCoreFFI.xcframework + Swift bindings (after any core change)
ios/run-tests.sh       # build the Pigeon app, run its unit tests on a simulator
ios/run-app.sh         # build, install and launch it; assert it boots and stays up
```

- `build-core.sh` builds the core in release mode for device arm64 and a fat
  arm64/x86_64 simulator slice, then generates the Swift bindings. Both outputs are
  git-ignored.
- `run-tests.sh` runs `PigeonTests`: the real-Keychain `KeychainKeyStoreTests` and
  `AuthErrorTests`. It ad-hoc signs the app so the Keychain entitlement is present. Android's
  `ChatErrorTest`, `BackupFeedbackTest` and `TimelineMergeTest` have no iOS mirror yet.
- `run-app.sh` launches the app and checks the unified log for
  `PigeonApp: core callbacks installed`, then confirms the process is still alive a few
  seconds later.

Both scripts use a dedicated simulator, **`pigeon-sim`**. They create it on first use,
picking the newest iOS runtime together with an iPhone that runtime supports. All three
scripts run in CI's `ios` job.

To watch the app's logs on the simulator:

```bash
xcrun simctl spawn booted log stream --level info --predicate 'subsystem == "com.pigeon.mobile"'
```

## 5. Before committing

`CLAUDE.md`'s Commit Rule sets the gate: §1 for any core change, plus `assembleDebug` when
the FFI surface or Android code changed, plus the iOS scripts when Swift code changed.
Protocol-flow changes also need §2.
