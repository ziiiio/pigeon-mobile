# CLAUDE.md

This file orients Claude (and other AI coding assistants) on the **Pigeon mobile client**. Read it before making non-trivial changes. The companion docs are [`ROADMAP.md`](ROADMAP.md) (the plan), [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) (the structural map), and [`docs/CODEBASE_GUIDE.md`](docs/CODEBASE_GUIDE.md) (a narrated, junior-dev walk through the code).

## Project Summary

**Pigeon Mobile** is the official mobile client for [Pigeon](../pigeon), the federated, end-to-end-encrypted instant messaging system. It is a **separate repository** from the Pigeon homeserver — it is a *consumer* of Pigeon's stable wire protocol (`/_pigeon/client/v1/*`) and a *reuser* of the homeserver workspace's client-side crates (`pigeon-crypto`, `pigeon-core`).

**Android ships first; iOS follows.** The defining architectural decision (see ARCHITECTURE.md) is that almost everything that is *not UI* — MLS end-to-end encryption, session/auth, the sync loop, key management, encrypted backup — lives **once**, in a shared **Rust core** (`pigeon-mobile-core`), exposed to Kotlin and Swift via **UniFFI**-generated bindings. The platforms contribute only their UI layer (Jetpack Compose on Android, SwiftUI on iOS later). This mirrors how `matrix-rust-sdk` powers Element's mobile apps, and it is the *only* responsible way to handle the E2EE requirement (see Gotcha #1).

This is a multi-phase build. See `ROADMAP.md` for the full plan. **The current phase governs what code is in scope — do not pull work forward from a later phase unless explicitly asked.**

## The Cardinal Rule: crypto and protocol logic live in Rust, not in Kotlin/Swift

This is the project's reason for existing. Before writing *any* logic on the native side, ask: "does this belong in the shared core instead?" The answer is **yes** for anything touching:

- MLS / `openmls` (group creation, encrypt/decrypt, Welcome processing, device state, backup/restore)
- Private keys or any secret material (Gotcha #1 — they never leave the device, and on-device they live in the Rust core + platform keystore, never in app-level Kotlin/Swift state)
- Canonical JSON, event hashing, signature verification (`pigeon-core`)
- The sync loop, token/session handling, key upload/claim/query, to-device messaging

The native side is allowed to own: **UI, navigation, OS integration** (notifications/FCM, photo picker, sharing, biometric prompts, foreground services), and **a thin view-model layer** that calls the core and renders what it returns. If you find yourself parsing a Pigeon event, doing crypto, or constructing a federation/client API request body in Kotlin or Swift, **stop** — that logic belongs in `pigeon-mobile-core`.

## The Completeness Rule: no deferred work — if it can be done now, do it now

**Do not defer work you could finish now.** If a piece of a feature is implementable with what already exists — the dependency is present, the wire contract is known, the foundation it needs is built — implement it, completely, in the same change. Do not split doable work into a "later" bucket, leave it as a bare `TODO`, or label it "deferred"/"M-later" to shrink a diff. A stage is done when its endpoints, its error paths, and its tests are all in — not when the happy path compiles.

**This does not contradict the phase guard — the two cover different things, and the distinction is *blocked* vs *deferred*:**
- **Blocked** = the work genuinely can't be built yet because its foundation doesn't exist (the sync loop before the store; e2ee before sync; anything needing a not-yet-built module or a later milestone's groundwork). Blocked work *waits* — that's real sequencing, and the phase guard ("don't pull work forward") governs it. A stub/interface for blocked work is fine.
- **Deferred** = postponing work you *could* finish now, for convenience. This is what the rule forbids. If it's buildable today, "we'll wire it up in a later stage" is not an acceptable reason to skip it.

Litmus test: if you're about to write "deferred", "TODO later", "M-later", or "left for a follow-up" for something whose dependencies already exist — stop and build it now. The current phase still scopes *which features* you pursue; within that scope (and for anything a stage already touches), finish it.

**The one legitimate deferral** is an explicit decision that is genuinely the user's to make — a new dependency needing approval, a real design fork, or work that would pull a *later phase's* feature forward. Flag it, name the trade-off, and get a decision. Never silently defer.

## Architecture at a Glance

```
            ┌───────────────────────────────────────────────┐
            │           pigeon-mobile-core (Rust)            │
            │  reuses pigeon-crypto (MLS) + pigeon-core      │
            │  owns: session, sync loop, key mgmt, backup,   │
            │        the Client-Server API client            │
            │  UniFFI-annotated public API + state store     │
            └───────────────────────────────────────────────┘
                 │ UniFFI gen                    │ UniFFI gen
                 ▼ (Kotlin bindings)             ▼ (Swift bindings, later)
       ┌───────────────────────┐       ┌───────────────────────┐
       │  Android app (Kotlin) │       │   iOS app (Swift)      │
       │  Jetpack Compose UI   │       │   SwiftUI (Phase M5)   │
       │  FCM, keystore, etc.  │       │   APNs, Keychain       │
       └───────────────────────┘       └───────────────────────┘
                         │ HTTPS (token auth)
                         ▼
              Pigeon homeserver  /_pigeon/client/v1/*
```

The mobile client speaks **only the Client–Server API** (`/_pigeon/client/v1/*`, token auth over HTTPS). It never speaks federation directly — federation is server-to-server. Treat the homeserver as the client's single point of contact; cross-server delivery, key fetch, and media fetch are the *server's* job (the client just talks to its own homeserver).

## Repository Layout (target)

```
core/                     # pigeon-mobile-core — the shared Rust crate (UniFFI)
  src/
    lib.rs                # UniFFI scaffolding + public API surface
    session.rs            # login/register/token persistence
    sync.rs               # the /sync long-poll loop + state diffing
    rooms.rs              # room list, timeline, send (plain + encrypted)
    e2ee.rs               # wraps pigeon-crypto: groups, encrypt/decrypt, welcomes
    keys.rs               # device keys + KeyPackage upload/claim/query
    backup.rs             # encrypted device-state backup/restore
    media.rs              # upload/download (opaque, client-encrypted)
    store.rs              # local persistence (SQLite via rusqlite, bundled) — built M2.1
    api.rs                # the Client-Server HTTP client (reqwest)
    ffi.rs                # UniFFI type/error/callback definitions
  pigeon_mobile_core.udl  # (or proc-macro) UniFFI interface definition
android/                  # the Android app (Gradle, Kotlin, Jetpack Compose)
  app/
  core-bindings/          # generated Kotlin + the bundled .so per ABI
ios/                      # the iOS app (Phase M5; Swift, SwiftUI, xcframework)
docs/
  ARCHITECTURE.md
```

`core/` is the heart. Keep it free of platform assumptions — no Android/iOS APIs, no `android_logger` in the public path (use a UniFFI callback for logging so the host owns the sink). It must build and unit-test on the host (`cargo test`) with no device.

## Current Phase

Update this section as the project progresses. Claude should refuse to add Phase N+1 features when working in Phase N unless explicitly asked.

**Active phase: Phase M3 (E2EE via `pigeon-crypto`/MLS). M2 is complete (M2.1–M2.6). M1 is complete (M1.1–M1.5).** **M0 is complete** — the Hello-core app was run on an arm64 emulator and renders Rust-computed values (`core_version()` + `self_test_crypto()`) with the M0.7 log sink reaching logcat; the full Rust→cargo-ndk→UniFFI→Kotlin→Compose pipeline round-trips on a real device. **M1 record:** M1.1–M1.5 have landed.
- **M1.1** (`core/src/api.rs`): reqwest+rustls `Api` with base-URL/token config, `get`/`post`/`put` helpers, `P_`-error → typed `ApiError`/`ErrorCode` (12 codes + `Other`). Server discovery: none exists in Pigeon's client contract (see Protocol note below).
- **M1.2** (`core/src/session.rs`): async FFI `register`/`login` → an opaque `PigeonClient` object holding the token *in-core* (Gotcha #1); the UI gets only the `Session` record (no token). `CoreError` expanded (`Api{code}`/`Network`/`Protocol`) with `From<ApiError>`. First async FFI (UniFFI **tokio** runtime → Kotlin `suspend` fns); the generated bindings compile via `assembleDebug`.
- **M1.3** (`core/src/session.rs`): a host `KeyStore` callback (`put`/`get`/`delete`) installed via `set_key_store`; login/register persist the session blob (identity + token) through it (never the app DB in clear); `restore_session()` reloads + validates via `/account/whoami`, **offline-first** (revoked token → clear+`None`; network error → optimistic restore). `CoreError::Storage` for keystore faults.
- **M1.4** (`android/app/.../auth/`): the Compose auth UI. `PigeonApp` (an `Application`) installs the two host callbacks once at process start — `LogcatSink` (M0.7) and `AndroidKeyStore` (M1.3, backed by `EncryptedSharedPreferences` → Android Keystore, throwing `KeyStoreException.Backend`). `AuthViewModel` is the thin VM: `restore_session()` on launch (offline-first), then `login`/`register`, holding the opaque `PigeonClient` handle (token stays in-core — Gotcha #1). `AuthScreen` is the homeserver/username/password form with loading + per-error states; `AuthError.authErrorMessage` maps each typed `CoreException` (branching on the `P_` `ErrorCode`, never text) to a user message; `HomeScreen` shows the non-secret `Session`. New Android deps (flagged): `lifecycle-viewmodel-compose`/`lifecycle-runtime-compose`, `kotlinx-coroutines-android`, `androidx.security:security-crypto` (Keystore wrapper — no new crypto path), and `junit` (test). **Validated** by JVM unit tests for the error mapper (`AuthErrorTest`, exhaustive over variants incl. unknown code) and a green `assembleDebug`.
- **M1.5** (`core/src/session.rs` + `android/app/.../auth/`): logout & token invalidation. An async `PigeonClient.logout()` FFI revokes the token server-side via `POST /logout` (best-effort — like the reference CLI, an unreachable server or already-dead token still clears locally) then wipes the keystore blob; a genuine keystore fault surfaces as `CoreError::Storage`. `HomeScreen` gains a Sign-out action driving `AuthViewModel.logout()`: success drops the `PigeonClient` handle → back to the form; a keystore fault keeps the session live and shows the error to retry (`AuthState.SignedIn` gained `signingOut`/`error`). **Validated** by mock-HTTP tests in `core` (revoke-with-bearer + local clear; clear-even-when-revoke-fails) and the `e2e/` oneshot lane (real server-side revocation proven by re-injecting the pre-logout blob and watching `/account/whoami` reject it). No new deps.

**Validation:** register/login/restore/logout are covered two ways — (1) fast mock-HTTP tests in `core` (`wiremock`, no Docker: request shapes, bearer injection, `P_`-error → typed code, offline-first restore paths); and (2) a real oneshot-homeserver e2e in the **separate `e2e/` crate** (path-deps on `core` + the server's `tests-integration`), which spins the actual `pigeon` server over TCP with a real Postgres via testcontainers and drives the core's FFI. The e2e needs Docker, so it lives OUT of `core/` — the dev container's `cargo test` and the core CI lane never build it. Run it on a host with Docker: `cd e2e && cargo test`. ✅ passing.

**Protocol note (learned in M1):** Pigeon has **no client-side server discovery**. `.well-known/pigeon/server` is federation-only (`{"m.server": …}`); there's no client `.well-known`/`m.homeserver`. Clients take the homeserver URL directly (as the reference CLI does) — do not wire discovery into the client.

**M2 record so far:**
- **M2.1** (`core/src/store.rs`): the offline-first local store. **SQLite via `rusqlite` (`bundled`)** — the flagged dep decision, chosen over `sqlx` (lighter tree, clean `cargo-ndk` cross-compile with SQLite statically linked, no build-time DB). Schema (append-friendly, `user_version` migrations): an insert-only `events` log keyed by the server's content-hash `event_id` (so re-applying a `/sync` batch is idempotent — Gotcha #8); `room_state`, current state folded from state events last-writer-wins by DAG `depth` (rooms have no wire object — name/topic/encryption/membership are all state events we fold); a single-row `sync_token` holding the opaque composite `next_batch` verbatim (Gotcha #5). Reads: `timeline` (depth-ordered `depth ASC, event_id ASC`, back-paginatable via a `before` depth cursor; `origin_server_ts` is display-only), `list_rooms` (folded name/topic/encryption, most-recent-activity first), `membership` (latest `p.room.member` per target), `current_state` (generic accessor). `StoreError` (`Db`/`Malformed`) → `CoreError::Storage`. **Internal Rust module — no FFI surface of its own yet; the sync loop (M2.2) drives it, and the room/timeline FFI records land in M2.3–M2.4.** Validated by 13 host unit tests (idempotent re-sync, LWW fold incl. a late-arriving stale state event, depth ordering + pagination, room-scoping, malformed-event aborts the batch atomically with no partial write, and reopen-persistence). No FFI/binding change, so `assembleDebug` is unaffected.

- **M2.2** (`core/src/sync.rs` + `api.rs`/`session.rs`): the sync loop. `PigeonClient.run_sync(observer)` — an endless async FFI long-polling `GET /_pigeon/client/v1/sync?since&timeout&limit` (`api.sync`, 30s/100), folding each batch into the store (`apply_sync` flattens `rooms.join[*].timeline.events` → one idempotent `store.apply_events` txn), advancing the opaque `next_batch` verbatim (Gotcha #5). Change stream = a `SyncObserver` callback (`on_change` on new events, `on_status(connected)` for offline) — coarse (Gotcha #7). **Cancellation (Gotcha #6):** host runs it in a cancellable coroutine; UniFFI drops the future → in-flight `/sync` cancelled. Offline-first: transport errors back off (1→30s) + retry; only a fatal error (revoked token) returns `Err`. Store wired into `PigeonClient` via a startup **`set_store_dir`** config (in-memory fallback when unset; `api.rs` also gained `create_room`/`join_room`/`invite`/`send_message` for M2.3–M2.6). `tokio` `time` feature added. Validated by unit tests (fold/idempotency/token-advance-on-empty) + mock-HTTP (`/sync` shape; `run_sync` applies→notifies→cancels).

- **M2.3** (`core/src/rooms.rs` + `android/.../rooms/`): room list + create/join. Core: `PigeonClient.list_rooms()` (sync store read → `Room` FFI record), `create_room(name, topic)` / `join_room(id)` (async; effects surface via the sync loop, not the return value). UI: `RoomListScreen`/`RoomsViewModel` — the signed-in landing screen (replaced `HomeScreen`, now removed). `RoomListRoute` runs `run_sync` in a **client-keyed `LaunchedEffect`** so leaving the screen or a new session cancels it (Gotcha #6); the `SyncObserver` re-reads the store on `on_change` and drives an offline indicator. `PigeonApp` now installs `set_store_dir(filesDir)` at startup. Create/join dialogs; the token still never crosses into UI state (the `PigeonClient` handle rides in `AuthState.SignedIn`). Validated: mock-HTTP (create/join request shapes) + host `assembleDebug` (bindings regenerate; Compose compiles) + JVM unit tests.

- **M2.4** (`core/src/rooms.rs`/`api.rs` + `android/.../rooms/Chat*`): timeline + pagination. Core: `TimelineEvent` FFI record — the core pre-renders a message `body` **or** a `system_text` line (membership/name/topic/encryption/create) so native code never parses events (Gotcha #9); `cursor` is the opaque load-older key (DAG depth). `PigeonClient.timeline(room, limit, before)` (paginated local store read) + `fetch_messages(room, limit)` (async backfill → `store.apply_events`, deduped). **Server limit:** `GET /rooms/:id/messages` has no older-than cursor, so `fetch_messages` tops up recent history; deeper scroll-back pages through the store. UI: `ChatScreen`/`ChatViewModel` (bubbles aligned by sender vs `session.user_id`, centered system lines), opened by tapping a room; scroll-to-top loads older; a shared `MutableStateFlow` change-tick (bumped by the room list's `SyncObserver`) refreshes the open timeline. Validated: unit tests (render mapping) + mock-HTTP (`fetch_messages`) + host `assembleDebug`.

- **M2.5** (`core/src/store.rs`/`rooms.rs`/`sync.rs` + `android/.../rooms/Chat*`): plaintext send, offline-first with **local echo**. Store schema **v2** (migration): `events.local` echo flag (0 confirmed / 1 sending / 2 failed) + a `pending_sends` retry queue. `store.queue_send` writes a provisional echo + queue row; `PigeonClient.send_message` queues then flushes; `flush_pending` (re)transmits oldest-first — `resolve_send` **promotes** the echo to the server's real event id (rewriting the column *and* the payload's `event_id` via `json_set`) so the authoritative event from `/sync` dedups (no dup, no flicker), `fail_send` flags a rejected echo and dequeues it. The sync loop calls `flush_pending` each cycle (auto-retry when back online). `TimelineEvent` gained `pending`/`failed`; the composer dims own messages while sending and shows "Sending…"/"Not sent". Validated: store unit tests (queue/resolve/promote/race-dedup/fail) + mock-HTTP (echo→confirm; server-rejection→failed) + host `assembleDebug`.

- **M2.6** (`core/src/rooms.rs` + `android/.../rooms/Chat*`): invites & membership. Core: `PigeonClient.invite(room, user)` (async → `POST /rooms/{id}/invite` with `{user_id}`); **accepting** an invite is `join_room(id)` — the server exposes no invite list in `/sync` (a pending invite is learned out-of-band) and no client leave/decline endpoint yet, so decline is a documented server-side gap, not a stub. Membership changes already render in the timeline as core-pre-rendered system lines (`render_system` — Gotcha #9). UI: an **Invite** action in the chat top bar opens `InviteDialog` → `ChatViewModel.invite`; the invite/join surface in the timeline via the sync loop. Validated: mock-HTTP (`invite` posts `{user_id}` to the invite path with bearer) + the M2 exit-gate e2e + `assembleDebug` + JVM unit tests. No new deps. **This closes M2.**

**M2 exit gate ✅:** the `e2e/` oneshot lane gained `two_clients_hold_a_plaintext_conversation` — Alice registers, creates a plaintext room, invites Bob; Bob joins (membership); both run the real sync loop in the background; Alice's message reaches Bob and Bob's reply reaches Alice (round-trip over `/sync`); Bob's join renders as a system line in Alice's timeline. Drives the same FFI the UI does, against the real `pigeon` server (Docker-gated).

**M3 record so far:**
- **M3.1** (`core/src/e2ee.rs` + `session.rs`/`api.rs`): device identity & key publishing. The `E2ee` engine wraps `pigeon-crypto::Device` (the only crypto path), mirroring the reference CLI's engine but persisting MLS state through the host `KeyStore` (`pigeon.mls.state.v1`), not a file — `pigeon-crypto` has no pluggable storage, so it `export_storage()`s and persists `{pubkey, blob}` (base64) after every mutation (Gotcha #1: private keys under the keystore). `create`/`restore`/`clear`, `signature_public_key_b64`, `key_packages(n)`. register/login mint a fresh identity and best-effort publish it (`publish_device_keys` → `POST /keys/upload`: `device_keys` + 5 base64 KeyPackages); `restore_session` restores the engine (or mints+publishes); `logout` wipes MLS state too. `api.rs` gained `upload_keys`/`query_keys`/`claim_keys`/`send_to_device`; `CoreError: From<CryptoError>`. **Wire facts (verified against server `e2ee_message.rs` + the CLI):** all key material is opaque base64 the server never parses; identity is stamped server-side from the token (absent from bodies); group_id = room_id bytes; Welcomes ride to-device (`p.mls.welcome`), not the room DAG; encrypted messages are `p.room.encrypted` with `{algorithm:"p.mls.1", ciphertext:<b64>}`; `add_member` self-merges and returns only the Welcome (add-mostly groups; member removal out of scope). New dep (flagged): `base64 = "0.22"` — transport encoding only (not crypto), matching the server workspace. Validated: 5 e2ee unit tests + updated M1 session tests; `cargo test`/`clippy`/`fmt` green. Internal engine — no FFI surface change.

**Next: M3.2 (key query/claim)** — then M3.3 to-device + Welcome → M3.4 encrypted room + invite-with-Welcome → M3.5 encrypted send/receive (cache plaintext on first decrypt — Gotcha #3) → M3.6 encrypted-room UX. Do **not** reinvent crypto: wrap `pigeon-crypto` (`openmls`) only. Reference: `../pigeon/clients/cli/src/e2ee.rs` (the canonical engine) + `../pigeon/tests/integration/tests/e2ee_message.rs` (the full wire flow). **Wire contract confirmed** (server + reference CLI, M2.1 research): `/sync` returns only `rooms.join`; state/membership events flow inline in `timeline.events` (no separate `state` block); an invite doesn't surface until you're joined; the messages endpoint (`rooms/:id/messages`) takes only `limit` (no `from`/`dir` cursor yet); send txn ids are **not** honored server-side (dedup locally if needed).

New deps in M2 so far: `rusqlite` (`bundled`) — sanctioned; plus the `tokio` `time` feature (M2.2, no new crate). New deps across M1: `reqwest`, `serde_json`, `tokio`, the `uniffi` `tokio` feature; dev-only `wiremock` + `serial_test` — all in the sanctioned stack.

**M0 recap — built & verified:** the `pigeon-mobile-core` crate (path-deps on `pigeon-core`/`pigeon-crypto`), UniFFI 0.28 in proc-macro mode exposing `core_version`/`self_test_crypto`/`CoreError` + the M0.7 `LogSink`/`set_log_sink`/`emit_test_log` callback (M0.1/M0.2/M0.7); Android NDK cross-compile via cargo-ndk producing the `.so` for `arm64-v8a` + `x86_64` (M0.3); the Hello-core Compose app that drives all of the above through the generated bindings (M0.4); Gradle build glue so `./gradlew assembleDebug` rebuilds core + regenerates bindings + packages the `.so` per ABI in one command (M0.5); and a two-lane GitHub Actions CI (M0.6). The **full pipeline round-trips in the build** — Rust → cargo-ndk `.so` → UniFFI Kotlin → a compiled debug APK — verified in the Docker containers (`cargo test`/`clippy`/`fmt` + `assembleDebug`). **On-emulator run confirmed** (2026-07-04): the current app (the M1 auth UI) builds and runs on an **x86_64 API-36 emulator on a Linux/amd64 host** with the upgraded toolchain (Gradle 9.6.1 / AGP 9.2.1 / Kotlin 2.2.10 / JDK 25, NDK r28c) — the auth screen renders and the M0.7 Rust `LogSink` callback reaches logcat (`pigeon/pigeon_mobile_core: PigeonApp: core callbacks installed`), no crash; the full Rust→cargo-ndk→UniFFI→Kotlin→Compose pipeline round-trips on-device. See `ROADMAP.md`. **✓ Resolved:** the Docker `docker/Dockerfile` (dev + CI + the `claude-sandbox` layer that derives from it) now pins the AGP-9 toolchain — **JDK 25 (Temurin) / SDK 36 (`platforms;android-36` + `build-tools;36.0.0`) / NDK r28c (28.2.13676358) / Gradle 9.6.1** — so `assembleDebug` builds in the container and the android CI lane is unblocked.

**FFI gotcha (learned in M0.4):** never name a `uniffi::Error` variant field `message` — UniFFI maps errors to Kotlin `Throwable` subclasses and `message` collides with `Throwable.message`, producing uncompilable bindings. `CoreError::Crypto` uses `reason`.

**Dev workflow:** work in the Docker container so builds are reproducible and continuous — `docker compose up -d`, then `docker compose exec -w /workspace/pigeon-mobile/core dev cargo test` (and `cargo clippy`/`cargo fmt`). The parent `projects/` dir is mounted at `/workspace`, so the `../pigeon` path-deps resolve as on the host. See README "Building" and ARCHITECTURE §7. The container toolchain now matches the host (JDK 25 / SDK 36 / NDK r28c), so **Android app builds** (`docker compose exec android bash -c 'cd android && ./gradlew assembleDebug'`) run in the container too — no longer host-only. (Rebuild the images after a `docker/Dockerfile` bump: `docker compose build`.)

**Sandboxed autonomous runs:** for an unattended session with `--dangerously-skip-permissions`, `docker/Dockerfile.sandbox` (the dev image + Node + Claude Code) and `scripts/claude-sandbox.sh` run it in a **read-only** container — the only writable surfaces are the bind-mounted repo (edits persist) and tmpfs/volume caches. Build once (`docker compose build dev` → `docker build -f docker/Dockerfile.sandbox -t claude-sandbox .`), then `./scripts/claude-sandbox.sh`. The script mounts the **parent** `projects/` dir (so `../pigeon` resolves — a `$PWD`-only mount fails), relocates `CARGO_HOME`/`GRADLE_USER_HOME`/`$HOME` off the read-only rootfs, and sets `IS_SANDBOX=1` (lets skip-permissions run as root). **Auth reuses your host Claude subscription login** (no API key): `~/.claude` is mounted read-only and seeded into the container's ephemeral `$HOME` at start (`docker/sandbox-entrypoint.sh`), so the session is logged in but never writes to your host credentials. Both `cargo test` and `assembleDebug` work in it (the container toolchain now matches the host).

**Ordering rule:** M0 (toolchain) gates everything. Then M1 (session) → M2 (sync + plaintext rooms) → M3 (E2EE) → M4 (media + polish) → M5 (iOS). Do not start M3 (the crypto integration) until M1–M2 prove the core⇄UI boundary on real flows; do not start M5 (iOS) until the Android app is feature-complete enough to be worth mirroring.

## Protocol Coupling — the server is the source of truth

The homeserver's wire contract is defined in [`../pigeon/CLAUDE.md`](../pigeon/CLAUDE.md), [`../pigeon/docs/ARCHITECTURE.md`](../pigeon/docs/ARCHITECTURE.md), and `../pigeon/clients/cli` (the reference client — **read it before implementing any endpoint**; the CLI already exercises every flow this app needs).

- **Anything under `/_pigeon/` is a stable wire interface.** Pin to a server protocol version. A breaking change is a path version bump (`v1` → `v2`) on the server side; mirror it here deliberately, never silently.
- **Error codes** use the `P_` prefix (`P_FORBIDDEN`, `P_UNKNOWN_TOKEN`, `P_MISSING_TOKEN`, `P_BAD_SIGNATURE`, …). The core maps these to typed errors across the FFI; the UI maps typed errors to messages. Never string-match error text.
- **Sync tokens are opaque and composite** (`"<room_pos>_<to_device_pos>"` today). Treat them as opaque strings — store and return them verbatim; never parse or synthesize one.
- When in doubt about a flow (login, encrypted send, invite-with-Welcome, backup/restore), the canonical reference is `clients/cli` in the server repo. Match its sequence of calls.

## Technology Stack

Stick to these unless you have a specific reason and have discussed it.

**Shared core (`core/`):**
- **Language**: Rust (edition 2021), same toolchain floor as the server.
- **FFI**: **UniFFI** (Mozilla) — generates Kotlin & Swift bindings. No hand-written JNI.
- **HTTP**: `reqwest` with `rustls` (matches the server's client choice).
- **Crypto**: reuse `pigeon-crypto` (MLS via `openmls`, `openmls_rust_crypto` provider). **Do not add a second crypto path.** Do not roll your own crypto. New crypto deps need flagging *and* must already be approved server-side.
- **Local store**: SQLite via **`rusqlite` (`bundled`)** — decided & flagged in M2.1 (over `sqlx`: lighter, statically links SQLite for a clean `cargo-ndk` cross-compile, no build-time DB). Secrets/keys go in the platform keystore, *referenced* from the core, not stored in the app DB in clear.
- **Async**: `tokio`. UniFFI async functions are supported; prefer them over callback soup.
- **Serialization**: `serde` / `serde_json`. Canonical JSON for anything signed → `pigeon-core::canonical` (Gotcha #4).
- **Errors**: `thiserror` in the core; surfaced as UniFFI error enums.

**Android (`android/`):**
- **Language**: Kotlin. **UI**: Jetpack Compose. **DI**: Hilt (or manual — decide in M1). **Async**: coroutines/Flow bridging the core's async API.
- **Build toolchain** (pinned in `android/`): **Gradle 9.6.1** (wrapper), **AGP 9.2.1**, **Kotlin 2.2.10**. AGP 9 provides **built-in Kotlin** — the standalone `org.jetbrains.kotlin.android` plugin is *not* applied; only the Compose compiler plugin is, and its version must match AGP's bundled Kotlin (2.2.10). Generated UniFFI Kotlin is fed to compilation via the **Variant API** (`variant.sources.kotlin`), not the legacy `sourceSets[...].java.srcDir` (which no longer reaches the Kotlin compiler under built-in Kotlin). Building needs a **JDK with a compiler** (a JRE is not enough); verified on JDK 25, which requires Gradle 9 (Gradle 8.x cannot run on JDK 25). `android.sourceset.disallowProvider=false` re-enables the provider-based `.so` `srcDir` wiring.
- **Cross-compilation**: `cargo-ndk` builds the `.so` per ABI (`arm64-v8a`, `armeabi-v7a`, `x86_64` for the emulator). NDK **r28c (28.2.13676358)** on the host.
- **Push**: FCM (the server side of push routing is out of scope for this repo; integrate against whatever the homeserver exposes — confirm before building).
- **SDK levels**: `minSdk 24`, `compileSdk`/`targetSdk 36` (AGP 9 requires a modern SDK). Set in `android/app/build.gradle.kts`.

**iOS (`ios/`, Phase M5):** Swift, SwiftUI, the core packaged as an `xcframework`, Keychain for secrets, APNs for push.

## Code Conventions

- **Layering**: core = logic + protocol + crypto; native = UI + OS. The FFI surface is the contract — keep it small, typed, and stable. Changing the FFI surface ripples into generated bindings on every platform, so treat it like a public API.
- **Errors**: core exposes `thiserror` enums → UniFFI error types. Never `unwrap()` in the core outside tests; `expect()` needs a why. The UI must handle every error variant (a federated, offline-prone client *will* hit them).
- **Async**: I/O is async in the core. Pure logic (event/JSON shaping, state diffing) is sync and unit-tested without a runtime — same discipline as the server's `core`/`state-res`.
- **Secrets**: private keys and tokens live in the Rust core + platform keystore. Never log them, never put them in a crash report, never cross the FFI boundary as a plain string into long-lived Kotlin/Swift state. (Gotcha #1.)
- **Logging**: structured, via a UniFFI log callback the host installs. Include `room_id`/`event_id`/`server` as fields, never message content. **Message plaintext, user identifiers in clear → not in logs, metrics, or crash reports, ever.** (Gotcha #2.)
- **Tests**: core logic gets Rust unit tests (`cargo test`, host-side, no device). Crypto code requires negative tests (wrong recovery key, tampered ciphertext, replay). Native side gets instrumented/UI tests for the view-model⇄core boundary. An end-to-end test that runs the core against a real oneshot homeserver (reuse the server repo's test harness pattern) is the gold standard for a protocol flow.
- **Offline-first**: the network is unreliable on mobile. Reads come from the local store; the sync loop reconciles. Sends are queued and retried. Don't block the UI on the network.

## Gotchas (Read These)

1. **E2EE keys never leave the device — and barely leave the core.** Private MLS/signing keys live inside `pigeon-crypto` state, persisted via the encrypted-backup mechanism (3F.7e in the server repo) and protected at rest by the platform keystore. They do **not** cross the FFI into app-level Kotlin/Swift. If you're writing native code that holds a private key, stop. This is the server repo's Gotcha #9, pulled onto the device.

2. **No plaintext in logs/metrics/crash reporters.** Mobile crash/analytics SDKs love to vacuum up state. Decrypted message bodies, plaintext user handles, tokens, and keys must never reach them. Audit any analytics/crash dependency before adding it (and flag the dependency).

3. **Decryption advances the MLS ratchet — and it's persisted.** Re-reading an already-decrypted message after the cursor passes it can fail (documented limitation in the server's 3F.7d). Design the timeline cache so decrypted plaintext is *stored locally* after first decrypt; do not assume you can re-decrypt the same ciphertext on demand.

4. **Canonical JSON for anything signed.** If the client ever signs or verifies (event hashing, key uploads that the server checks), it must use `pigeon-core::canonical` — `serde_json` is not canonical. Verifying against non-canonical bytes silently fails. (Server Gotcha #1/#2.)

5. **Sync tokens are opaque and composite.** Store/return them verbatim. Never parse `<room_pos>_<to_device_pos>` apart or fabricate one — the format is the server's and may change.

6. **Sync long-poll cancellation.** When the app backgrounds or a screen closes, cancel the in-flight `/sync` and propagate cancellation into the core's async task. Leaking sync tasks per screen will drain battery and sockets. (Server Gotcha #7, device edition.)

7. **The FFI boundary is a real cost — design it coarse.** Don't make per-message chatty FFI calls in a tight loop. Batch: "give me the timeline page," "decrypt these N events," "run one sync and return the diff." Chatty fine-grained FFI is a performance and complexity trap.

8. **To-device delivery is at-least-once.** Welcomes and key material can arrive more than once; the core must be idempotent on them. Don't assume exactly-once. (Mirror of the server's to-device semantics.)

9. **Don't reimplement protocol logic natively "just for this one screen."** Every native shortcut around the core is a future federation-compatibility bug and a second place crypto can go wrong. The whole architecture fails the moment Kotlin/Swift starts parsing Pigeon events.

10. **Binary size & ABI matrix.** Each Android ABI bundles a full `.so`. Use `cargo-ndk`, strip release builds, and consider per-ABI app bundles. Don't ship debug `.so`s. Confirm the emulator ABI (`x86_64`) is built in dev configs or local testing breaks.

## Decisions Already Made

- **Shared Rust core via UniFFI**, thin native UI. Not native reimplementation (would duplicate MLS — unacceptable). Not a cross-platform UI framework (keeps the stack to Rust + Kotlin + Swift). If a change would reverse this, flag it loudly.
- **Android first, iOS later.** iOS reuses the *same* core; the only iOS-new work is SwiftUI + Apple OS integration.
- **Crypto is `pigeon-crypto`, reused, not reinvented.** MLS via `openmls`. Same as the server repo's Decision on E2EE.
- **The app speaks only the Client–Server API.** No direct federation. The homeserver is the single point of contact.
- **Separate repository** from the homeserver; depends on the server's client-side crates (`pigeon-crypto`, `pigeon-core`) and pins a server protocol version.

## What Claude Should Do

- Before writing code, decide: **core (Rust) or native (UI)?** Default to core for anything logic/crypto/protocol. If it crosses the FFI boundary, design the boundary type first.
- Read the server's reference CLI (`../pigeon/clients/cli`) for any protocol flow before implementing it — it already does the dance correctly.
- When touching crypto or signatures, write a negative test in the same change.
- For new core API surface, keep the FFI type small and stable; regenerate and smoke-test bindings on Android before considering it done.
- Keep the network off the UI thread and out of the way of reads — offline-first.
- Finish what's doable now — complete a stage's endpoints, error paths, and tests in the same change; don't defer buildable work (see "The Completeness Rule").
- Keep the docs in sync **in the same commit as the code** (see below).
- **Commit after every completed step** — one green stage, one commit (see "The Commit Rule").

## What Claude Should Not Do

- Don't put crypto, key handling, event parsing, or API-request construction in Kotlin/Swift.
- Don't add dependencies (especially crypto, networking, analytics/crash, DB) without flagging them for approval.
- Don't add `unsafe` in the core; don't write new crypto primitives.
- Don't log/transmit plaintext, tokens, or keys.
- Don't add Phase N+1 features while in Phase N. A stub/interface for genuinely *blocked* later-phase work is fine; its implementation is not. (This is not licence to defer work you *could* finish now — see "The Completeness Rule".)
- Don't defer doable work. If a stage already touches it and its dependencies exist, finish it now — don't leave a `TODO`/"later" behind. (The Completeness Rule.)
- Don't diverge from the server's wire contract to "make this screen easier."

## The Commit Rule

**Commit after each completed step — don't batch.** A "step" is a ROADMAP stage or substage (e.g. `M2.1`, `M2.2`), or a self-contained unit of work the user asked for. When one is done, commit it before starting the next. Do **not** let several stages pile up in the working tree as one mega-diff.

- **One commit per step, and it is complete:** the code, its tests, and the doc updates the change requires (the Documentation Sync Rule) land **together** in that single commit. A commit that leaves a doc contradicting the code, or tests unwritten, is not a finished step.
- **Green before commit.** Only commit a step once its gate passes — `cargo test` + `cargo clippy -D warnings` + `cargo fmt --check` for core changes, plus `assembleDebug` when the FFI surface or Android side changed. Never commit a red build.
- **Message convention:** match the existing history — a `M<phase>.<step>:` (or `build(...)`/`docs:`/`dev:`) prefix and a one-line summary of what the step delivered, mirroring commits like `M1.5: logout & token invalidation — closes M1`. End the message with the `Co-Authored-By` trailer the harness specifies.
- **Commit, not push.** This rule authorizes committing after each step without re-asking. Pushing still waits for an explicit request. This repo's history is linear on `main` (each M-step committed directly, then pushed on request) — follow that convention; don't spin up a branch per step.
- Unrelated pre-existing cruft in the tree (e.g. stray build artifacts) is **not** part of a step's commit — stage only the files the step actually changed.

## Documentation Sync Rule

This project keeps **three** docs, each with one job. A change that lands code a doc now contradicts is incomplete — update the relevant doc(s) **in the same commit**, keep each fact in one canonical place, and cross-reference rather than copy.

- [`README.md`](README.md) *(add when the app is runnable)* — the front door: what it is, what works today, how to build & run, supported platforms.
- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) — the structural map: the core's FFI surface, modules, the native layer's shape, the build/codegen pipeline, end-to-end flows. Update when a module, FFI type, store, screen-to-core flow, or build step is added/removed/renamed.
- [`ROADMAP.md`](ROADMAP.md) — the plan: phases, stage breakdowns, success criteria. Update when scope, ordering, or a stage's status changes.
- `CLAUDE.md` (this file) — the rulebook: conventions, gotchas, decisions, the current-phase guard. Update when a convention/gotcha/decision changes or the active phase moves.

## References

- [`../pigeon`](../pigeon) — the homeserver. Its `CLAUDE.md`, `docs/ARCHITECTURE.md`, and `docs/CODEBASE_GUIDE.md` define the protocol and the crates this client reuses. **The authoritative protocol source.**
- `../pigeon/clients/cli` — the reference client. Read it before implementing any flow.
- **UniFFI** — https://mozilla.github.io/uniffi-rs/ — the Rust↔Kotlin/Swift binding generator.
- **matrix-rust-sdk** — the prior art for "thin native apps over a shared Rust messaging core." Pigeon is Matrix-inspired; this is the closest reference architecture.
- **RFC 9420 (MLS)** — the E2EE protocol, used via `openmls` in `pigeon-crypto`.
- **cargo-ndk** — Android cross-compilation for the Rust core.
