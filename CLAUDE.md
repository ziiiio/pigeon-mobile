# CLAUDE.md

This file orients Claude (and other AI coding assistants) on the **Pigeon mobile client**. Read it before making non-trivial changes. The companion docs are [`ROADMAP.md`](ROADMAP.md) (the plan) and [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) (the structural map).

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
    store.rs              # local persistence (SQLite via sqlx/rusqlite)
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

**Active phase: Phase M1 (identity: register/login/session).** **M0 is complete** — the Hello-core app was run on an arm64 emulator and renders Rust-computed values (`core_version()` + `self_test_crypto()`) with the M0.7 log sink reaching logcat; the full Rust→cargo-ndk→UniFFI→Kotlin→Compose pipeline round-trips on a real device. **M1 progress:** M1.1–M1.4 have landed.
- **M1.1** (`core/src/api.rs`): reqwest+rustls `Api` with base-URL/token config, `get`/`post`/`put` helpers, `P_`-error → typed `ApiError`/`ErrorCode` (12 codes + `Other`). Server discovery: none exists in Pigeon's client contract (see Protocol note below).
- **M1.2** (`core/src/session.rs`): async FFI `register`/`login` → an opaque `PigeonClient` object holding the token *in-core* (Gotcha #1); the UI gets only the `Session` record (no token). `CoreError` expanded (`Api{code}`/`Network`/`Protocol`) with `From<ApiError>`. First async FFI (UniFFI **tokio** runtime → Kotlin `suspend` fns); the generated bindings compile via `assembleDebug`.
- **M1.3** (`core/src/session.rs`): a host `KeyStore` callback (`put`/`get`/`delete`) installed via `set_key_store`; login/register persist the session blob (identity + token) through it (never the app DB in clear); `restore_session()` reloads + validates via `/account/whoami`, **offline-first** (revoked token → clear+`None`; network error → optimistic restore). `CoreError::Storage` for keystore faults.
- **M1.4** (`android/app/.../auth/`): the Compose auth UI. `PigeonApp` (an `Application`) installs the two host callbacks once at process start — `LogcatSink` (M0.7) and `AndroidKeyStore` (M1.3, backed by `EncryptedSharedPreferences` → Android Keystore, throwing `KeyStoreException.Backend`). `AuthViewModel` is the thin VM: `restore_session()` on launch (offline-first), then `login`/`register`, holding the opaque `PigeonClient` handle (token stays in-core — Gotcha #1). `AuthScreen` is the homeserver/username/password form with loading + per-error states; `AuthError.authErrorMessage` maps each typed `CoreException` (branching on the `P_` `ErrorCode`, never text) to a user message; `HomeScreen` shows the non-secret `Session`. Logout is **M1.5** (not built). New Android deps (flagged): `lifecycle-viewmodel-compose`/`lifecycle-runtime-compose`, `kotlinx-coroutines-android`, `androidx.security:security-crypto` (Keystore wrapper — no new crypto path), and `junit` (test). **Validated** by JVM unit tests for the error mapper (`AuthErrorTest`, exhaustive over variants incl. unknown code) and a green `assembleDebug`.

**Validation:** register/login/restore are covered two ways — (1) fast mock-HTTP tests in `core` (`wiremock`, no Docker: request shapes, bearer injection, `P_`-error → typed code, offline-first restore paths); and (2) a real oneshot-homeserver e2e in the **separate `e2e/` crate** (path-deps on `core` + the server's `tests-integration`), which spins the actual `pigeon` server over TCP with a real Postgres via testcontainers and drives the core's FFI. The e2e needs Docker, so it lives OUT of `core/` — the dev container's `cargo test` and the core CI lane never build it. Run it on a host with Docker: `cd e2e && cargo test`. ✅ passing.

**Protocol note (learned in M1):** Pigeon has **no client-side server discovery**. `.well-known/pigeon/server` is federation-only (`{"m.server": …}`); there's no client `.well-known`/`m.homeserver`. Clients take the homeserver URL directly (as the reference CLI does) — do not wire discovery into the client.

**Next: M1.5** — logout & token invalidation: clear the local session + keystore, and call the server logout if it exists (check the reference CLI). Wire a Sign-out action into `HomeScreen` driving a new core `logout` FFI. That closes M1; then M2 (sync + plaintext rooms) opens.

New deps across M1: `reqwest`, `serde_json`, `tokio`, the `uniffi` `tokio` feature; dev-only `wiremock` + `serial_test` — all in the sanctioned stack.

**M0 recap — built & verified:** the `pigeon-mobile-core` crate (path-deps on `pigeon-core`/`pigeon-crypto`), UniFFI 0.28 in proc-macro mode exposing `core_version`/`self_test_crypto`/`CoreError` + the M0.7 `LogSink`/`set_log_sink`/`emit_test_log` callback (M0.1/M0.2/M0.7); Android NDK cross-compile via cargo-ndk producing the `.so` for `arm64-v8a` + `x86_64` (M0.3); the Hello-core Compose app that drives all of the above through the generated bindings (M0.4); Gradle build glue so `./gradlew assembleDebug` rebuilds core + regenerates bindings + packages the `.so` per ABI in one command (M0.5); and a two-lane GitHub Actions CI (M0.6). The **full pipeline round-trips in the build** — Rust → cargo-ndk `.so` → UniFFI Kotlin → a compiled debug APK — verified in the Docker containers (`cargo test`/`clippy`/`fmt` + `assembleDebug`). **The one open M0 item is a visual confirmation on an emulator** (build-verified, not yet run on-device; the amd64 container can't host an x86_64 emulator). Once that's ticked, M0 closes and **M1 (session/identity)** opens. See `ROADMAP.md`.

**FFI gotcha (learned in M0.4):** never name a `uniffi::Error` variant field `message` — UniFFI maps errors to Kotlin `Throwable` subclasses and `message` collides with `Throwable.message`, producing uncompilable bindings. `CoreError::Crypto` uses `reason`.

**Dev workflow:** work in the Docker container so builds are reproducible and continuous — `docker compose up -d`, then `docker compose exec -w /workspace/pigeon-mobile/core dev cargo test` (and `cargo clippy`/`cargo fmt`). The parent `projects/` dir is mounted at `/workspace`, so the `../pigeon` path-deps resolve as on the host. See README "Building" and ARCHITECTURE §7.

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
- **Local store**: SQLite (via `sqlx` or `rusqlite` — decide in M2, flag it). Secrets/keys go in the platform keystore, *referenced* from the core, not stored in the app DB in clear.
- **Async**: `tokio`. UniFFI async functions are supported; prefer them over callback soup.
- **Serialization**: `serde` / `serde_json`. Canonical JSON for anything signed → `pigeon-core::canonical` (Gotcha #4).
- **Errors**: `thiserror` in the core; surfaced as UniFFI error enums.

**Android (`android/`):**
- **Language**: Kotlin. **UI**: Jetpack Compose. **DI**: Hilt (or manual — decide in M1). **Async**: coroutines/Flow bridging the core's async API.
- **Cross-compilation**: `cargo-ndk` builds the `.so` per ABI (`arm64-v8a`, `armeabi-v7a`, `x86_64` for the emulator).
- **Push**: FCM (the server side of push routing is out of scope for this repo; integrate against whatever the homeserver exposes — confirm before building).
- **Min SDK**: decide in M0 (target a modern floor; document it).

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

## What Claude Should Not Do

- Don't put crypto, key handling, event parsing, or API-request construction in Kotlin/Swift.
- Don't add dependencies (especially crypto, networking, analytics/crash, DB) without flagging them for approval.
- Don't add `unsafe` in the core; don't write new crypto primitives.
- Don't log/transmit plaintext, tokens, or keys.
- Don't add Phase N+1 features while in Phase N. A stub/interface for genuinely *blocked* later-phase work is fine; its implementation is not. (This is not licence to defer work you *could* finish now — see "The Completeness Rule".)
- Don't defer doable work. If a stage already touches it and its dependencies exist, finish it now — don't leave a `TODO`/"later" behind. (The Completeness Rule.)
- Don't diverge from the server's wire contract to "make this screen easier."

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
