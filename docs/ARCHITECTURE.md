# ARCHITECTURE — Pigeon Mobile

The structural map of the Pigeon mobile client: *what parts exist, where, and how they fit*. For the rules and conventions see [`../CLAUDE.md`](../CLAUDE.md); for the plan see [`../ROADMAP.md`](../ROADMAP.md). For the **protocol and the reused crates**, the authority is the homeserver repo ([`../../pigeon`](../../pigeon)).

> Status note: this describes the **target** architecture. The project is at the end of Phase M0 (foundations). **Built so far:** the `core/` crate and its UniFFI surface (`core_version`, `self_test_crypto`, `CoreError`, plus the `LogSink`/`set_log_sink`/`emit_test_log` log callback); cargo-ndk cross-compile to `arm64-v8a` + `x86_64` `.so`s; the Hello-core Jetpack Compose app driving the core through the generated Kotlin; Gradle build glue that makes `./gradlew assembleDebug` rebuild core + regen bindings + package the `.so` in one command; a two-lane GitHub Actions CI; and a Docker dev container. **The whole Rust→cargo-ndk→UniFFI→Kotlin→APK pipeline round-trips in the build** (M0.1–M0.7), verified by `cargo test`/`clippy`/`fmt` + `assembleDebug` in the containers; the only unticked M0 item is a visual run on an emulator. **Phase M1 is underway (M1.1–M1.4 built):** `api.rs` is the reqwest+rustls Client–Server HTTP client (base-URL/token config, `get`/`post`/`put`, `P_`-error → typed `ApiError`/`ErrorCode`); `session.rs` adds the async FFI `register`/`login`/`restore_session` returning a `PigeonClient` object that holds the token in-core and exposes only the `Session` identity — the first async FFI surface (UniFFI tokio runtime → Kotlin `suspend` fns), with `CoreError` mapping `ApiError` across the boundary. Session persistence (M1.3) is done: a host `KeyStore` callback (installed via `set_key_store`) stores the session blob at rest; `restore_session` reloads and validates it offline-first. The auth UI (M1.4) is built: the Hello-core screen is replaced by a real Compose auth flow (`android/app/.../auth/` — `AuthScreen`/`HomeScreen`/`AuthViewModel`/`AuthError`), with `PigeonApp` (an `Application`) installing the log sink and the `AndroidKeyStore` (EncryptedSharedPreferences → Android Keystore) at process start; the VM drives `register`/`login`/`restore_session` and the error mapper branches on the typed `ErrorCode`. The rest of M1 (logout M1.5) and everything past it (sync, rooms, e2ee, media, iOS) is not built yet. Keep this doc in sync as code lands (CLAUDE.md doc-sync rule).

## 1. The one big idea

Everything that is **not UI** lives once, in a shared **Rust core**, and is called from each platform through **UniFFI**-generated bindings. The platforms add only UI and OS integration.

```
┌─────────────────────────────────────────────────────────────────┐
│                    pigeon-mobile-core  (Rust)                     │
│                                                                   │
│   reuses:   pigeon-crypto  (MLS / openmls — client-side E2EE)     │
│             pigeon-core    (event types, canonical JSON, hashing) │
│                                                                   │
│   owns:     session/auth · the /sync loop · room & timeline model │
│             key mgmt (upload/query/claim) · to-device/Welcome     │
│             encrypted backup/restore · media · local SQLite store │
│             the Client–Server HTTP API client (reqwest+rustls)    │
│                                                                   │
│   exposes:  a small, typed UniFFI surface (functions + observers) │
└─────────────────────────────────────────────────────────────────┘
        │  UniFFI codegen                       │  UniFFI codegen
        ▼  Kotlin bindings + per-ABI .so        ▼  Swift bindings + xcframework
┌───────────────────────────────┐    ┌───────────────────────────────┐
│   android/  (Kotlin)          │    │   ios/  (Swift, Phase M5)     │
│   Jetpack Compose UI          │    │   SwiftUI                      │
│   view-models → core          │    │   view-models → core          │
│   FCM · Keystore · pickers    │    │   APNs · Keychain · pickers    │
└───────────────────────────────┘    └───────────────────────────────┘
                         │ HTTPS, token auth
                         ▼
        Pigeon homeserver   /_pigeon/client/v1/*   (one homeserver per user)
```

**Why this shape:** the client must do MLS E2EE on-device, and a tested MLS engine already exists in `pigeon-crypto` (Rust). Reimplementing it in Kotlin and again in Swift would duplicate security-critical crypto and risk federation incompatibility. Sharing the Rust core means crypto and protocol logic exist once; iOS becomes mostly UI work. This is the `matrix-rust-sdk` model.

## 2. Trust & data-flow boundaries

Three boundaries, each with a distinct trust posture:

1. **App ↔ Core (the FFI boundary).** Same process, same trust domain, but a *real cost boundary* (marshalling, codegen). Keep it **coarse and typed**: "load timeline page," "send message," "run one sync → diff," not per-field chatter. Secrets (private keys, tokens) stay *inside* the core + platform keystore — they do not cross into long-lived app-level state. (CLAUDE.md Gotcha #1, #7.)
2. **Core ↔ Homeserver (the network boundary).** HTTPS, token auth, the Client–Server API only. The network is hostile and flaky → offline-first: the store answers reads, the sync loop reconciles, sends queue and retry.
3. **Homeserver ↔ Federation (not our boundary).** The app never speaks federation. Cross-server key fetch, message delivery, and media fetch are the *server's* responsibility. The client only ever talks to its own homeserver.

End-to-end encryption means the homeserver — and every server in the federation — sees only ciphertext for encrypted rooms. The plaintext exists only inside the core (and the local store) on the device.

## 3. Repository layout

```
pigeon-mobile/
├── CLAUDE.md                     # rulebook
├── ROADMAP.md                    # plan
├── README.md                     # front door (added when runnable)
├── docs/
│   └── ARCHITECTURE.md           # this file
├── core/                         # pigeon-mobile-core — the shared Rust crate
│   ├── Cargo.toml                # crate-type = ["cdylib","staticlib"]; deps incl. pigeon-crypto, pigeon-core
│   ├── pigeon_mobile_core.udl    # UniFFI interface (or proc-macro attrs in lib.rs)
│   ├── build.rs                  # UniFFI scaffolding generation
│   └── src/
│       ├── lib.rs                # crate root + UniFFI setup + public re-exports
│       ├── ffi.rs                # UniFFI types: records, enums, errors, observer callbacks
│       ├── api.rs                # Client–Server HTTP client (reqwest+rustls), P_-error mapping
│       ├── session.rs            # register/login/logout, token+device_id lifecycle
│       ├── sync.rs               # the /sync long-poll loop, token handling, store diffing
│       ├── store.rs              # local persistence (SQLite): rooms, timeline, sync token
│       ├── rooms.rs              # room list, create/join, timeline reads, send + send-queue
│       ├── e2ee.rs               # thin wrapper over pigeon-crypto: groups, encrypt/decrypt, welcomes
│       ├── keys.rs               # device keys + KeyPackage upload/query/claim
│       ├── backup.rs             # encrypted device-state backup/restore (recovery key)
│       └── media.rs              # media upload/download/thumbnails (opaque/encrypted)
├── android/                      # the Android app
│   ├── settings.gradle.kts
│   ├── app/                      # Compose UI, view-models, navigation, DI
│   └── core-bindings/            # generated Kotlin + bundled .so per ABI (build output)
└── ios/                          # the iOS app (Phase M5)
    ├── PigeonMobile.xcodeproj
    └── Sources/                  # SwiftUI + generated Swift bindings + xcframework
```

`core/` must build and unit-test on the host with no device. It contains **no** platform APIs; the host installs side-effecting capabilities (logging, maybe keystore access) via UniFFI callback interfaces.

## 4. The shared core (`core/`)

### 4.1 Reused server crates

| Crate (from `../../pigeon`) | What the mobile core uses it for |
|---|---|
| `pigeon-crypto` | The client-side MLS engine: `Device`, group create/join, `add_member`→Welcome, `encrypt`/`decrypt`, export/restore storage, encrypted backup (recovery key). The core does **not** reimplement any of this. |
| `pigeon-core` | Event types, content structs, canonical JSON (for anything signed), event-id hashing — to construct/interpret events identically to the server. |

Depend on these by path (monorepo-adjacent) or pinned git rev — decided and documented in M0.1. The mobile client must track a **server protocol version** and update deliberately on a `v1→v2` bump.

### 4.2 Module responsibilities

- **`api.rs`** — the only place that makes HTTP calls. Owns the homeserver base URL, token injection, ret/timeout policy, and maps `P_*` error codes → typed core errors. Models its calls on the reference CLI (`../../pigeon/clients/cli`).
- **`session.rs`** — register/login/logout; produces a session (token, device_id, server). Secrets handed to the platform keystore via a callback; never persisted in clear.
- **`sync.rs`** — the long-poll `/sync` loop. Treats the composite sync token as opaque (Gotcha #5), diffs the response into the store, emits change events to the host observer, and propagates cancellation when the app backgrounds (Gotcha #6).
- **`store.rs`** — local SQLite. Source of truth for reads (offline-first). Holds rooms, timeline events (incl. **decrypted plaintext cached on first decrypt** — Gotcha #3), membership, and the sync token.
- **`rooms.rs`** — room list, create/join, paginated timeline reads, send (with local echo + a retrying send queue).
- **`e2ee.rs`** — the bridge to `pigeon-crypto`: maintains the per-room MLS group (group_id = room_id bytes), encrypts outbound to `p.room.encrypted`, decrypts inbound, processes `p.mls.welcome`. Idempotent on at-least-once to-device delivery (Gotcha #8).
- **`keys.rs`** — generates/publishes device identity + KeyPackages (`/keys/upload`); queries/claims peers' keys (`/keys/query`, `/keys/claim`).
- **`backup.rs`** — wraps `pigeon-crypto`'s recovery-key backup: produce a recovery key + encrypted blob, store via the key-backup endpoints, and restore identity+groups on a fresh device.
- **`media.rs`** — upload (size-cap aware → 413), download by `pigeon://server/id` URI, thumbnails; client-side encryption for encrypted rooms (server stores opaque bytes).
- **`ffi.rs`** — the UniFFI surface: records (DTOs the UI renders), error enums, and **observer/callback interfaces** for the host (sync change stream, logging sink, keystore access).

### 4.3 The FFI surface (illustrative — not final)

Keep it small, coarse, and stable. Sketch:

```
// session  — register/login/restore_session IMPLEMENTED (M1.2/M1.3); logout is M1.5
async fn register(server, username, password) -> PigeonClient   // token stays in-core
async fn login(server, user, password) -> PigeonClient          // ↑ Gotcha #1
//   PigeonClient.session() -> Session { user_id, device_id, server }   // no token
async fn restore_session() -> PigeonClient?  // reload+validate via keystore on launch (M1.3, offline-first)
fn set_key_store(store: KeyStore)            // install the platform-keystore backend once at startup (M1.3)
async fn logout()                            // (M1.5)

// sync (async; emits diffs to the observer)
fn start_sync(observer: SyncObserver)        // long-poll loop, cancelable
fn stop_sync()

// rooms & timeline (reads served from the local store)
fn rooms() -> [RoomSummary]
fn timeline(room_id: String, page: PageCursor) -> TimelinePage
fn create_room(opts: CreateRoomOpts) -> String   // returns room_id
fn join_room(room_id_or_alias: String)
fn invite(room_id: String, user_id: String)
fn send_message(room_id: String, body: String)   // plaintext OR encrypted, decided by room state

// e2ee is transparent: send/receive route through e2ee.rs based on the room's
// p.room.encryption marker — the UI does not branch on encryption.

// media
fn upload_media(bytes: [u8], content_type: String) -> String   // returns pigeon:// URI
fn download_media(uri: String) -> MediaBlob

// backup / restore
fn create_backup() -> RecoveryKey
fn restore_from_backup(recovery_key: String)

// host-provided capabilities (callback interfaces the app implements)
interface SyncObserver { fn on_change(diff: SyncDiff); fn on_error(e: CoreError); }
interface LogSink      { fn log(level: LogLevel, target: String, msg: String); }
interface KeyStore     { fn put(key, value: [u8]); fn get(key) -> [u8]?; fn delete(key); }   // IMPLEMENTED (M1.3); throws KeyStoreError
```

Encryption is **transparent**: `send_message`/timeline reads route through `e2ee.rs` based on the room's `p.room.encryption` marker. The UI never branches on "encrypted vs not" beyond showing an indicator.

## 5. The Android app (`android/`)

- **UI:** Jetpack Compose. Screens: homeserver/auth, room list, chat/timeline, room create, invite, media viewer, settings, backup/restore.
- **View-models:** translate UI intent → coarse core calls; collect the core's `SyncObserver` diffs into Compose state (a `Flow`/`StateFlow`). No protocol logic here — only presentation.
- **OS integration:** Android Keystore (implements the core's `KeyStore` callback), FCM (push → wake/sync), photo/file pickers, sharing, foreground/background lifecycle (drives `start_sync`/`stop_sync`), biometric unlock (optional).
- **Bindings:** generated Kotlin + the per-ABI `.so` (`arm64-v8a`, `armeabi-v7a`, `x86_64` for the emulator), produced by the build pipeline (§7).

## 6. The iOS app (`ios/`, Phase M5)

Same core, packaged as an `xcframework` with UniFFI Swift bindings. SwiftUI UI + Apple OS glue: Keychain (the `KeyStore` callback), APNs, native pickers/sharing, background refresh driving the sync loop. **No protocol or crypto code in Swift** — if any is needed, the FFI boundary leaked and the fix belongs in the core (benefiting Android too).

## 7. Build & codegen pipeline

```
   core/ (Rust)
      │  1. cargo build  (host: tests;  cargo-ndk: per-ABI .so;  iOS targets: static libs)
      │  2. uniffi-bindgen  →  Kotlin bindings (Android)  /  Swift bindings + xcframework (iOS)
      ▼
   android/  ← bundles .so + Kotlin bindings;  ./gradlew assembleDebug
   ios/      ← links xcframework + Swift bindings;  xcodebuild
```

- **Dev containers (`docker/Dockerfile` + `docker-compose.yml`):** persistent environments for continuous build/test. The host's `projects/` dir is bind-mounted at `/workspace` so the `../pigeon` path-deps resolve unchanged; cargo registry/target and the Gradle cache live in named volumes for caching across restarts. The image carries the full toolchain: Rust + `rustfmt`/`clippy` + Android targets + `cargo-ndk`, JDK 17, Android SDK (platform/build-tools 34) + NDK 26, and Gradle. **Two services** because the NDK ships only x86_64 Linux binaries:
  - `dev` — native arch (fast). Day-to-day core work:
    ```
    docker compose up -d dev
    docker compose exec -w /workspace/pigeon-mobile/core dev cargo test
    docker compose exec -w /workspace/pigeon-mobile/core dev \
      cargo run --bin uniffi-bindgen -- generate \
        --library target/debug/libpigeon_mobile_core.so --language kotlin \
        --out-dir target/bindings/kotlin
    ```
    (Run bindgen from the crate dir — it shells out to `cargo metadata`.)
  - `android` — `platform: linux/amd64` (Rosetta-emulated on Apple Silicon), so the NDK's x86_64 toolchain runs. The cross-compile + app build lane:
    ```
    docker compose up -d android
    docker compose exec android bash -c 'cd android && ./gradlew assembleDebug'
    ```
  Each service has its own `core/target` volume so amd64 and native artifacts don't mix. (CI runs both lanes on native amd64 Linux runners — no emulation there.)
- **Android cross-compile:** `cargo-ndk` (NDK version + Rust targets + min SDK documented in M0). One Gradle task rebuilds the core, runs codegen, and bundles the `.so`s so `assembleDebug` is a single command.
- **iOS cross-compile:** `cargo` for `aarch64-apple-ios` (+ simulator), packaged into an `xcframework`.
- **CI lanes:** **core** (`cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`), **android** (codegen + `assembleDebug` + lint), and later **ios** (xcframework + build). Aggressive caching.
- **End-to-end lane (`e2e/`):** a **separate crate** (not part of `core/`) that path-deps on `core` + the server's `tests-integration` harness. It boots the real `pigeon` server in-process over a TCP socket (real Postgres via **testcontainers**) and drives the core's FFI against it — the real-wire-contract proof the mock tests can't give. **Requires Docker**, so it is deliberately excluded from `core/`'s build and the core CI lane (the dev container has no Docker socket). Run on a host/CI with Docker: `cd e2e && cargo test`. First proof: the M1 register/login/restore flows (M1.2/M1.3).

## 8. End-to-end flows (how the pieces cooperate)

**Login (M1).** UI → `login()` → `api.rs` calls `/_pigeon/client/v1/login` → session returned → token+device_id stored in Keystore via the `KeyStore` callback → `start_sync()` begins.

**Receiving a message (M2/M3).** `sync.rs` long-polls `/sync` → for an encrypted room, `e2ee.rs` decrypts the `p.room.encrypted` event via `pigeon-crypto` → **plaintext cached in the store** (Gotcha #3) → `store.rs` diff → `SyncObserver.on_change` → view-model updates Compose state → chat screen re-renders.

**Sending an encrypted message (M3).** UI → `send_message()` → `rooms.rs` sees the room's `p.room.encryption` marker → `e2ee.rs` encrypts via the room's MLS group → `api.rs` sends `p.room.encrypted` → local echo into the store → render. The server stores only ciphertext.

**Inviting to an encrypted room (M3).** `invite()` → `keys.rs` claims the invitee's KeyPackage (`/keys/claim`) → `e2ee.rs` `add_member` → resulting `Welcome` shipped over `/sendToDevice` (type `p.mls.welcome`). The invitee's core picks it up from `/sync` `to_device`, joins the group idempotently.

**Device recovery (M4).** `create_backup()` → `backup.rs` produces a recovery key + encrypted blob (via `pigeon-crypto`) → stored via the key-backup endpoints. On a new device: `restore_from_backup(recovery_key)` → fetch blob → decrypt → identity + groups recovered → encrypted history decryptable from the backed-up epoch onward (forward-secrecy caveat documented).

## 9. Where things live (quick index)

| Concern | Lives in |
|---|---|
| MLS crypto | `pigeon-crypto` (reused), wrapped by `core/src/e2ee.rs` |
| Event types / canonical JSON | `pigeon-core` (reused) |
| HTTP / protocol calls | `core/src/api.rs` |
| Sync loop | `core/src/sync.rs` |
| Local data | `core/src/store.rs` (SQLite) + platform keystore for secrets |
| FFI types & callbacks | `#[uniffi::export]` in the relevant module (proc-macro mode — no `.udl`); shared records/errors collect in `core/src/ffi.rs` |
| Android UI | `android/app/` |
| iOS UI | `ios/Sources/` (M5) |
| Build/codegen | `docker/` dev container, the `uniffi-bindgen` bin (`core/src/bin/`), `cargo-ndk` (M0.3), Gradle tasks |
| Protocol contract (authoritative) | `../../pigeon` repo + its `clients/cli` |

## 10. Open decisions (resolve as phases reach them)

- ~~Core dependency on the server crates: path vs pinned git rev~~ → **path deps** (`../../pigeon/crates/*`), via the `/workspace` bind mount in the dev container (M0.1).
- ~~UniFFI style: `.udl` vs proc-macro~~ → **proc-macro / library mode** (`uniffi::setup_scaffolding!()`, UniFFI 0.28), bindings generated from the built cdylib (M0.2).
- Local store: `sqlx` vs `rusqlite` (M2). — flag the dep.
- Server discovery (`.well-known`) in the core: M1 or later.
- Push contract: what the homeserver exposes for push (confirm with the server repo before M4.4).
- Android min SDK / NDK version pin (M0.3).