# ARCHITECTURE — Pigeon Mobile

The structural map of the Pigeon mobile client: *what parts exist, where, and how they fit*. For the rules and conventions see [`../CLAUDE.md`](../CLAUDE.md); for the plan see [`../ROADMAP.md`](../ROADMAP.md); for a narrated, junior-developer walk through the actual code see [`CODEBASE_GUIDE.md`](CODEBASE_GUIDE.md). For the **protocol and the reused crates**, the authority is the homeserver repo ([`../../pigeon`](../../pigeon)).

> Status note: this describes the **target** architecture, now largely realised. **The project is through Phase M5 — M1 (identity), M2 (plaintext messaging), M3 (the headline E2EE feature), M4 (media, encrypted media, backup/restore, feasible polish) and M5 (the iOS parity app) are complete; M6 (hardening & release) is next.** The one unbuilt feature is push (M4.4/M5.3): the homeserver exposes no push/APNs/FCM contract, so it is blocked server-side, not deferred. See `ROADMAP.md` / `CLAUDE.md` for the authoritative per-stage status; the per-module sections below carry each module's built state. The M0/M1 detail that follows is retained for history. **Built in M0:** the `core/` crate and its UniFFI surface (`core_version`, `self_test_crypto`, `CoreError`, plus the `LogSink`/`set_log_sink`/`emit_test_log` log callback); cargo-ndk cross-compile to `arm64-v8a` + `x86_64` `.so`s; the Hello-core Jetpack Compose app driving the core through the generated Kotlin; Gradle build glue that makes `./gradlew assembleDebug` rebuild core + regen bindings + package the `.so` in one command; a two-lane GitHub Actions CI; and a Docker dev container. **The whole Rust→cargo-ndk→UniFFI→Kotlin→APK pipeline round-trips in the build** (M0.1–M0.7), verified by `cargo test`/`clippy`/`fmt` + `assembleDebug` in the containers, and (2026-07-04) the app runs on an **x86_64 API-36 emulator on a Linux/amd64 host** after a build-toolchain upgrade (Gradle 9.6.1 / AGP 9.2.1 / Kotlin 2.2.10 / JDK 25 / NDK r28c — see §7 and ROADMAP; the Docker/CI toolchain has since been bumped to match). **Phase M1 is complete (M1.1–M1.5 built):** `api.rs` is the reqwest+rustls Client–Server HTTP client (base-URL/token config, `get`/`post`/`put`, `P_`-error → typed `ApiError`/`ErrorCode`); `session.rs` adds the async FFI `register`/`login`/`restore_session` returning a `PigeonClient` object that holds the token in-core and exposes only the `Session` identity — the first async FFI surface (UniFFI tokio runtime → Kotlin `suspend` fns), with `CoreError` mapping `ApiError` across the boundary. Session persistence (M1.3) is done: a host `KeyStore` callback (installed via `set_key_store`) stores the session blob at rest; `restore_session` reloads and validates it offline-first. The auth UI (M1.4) is built: the Hello-core screen is replaced by a real Compose auth flow (`android/app/.../auth/` — `AuthScreen`/`HomeScreen`/`AuthViewModel`/`AuthError`), with `PigeonApp` (an `Application`) installing the log sink and the `AndroidKeyStore` (EncryptedSharedPreferences → Android Keystore) at process start; the VM drives `register`/`login`/`restore_session`/`logout` and the error mapper branches on the typed `ErrorCode`. Logout (M1.5) is done: `PigeonClient.logout()` revokes the token server-side (best-effort) then clears the keystore, wired to a Sign-out action in `HomeScreen` — proven end-to-end (local clear + real server-side revocation) by the `e2e/` oneshot lane. The stages past M1 (sync, rooms, e2ee, media, backup, and the iOS app) are all built too — see the per-module sections below and `ROADMAP.md` for their detail. Keep this doc in sync as code lands (CLAUDE.md doc-sync rule).

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
| `pigeon-crypto` | The client-side MLS engine: `Device`, group create/join, `add_member`→`AddOutcome{welcome, commit}`, `process_commit`, `encrypt`/`decrypt`, export/restore storage, encrypted backup (recovery key). The core does **not** reimplement any of this. |
| `pigeon-core` | Event types, content structs, canonical JSON (for anything signed), event-id hashing — to construct/interpret events identically to the server. |

Depend on these by path (monorepo-adjacent) or pinned git rev — decided and documented in M0.1. The mobile client must track a **server protocol version** and update deliberately on a `v1→v2` bump.

### 4.2 Module responsibilities

- **`api.rs`** — the only place that makes HTTP calls. Owns the homeserver base URL, token injection, ret/timeout policy, and maps `P_*` error codes → typed core errors. Models its calls on the reference CLI (`../../pigeon/clients/cli`).
- **`session.rs`** — register/login/logout; produces a session (token, device_id, server). Secrets handed to the platform keystore via a callback; never persisted in clear.
- **`sync.rs`** — the long-poll `/sync` loop. Treats the composite sync token as opaque (Gotcha #5), diffs the response into the store, emits change events to the host observer, and propagates cancellation when the app backgrounds (Gotcha #6). **Built in M2.2:** `PigeonClient.run_sync(observer)` (endless async FFI; host cancels via coroutine), the `SyncObserver` callback (`on_change`/`on_status`), `apply_sync` (folds `rooms.join[*].timeline.events` into the store, one idempotent txn), and reconnect backoff. The store is wired into `PigeonClient` via the startup `set_store_dir` config. **M3.3:** `apply_to_device` folds the `to_device.events` block — joins MLS groups from inbound `p.mls.welcome` events (idempotent on the Welcome's out-of-band `room_id`; best-effort, never fatal to the loop). **Token-ordering (review finding P2):** `apply_sync` deliberately does **not** advance the sync token; `run_sync` calls `persist_sync_token` **last**, after `apply_to_device`. The composite token also acks the to-device position (the server then deletes acked Welcomes), so persisting it before a Welcome is folded into MLS state could skip a Welcome the server had already deleted — leaving that room permanently undecryptable. Every gated step is idempotent, so a redelivery after a pre-ack crash is safe.
- **`store.rs`** — local SQLite (`rusqlite`, bundled). Source of truth for reads (offline-first). Holds rooms, timeline events (incl. **decrypted plaintext cached on first decrypt** — Gotcha #3), membership, the sync token, and the outbound send queue. **Built in M2.1:** append-only `events` log (idempotent on content-hash `event_id`) + `room_state` folded last-writer-wins by DAG `depth` + verbatim `sync_token`; reads for timeline/room-list/membership/current-state; `user_version` migrations. **M2.5:** `events.local` echo flag + `pending_sends` retry queue (schema v2); `queue_send`/`resolve_send`/`fail_send` back local echo with server-ack reconciliation. **M3.5:** the decrypted-plaintext cache (schema v3: `events.decrypted` + `decrypt_state`); `set_decrypted`/`set_decrypt_failed` — decrypt each encrypted event once and cache it (Gotcha #3). **Finding C1:** `pending_mls` yields inbound `p.room.encrypted` (to decrypt) **and** `p.mls.commit` (to apply) in one DAG-ordered stream — both advance the ratchet, so they share one ordering; `set_commit_processed` marks a commit done (via the same `decrypt_state` column), and the timeline read hides `p.mls.commit` (group plumbing, not content). Internal module (no FFI surface of its own); driven by the sync loop from M2.2.
- **`rooms.rs`** — room list, create/join, paginated timeline reads, send (with local echo + a retrying send queue). **Built in M2.3–M2.4:** the `Room` + `TimelineEvent` FFI records; `PigeonClient.list_rooms()`/`timeline(room, limit, before)` (sync store reads), `create_room`/`join_room`/`fetch_messages` (async). `TimelineEvent` pre-renders a message `body` **or** a `system_text` line in the core so native code never parses events (Gotcha #9). **M2.5:** `send_message` + `flush_pending` (offline-first send with local echo, backed by the store's send queue). **M2.6:** `invite(room, user)` (async → `POST /rooms/{id}/invite`); accepting an invite is `join_room` (the server exposes no invite list and no client decline endpoint — documented gaps). Membership changes render as `system_text` lines. **M2 exit gate** proven by the `e2e/` `two_clients_hold_a_plaintext_conversation` lane. **M3.4:** `create_encrypted_room(name, topic)` (`createRoom {encryption:true}` + `create_group`); `invite` is transparent — for a room whose MLS group we host it also runs `claim_all_devices` → `add_member` → `/sendToDevice p.mls.welcome` per device, **and broadcasts the resulting `p.mls.commit` as a room event** so existing members advance to the new epoch (finding C1). The decrypt pass is `process_inbound_mls` (below).
- **`e2ee.rs`** — the bridge to `pigeon-crypto` (the *only* crypto path — adds nothing of its own). Maintains the per-room MLS group (group_id = room_id bytes), encrypts outbound to `p.room.encrypted`, decrypts inbound, processes `p.mls.welcome`. Idempotent on at-least-once to-device delivery (Gotcha #8). **Built in M3.1:** the `E2ee` engine — device identity (`create`/`restore`/`clear`), `signature_public_key_b64`, `key_packages(n)`. `pigeon-crypto` has no pluggable storage backend, so the engine calls `export_storage()` after every state mutation and **persists `{pubkey, blob}` (base64) through the host `KeyStore`** (`pigeon.mls.state.v1`), not the app DB — private key material stays under the platform keystore (Gotcha #1). **M3.3:** group ops `has_group`/`create_group`/`add_member` (→ `AddOutcome{welcome, commit}`)/`join_from_welcome`. **M3.5:** `encrypt`/`decrypt` (base64 ciphertext ↔ plaintext; each persists as the ratchet advances). The client caches decrypted plaintext (`store.decrypted`) rather than re-decrypting — the ratchet is one-way (Gotcha #3). **Commit apply (finding C1):** `process_commit(room, commit_b64)` applies a `p.mls.commit` broadcast by another member's add, advancing our epoch. The sync loop's decrypt pass is `process_inbound_mls` — a single **DAG-ordered** pass over `store.pending_mls` that applies commits (skipping our own and non-applicable ones) and decrypts messages, interleaved so a commit lands before the ciphertext at its new epoch. Adding a third+ member now works; member removal is still out of scope. **Plaintext-downgrade guard (review finding P1):** when no group is held for a room, `flush_pending` consults `store.is_room_encrypted` before the plaintext fallback — a message for an encrypted room whose Welcome hasn't been processed yet is held queued (and goes out encrypted once the group joins), never sent as `p.room.message` (Gotcha #1). **Content-addressed KeyPackage ids (review finding P5):** `upload_keys` derives each `key_id` from a hash of the package, so republishing a pool after an identity change (e.g. `restore_backup` mints a fresh identity) isn't silently dropped by the server's `(user, device, key_id)` dedup. **M4.3:** `create_backup`/`restore_from_backup` (encrypted device-state backup: recovery key + opaque AEAD blob), driven by `PigeonClient.backup()`/`restore_backup()` against the server's `room_keys` slot (`api.put_room_key`/`get_room_key`). **M4.2:** `encrypt_media`/`decrypt_media` wrap `pigeon-crypto`'s new file-AEAD (per-file key, base64 at the boundary) for encrypted images.
- **`media.rs`** — opaque blob media (M4.1): `pigeon://` content-URI parsing + the 50 MiB size guard. Transfer verbs (`upload_media`/`download_media`, raw bodies) live in `api.rs`; `send_image`/`download_image` + `TimelineEvent.image` in `rooms.rs`. The server stores bytes verbatim. **M4.2 (encrypted media):** `send_image` takes raw bytes and, for a room whose MLS group we hold, encrypts under a fresh per-file key (`E2ee.encrypt_media` → `pigeon-crypto::encrypt_media`), uploads the *ciphertext*, and puts the URL + per-file key inside an E2EE'd `p.room.encrypted` `p.image` content (key never uploaded). `download_image` decrypts in-core. The AES-GCM file primitive was **added to `pigeon-crypto`** (no second crypto path).
- **`keys.rs`** — queries/claims peers' keys (`/keys/query`, `/keys/claim`) for group establishment. **M3.1** put the key-directory HTTP verbs (`upload_keys`/`query_keys`/`claim_keys`/`send_to_device`) in `api.rs`, and device-key *publishing* on login in `session.rs` (`publish_device_keys` → `POST /keys/upload`, best-effort like the reference CLI). **M3.2:** `claim_all_devices(api, user_id)` — the query→per-device-claim sequencing (returns `ClaimedKeyPackage`s), reused by the invite-with-Welcome flow (M3.4).
- **`backup.rs`** — wraps `pigeon-crypto`'s recovery-key backup: produce a recovery key + encrypted blob, store via the key-backup endpoints, and restore identity+groups on a fresh device.
- **`media.rs`** — upload (size-cap aware → 413), download by `pigeon://server/id` URI, thumbnails; client-side encryption for encrypted rooms (server stores opaque bytes).
- **`ffi.rs`** — the UniFFI surface: records (DTOs the UI renders), error enums, and **observer/callback interfaces** for the host (sync change stream, logging sink, keystore access).

### 4.3 The FFI surface (illustrative — not final)

Keep it small, coarse, and stable. Sketch:

```
// session  — register/login/restore_session/logout IMPLEMENTED (M1.2/M1.3/M1.5)
async fn register(server, username, password) -> PigeonClient   // token stays in-core
async fn login(server, user, password) -> PigeonClient          // ↑ Gotcha #1
//   PigeonClient.session() -> Session { user_id, device_id, server }   // no token
//   PigeonClient.logout()   // revoke server-side (best-effort) + clear keystore (M1.5)
async fn restore_session() -> PigeonClient?  // reload+validate via keystore on launch (M1.3, offline-first)
fn set_key_store(store: KeyStore)            // install the platform-keystore backend once at startup (M1.3)

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

- **UI:** Jetpack Compose. Screens: homeserver/auth, room list, chat/timeline, room create, invite, media viewer, settings, backup/restore. **Built through M3:** auth (M1.4), room list + create/join (M2.3), chat timeline + pagination + send (M2.4/M2.5), invite dialog (M2.6), and encrypted-room UX (M3.6 — a create-dialog E2EE toggle wiring `create_encrypted_room`, an "Encrypted" badge in the list, a 🔒 in the chat bar, and the "⚠ Unable to decrypt" line from the core). **M4:** media attach/thumbnails/viewer (M4.1) incl. encrypted media (M4.2, transparent); a backup/restore overflow menu (M4.3); and feasible polish — per-message timestamps + image a11y (M4.5). Push (M4.4) is blocked server-side.
- **View-models:** translate UI intent → coarse core calls; collect the core's `SyncObserver` diffs into Compose state (a `Flow`/`StateFlow`). No protocol logic here — only presentation.
- **OS integration:** Android Keystore (implements the core's `KeyStore` callback), FCM (push → wake/sync), photo/file pickers, sharing, foreground/background lifecycle (drives `start_sync`/`stop_sync`), biometric unlock (optional).
- **Bindings:** generated Kotlin + the per-ABI `.so` (`arm64-v8a`, `armeabi-v7a`, `x86_64` for the emulator), produced by the build pipeline (§7).

## 6. The iOS app (`ios/`, Phase M5)

Same core, packaged as an `xcframework` with UniFFI Swift bindings. SwiftUI UI + Apple OS glue: Keychain (the `KeyStore` callback), `os_log` (the `LogSink`), native photo picker, and a background-refresh-aware sync lifecycle. **No protocol or crypto code in Swift** — the M5 build needed none; if any were required, the FFI boundary leaked and the fix belongs in the core (benefiting Android too). **Phase M5 is complete** (built, unit-tested, and run on an iOS simulator on a macOS host — Xcode 26.6, iOS 26.5 runtime). Four scripts drive it: `build-core.sh` (the xcframework), `run-hellocore.sh`, `run-tests.sh`, `run-app.sh` — all in the macOS `ios` CI lane.

- **M5.1 — core packaged for Swift.** The whole FFI surface generates cleanly to Swift (`uniffi-bindgen --language swift` — smoke-checked in the Linux `core` CI lane too). `ios/build-core.sh` cross-compiles the Apple targets (device arm64 + a fat arm64/x86_64 simulator slice), generates the Swift bindings, and assembles `PigeonCoreFFI.xcframework`; `ios/PigeonCore/Package.swift` exposes it as a SwiftPM dependency (`import PigeonCore`). The xcframework assembly + Apple-target compile are **macOS-gated** (need Xcode + the iOS SDK; the Linux dev container can only generate the bindings) — see `ios/README.md`.
- **M5.2 — Hello-core smoke app.** `ios/HelloCore/` renders `coreVersion()` + `selfTestCrypto()` through the bindings and installs an `os_log` `LogSink` — the mirror of Android's M0.4, proving the Rust → xcframework → UniFFI → Swift → SwiftUI pipeline on a simulator.
- **M5.3 — OS integration.** The real `ios/Pigeon/` app lands the Apple-side glue over the shared core, mirroring Android's `PigeonApp`: `KeychainKeyStore` (the `KeyStore` callback backed by the real Keychain), `OsLogSink`, `PhotoPicker`, and a `SyncController` (background-refresh-aware sync lifecycle, cancels the in-flight `/sync` on background — Gotcha #6), installed by `PigeonApp`. Verified by a real-Keychain unit suite on a simulator. **APNs push is BLOCKED** — no server contract (inherits M4.4).
- **M5.4 — feature parity.** SwiftUI screens for the M1–M4 flows (auth/session, room list + create/join incl. E2EE, timeline + send/receive, media, invite, backup/restore), driven entirely by the shared core with zero protocol/crypto in Swift. Built + unit-tested (`run-tests.sh`) and launched on a simulator (`run-app.sh`).

## 7. Build & codegen pipeline

```
   core/ (Rust)
      │  1. cargo build  (host: tests;  cargo-ndk: per-ABI .so;  iOS targets: static libs)
      │  2. uniffi-bindgen  →  Kotlin bindings (Android)  /  Swift bindings + xcframework (iOS)
      ▼
   android/  ← bundles .so + Kotlin bindings;  ./gradlew assembleDebug
   ios/      ← links xcframework + Swift bindings;  xcodebuild
```

- **Dev containers (`docker/Dockerfile` + `docker-compose.yml`):** persistent environments for continuous build/test. The host's `projects/` dir is bind-mounted at `/workspace` so the `../pigeon` path-deps resolve unchanged; cargo registry/target and the Gradle cache live in named volumes for caching across restarts. The image carries the full toolchain: Rust + `rustfmt`/`clippy` + Android targets + `cargo-ndk`, **JDK 25 (Temurin)**, Android SDK (**platform/build-tools 36**) + **NDK r28c**, and **Gradle 9.6.1** — matching the `android/` build (AGP 9.2.1 / Gradle 9.6.1 / Kotlin 2.2.10 / compileSdk 36), so `assembleDebug` builds in the container. A `docker/Dockerfile.sandbox` layer adds Node + Claude Code on top for read-only autonomous runs (`scripts/claude-sandbox.sh`). **Two services** because the NDK ships only x86_64 Linux binaries:
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
- **CI lanes:** **core** (`cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`, plus a Swift-bindings-generation smoke check), **android** (codegen + `assembleDebug` + lint), and a macOS **ios** lane (xcframework build → `run-hellocore.sh`/`run-tests.sh`/`run-app.sh`). Aggressive caching.
- **End-to-end lane (`e2e/`):** a **separate crate** (not part of `core/`) that path-deps on `core` + the server's `tests-integration` harness. It boots the real `pigeon` server in-process over a TCP socket (real Postgres via **testcontainers**) and drives the core's FFI against it — the real-wire-contract proof the mock tests can't give. **Requires Docker**, so it is deliberately excluded from `core/`'s build and the core CI lane (the dev container has no Docker socket). Run on a host/CI with Docker: `cd e2e && cargo test`. First proof: the M1 register/login/restore flows (M1.2/M1.3).

## 8. End-to-end flows (how the pieces cooperate)

**Login (M1).** UI → `login()` → `api.rs` calls `/_pigeon/client/v1/login` → session returned → token+device_id stored in Keystore via the `KeyStore` callback → `start_sync()` begins.

**Receiving a message (M2/M3).** `sync.rs` long-polls `/sync` → for an encrypted room, `e2ee.rs` decrypts the `p.room.encrypted` event via `pigeon-crypto` → **plaintext cached in the store** (Gotcha #3) → `store.rs` diff → `SyncObserver.on_change` → view-model updates Compose state → chat screen re-renders.

**Sending an encrypted message (M3).** UI → `send_message()` → `rooms.rs` sees the room's `p.room.encryption` marker → `e2ee.rs` encrypts via the room's MLS group → `api.rs` sends `p.room.encrypted` → local echo into the store → render. The server stores only ciphertext.

**Inviting to an encrypted room (M3).** `invite()` → `keys.rs` claims the invitee's KeyPackage (`/keys/claim`) → `e2ee.rs` `add_member` → the `Welcome` is shipped over `/sendToDevice` (type `p.mls.welcome`) to the invitee **and the `commit` is broadcast as a `p.mls.commit` room event** (finding C1). The invitee picks the Welcome up from `/sync` `to_device` and joins idempotently; the existing members pick the commit up from their room timeline and apply it in DAG order (`process_inbound_mls` → `process_commit`) to advance to the new epoch.

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
- ~~Local store: `sqlx` vs `rusqlite` (M2)~~ → **`rusqlite` with the `bundled` feature** (M2.1): statically compiles SQLite into the cdylib (no system-libsqlite dep under `cargo-ndk`), lighter than `sqlx`, no build-time DB. Sync API off the FFI's async path; store ops guarded by a `Mutex<Connection>`.
- Server discovery (`.well-known`) in the core: M1 or later.
- Push contract: what the homeserver exposes for push (confirm with the server repo before M4.4).
- Android min SDK / NDK version pin (M0.3).