# Pigeon Mobile — A Guided Tour of the Codebase

A junior developer's onramp to the **Pigeon mobile client**. It assumes you can read
Rust and Kotlin but have *not* seen this project. Read it top to bottom once; after that
it's a reference.

Three companion docs, each with one job:

- **[`CLAUDE.md`](../CLAUDE.md)** — the rulebook (conventions, gotchas, decisions, the
  current-phase guard). If this guide and CLAUDE.md ever disagree, CLAUDE.md wins.
- **[`ARCHITECTURE.md`](ARCHITECTURE.md)** — the structural map (the FFI surface, modules,
  the build pipeline).
- **[`ROADMAP.md`](../ROADMAP.md)** — the plan (phases and their exit criteria).

This guide is the *narrated walk through the code*. It points at real files and line
numbers — follow along with them open.

## Table of contents

- [Part 0 — The 60-second mental model](#part-0--the-60-second-mental-model)
- [Part 1 — Get it running](#part-1--get-it-running)
- [Part 2 — The shape of the repo](#part-2--the-shape-of-the-repo)
- [Part 3 — The core, module by module](#part-3--the-core-module-by-module)
- [Part 4 — The FFI boundary (how Rust reaches Kotlin)](#part-4--the-ffi-boundary-how-rust-reaches-kotlin)
- [Part 5 — The Android layer](#part-5--the-android-layer)
- [Part 6 — The build pipeline](#part-6--the-build-pipeline)
- [Part 7 — Flows, end to end](#part-7--flows-end-to-end)
- [Part 8 — Testing (three layers)](#part-8--testing-three-layers)
- [Part 9 — The gotchas, in context](#part-9--the-gotchas-in-context)
- [Part 10 — Where to make changes + exercises](#part-10--where-to-make-changes--exercises)
- [Appendix A — Where the project is (phase status)](#appendix-a--where-the-project-is-phase-status)
- [Appendix B — Glossary](#appendix-b--glossary)

---

## Part 0 — The 60-second mental model

Pigeon is a federated, end-to-end-encrypted chat system. **This repo is only the mobile
client** — it is *not* the server. The server (the "homeserver") lives in a sibling repo,
[`../pigeon`](../../pigeon), and is the authoritative source of the wire protocol.

Four ideas unlock everything else:

1. **Almost nothing that matters is written in Kotlin.** MLS encryption, session/auth,
   the sync loop, key management, the HTTP client — all of it lives *once*, in a shared
   **Rust core** (`core/`, the crate `pigeon-mobile-core`). Android and (later) iOS
   contribute only their **UI + OS integration**. This mirrors how `matrix-rust-sdk`
   powers Element's mobile apps, and it's the only sane way to avoid writing MLS crypto
   twice. This is **the Cardinal Rule** (CLAUDE.md): *if you're about to parse a Pigeon
   event, do crypto, or build an API request body in Kotlin — stop, it belongs in Rust.*

2. **The core talks to Kotlin through UniFFI.** You annotate Rust types/functions with
   `#[uniffi::export]`, and a code generator produces Kotlin bindings (and later Swift).
   No hand-written JNI. The **FFI surface is a contract** — small, typed, and stable.

3. **The client speaks only the Client–Server API.** Every call goes to *your own
   homeserver* over HTTPS with a bearer token, under the path prefix
   `/_pigeon/client/v1/*`. The client never speaks federation (server-to-server) — that's
   the homeserver's job. Treat the homeserver as the client's single point of contact.

4. **Secrets barely leave the core.** The access token (and later the MLS private keys)
   live *inside* the Rust core and the platform keystore. They do **not** cross the FFI
   into Kotlin. The UI gets a non-secret identity and an opaque handle, never the token.
   This is Gotcha #1, and it shapes the whole session design.

The data flow, top to bottom:

```
Jetpack Compose UI  (Kotlin)        ← renders state, no logic
    │  calls suspend fns
AuthViewModel       (Kotlin)        ← thin: calls the core, holds a handle
    │  UniFFI-generated bindings
pigeon-mobile-core  (Rust)          ← session, HTTP client, (later) sync + crypto
    │  HTTPS + bearer token
Pigeon homeserver   /_pigeon/client/v1/*
```

---

## Part 1 — Get it running

Before reading code, build it and watch tests pass. Concrete feedback beats theory.

**Prerequisite:** the homeserver repo must be checked out as a *sibling directory*
(`../pigeon`) — the core depends on its `pigeon-core` and `pigeon-crypto` crates *by
path* (see `core/Cargo.toml`).

### The core (no device, no Docker — start here)

```bash
cd core
cargo test          # builds the core + runs unit tests and mock-HTTP tests
cargo clippy --all-targets -- -D warnings   # CI gates on this
cargo fmt --check
```

This is where you'll spend most of your time. The core builds and tests entirely on your
host machine — no emulator, no server.

### The Android app (needs the Android toolchain)

You need: a **full JDK with a compiler** (a JRE is not enough — verified on JDK 25), the
Android SDK with `platforms;android-36`, an NDK (r28c used here), `cargo-ndk`, and the
Rust Android targets. Point Gradle at the SDK with `android/local.properties`
(`sdk.dir=…`) and export `ANDROID_NDK_HOME` so `cargo-ndk` finds the NDK.

```bash
cd android
./gradlew assembleDebug     # cross-compiles the core, regenerates bindings, builds the APK
```

That single command runs the whole pipeline (Part 6). The APK lands at
`android/app/build/outputs/apk/debug/app-debug.apk`.

### On an emulator

```bash
# create an AVD once (x86_64 image for a Linux/amd64 host with KVM):
avdmanager create avd -n pigeon_x86_64 -k "system-images;android-36;google_apis;x86_64" -d pixel_6
emulator -avd pigeon_x86_64 &
adb install -r android/app/build/outputs/apk/debug/app-debug.apk
adb shell am start -n com.pigeon.mobile/.MainActivity
```

The app opens on the **auth screen** ("Sign in to Pigeon"). The default homeserver URL is
`http://10.0.2.2:8008` — inside the emulator, **`10.0.2.2` is the host machine's
loopback**, so that's how the app reaches a homeserver you're running on your laptop.

### The end-to-end test (needs Docker)

The `e2e/` crate drives the core against a *real* homeserver (spun in-process with a real
Postgres via `testcontainers`). It lives *outside* `core/` precisely because it needs
Docker, so the core's fast `cargo test` never pulls it in.

```bash
cd e2e && cargo test    # requires Docker
```

---

## Part 2 — The shape of the repo

```
core/                       # pigeon-mobile-core — the shared Rust crate (the heart)
  src/
    lib.rs                  # UniFFI scaffolding + top-level FFI surface + logging
    api.rs                  # the Client–Server HTTP client (reqwest/rustls)
    session.rs              # register/login/restore/logout + keystore persistence
    bin/uniffi-bindgen.rs   # the binding generator (pinned to the uniffi dep)
  Cargo.toml                # path-deps on ../../pigeon/crates/{core,crypto}
android/                    # the Android app (Gradle, Kotlin, Jetpack Compose)
  app/src/main/java/com/pigeon/mobile/
    PigeonApp.kt            # Application: installs the core's host callbacks once
    MainActivity.kt         # hosts the Compose auth flow
    LogcatSink.kt           # core LogSink → Android Logcat
    AndroidKeyStore.kt      # core KeyStore → EncryptedSharedPreferences
    auth/
      AuthViewModel.kt      # the thin view-model over the core's session API
      AuthScreen.kt         # the sign-in / register form
      HomeScreen.kt         # the signed-in screen (+ sign out)
      AuthError.kt          # typed CoreException → user-facing message
    app/build.gradle.kts    # the build glue (cargo-ndk + UniFFI codegen)
e2e/                        # the oneshot-homeserver end-to-end test (needs Docker)
docs/                       # ARCHITECTURE.md, this guide
```

`core/` is the heart. Keep it free of platform assumptions — no Android/iOS APIs. It must
build and unit-test on the host with no device.

---

## Part 3 — The core, module by module

Only three source files carry logic today: `lib.rs`, `api.rs`, `session.rs`. Read them in
that order.

### 3.1 `lib.rs` — the FFI entry point, logging, and the M0 smoke tests

The very first line is the load-bearing one:

```rust
uniffi::setup_scaffolding!();   // core/src/lib.rs:12
```

That macro (UniFFI 0.28 in **proc-macro / library mode** — no `.udl` file) wires up all
the `#[uniffi::export]` annotations in the crate. Everything the UI can call is an
annotated item somewhere below it.

**The error type (`CoreError`, `lib.rs:33`)** is the single error surfaced across the FFI.
Its variants map the failure classes the UI must handle: `Api { code, reason }` (a typed
server `P_*` rejection), `Network` (offline/DNS/TLS), `Protocol` (server sent the wrong
shape), `Storage` (keystore fault), `Crypto`. The `From<ApiError>` impl at `lib.rs:55`
translates the HTTP layer's error into it — **preserving the typed code**, never
string-matching.

> **A real FFI gotcha, learned the hard way (comment at `lib.rs:29`):** never name an
> error-variant field `message`. UniFFI maps an error enum to a Kotlin `Throwable`
> subclass, and a field named `message` collides with `Throwable.message` and produces
> *uncompilable* bindings. That's why every variant uses `reason`.

**Logging (`lib.rs:68`+).** The core never assumes a platform logger. Instead the host
*installs a sink*: `LogSink` (`lib.rs:87`) is a `#[uniffi::export(callback_interface)]`
trait — Kotlin implements it, Rust calls it. `set_log_sink` (`lib.rs:95`) stashes it in a
global `RwLock`, and `emit(...)` (`lib.rs:102`) forwards records to it. This is the
**callback pattern**, and you'll see it again for the keystore. Rule: never put message
plaintext, tokens, or user handles in a log record (Gotcha #2).

The remaining exports (`core_version`, `self_test_crypto`, `emit_test_log`) are **M0
toolchain smoke tests** — they exist to prove the Rust→UniFFI→Kotlin pipeline round-trips
and that the reused `pigeon-crypto` MLS engine links and runs (creating a real `Device`
and checking its 32-byte Ed25519 key). They are not "real" API and will fade as real
surface grows.

### 3.2 `api.rs` — the Client–Server HTTP client

A thin `reqwest` (rustls) wrapper around `/_pigeon/client/v1/*`. **It mirrors the
reference CLI's `api.rs` (`../../pigeon/clients/cli/src/api.rs`) — read that file before
adding an endpoint here.** The CLI already does every dance this app needs, correctly.

Three layers, bottom to top:

1. **`ErrorCode` (`api.rs:36`)** — the server's `P_*` codes as a typed enum, exported over
   the FFI (`uniffi::Enum`) so the UI can branch on them. `from_wire`/`as_str` convert to
   and from the wire string. Crucially, there's an `Other { code: String }` catch-all: a
   *newer server* may send a code this build doesn't know, and that must degrade
   gracefully (show *something*) rather than panic. **Never match on the human `error`
   text — only on the code.**

2. **`Api` (`api.rs:138`)** — one HTTP client per session (reqwest pools connections; don't
   build one per call). It owns the base URL and the bearer token. `req()` (`api.rs:186`)
   prefixes the base URL and attaches the token; `send()` (`api.rs:235`) parses JSON on
   2xx and maps the `{ errcode, error }` body to a typed error otherwise. The verb helpers
   `get`/`post`/`put` are the primitives.

   > Note there's a `connect_timeout` but **no global request timeout** (`api.rs:152`) —
   > deliberately, because the `/sync` long-poll (M2) must be allowed to hang for a while.
   > Per-request deadlines are the caller's job.

3. **Named endpoints (`api.rs:198`+)** — `register`, `login`, `logout`, `whoami`: thin
   wrappers over the verb helpers with the exact request shapes the server expects. The
   pure helpers `parse_error` and `parse_auth` are unit-tested without a live server (see
   the `#[cfg(test)]` block at `api.rs:301` — this is the pattern: pure functions, host
   tests).

### 3.3 `session.rs` — register / login / restore / logout, and persistence

This is the app's first real feature and the best example of the architecture in action.

**`Session` (`session.rs:30`)** is the *non-secret* identity — `user_id`, `device_id`,
`server`. It's a `uniffi::Record` (a plain data struct crossing the FFI). Notice what's
**absent**: the access token. That's the whole point.

**`PigeonClient` (`session.rs:45`)** is a `uniffi::Object` — an opaque, reference-counted
(`Arc`) handle the UI holds but cannot see inside. It owns the token-bearing `Api` and the
`Session`. The UI can call `.session()` (get the non-secret identity) and `.logout()`, and
in later phases sync/rooms/e2ee will hang off this same handle. **The token lives in
here and never crosses the FFI** (Gotcha #1).

**The keystore callback (`session.rs:96`+).** Same pattern as the log sink: `KeyStore` is
a host-implemented callback trait (`put`/`get`/`delete`), installed via `set_key_store`.
The core persists the session blob (identity + token, JSON) *through* it — and the host
backs it with the Android Keystore, so secrets never touch the app DB in clear. The
`ks_*` helpers (`session.rs:133`+) deliberately acquire and release the lock *inside* each
call so a lock guard is never held across an `.await` (that would be unsound and could
deadlock).

**The three entry points** are all `#[uniffi::export(async_runtime = "tokio")]`, i.e. they
become Kotlin **`suspend` functions**:

- `register` (`session.rs:224`) and `login` (`session.rs:239`) run the HTTP flow, then
  `finish_login` (`session.rs:198`) *persists the session first* (so a crash right after
  login still leaves a restorable session) and returns the client.
- `restore_session` (`session.rs:257`) is the **offline-first** heart. On launch it reads
  the blob, validates the token against `/account/whoami`, and:
  - token accepted → live session;
  - token *definitively* dead (`UnknownToken`/`MissingToken`) → wipe it, return `None`
    (reads as "logged out");
  - **network error → restore optimistically anyway** (don't sign the user out just
    because the network is down).

  That three-way branch is the offline-first rule made concrete — study it.

- `logout` (`session.rs:74`, a method on `PigeonClient`) revokes the token server-side
  **best-effort** (an unreachable server or already-dead token still clears locally,
  mirroring the reference CLI) then wipes the keystore. A *genuine* keystore fault is
  surfaced as `CoreError::Storage` — because otherwise the blob would silently restore on
  next launch.

---

## Part 4 — The FFI boundary (how Rust reaches Kotlin)

This is the seam the whole project pivots on. Understand the mapping and you can read both
sides.

**How the bindings are made.** `core/src/bin/uniffi-bindgen.rs` is a tiny binary pinned to
the exact `uniffi` version the crate uses. The Gradle build (Part 6) compiles the core to
a `.so`, then runs this bin to generate `uniffi/pigeon_mobile_core/pigeon_mobile_core.kt`.
Kotlin imports it as the package **`uniffi.pigeon_mobile_core`** (see the imports at the
top of every Kotlin file).

**The type mapping you'll actually use:**

| Rust (in the core) | Kotlin (generated) | Notes |
|---|---|---|
| `#[uniffi::export] fn foo_bar()` | `fun fooBar()` | snake_case → camelCase |
| `async fn` (tokio runtime) | `suspend fun` | driven on a Tokio runtime in the core |
| `pub fn` on a `uniffi::Object` | method on an opaque handle | e.g. `PigeonClient.logout()` |
| `uniffi::Record` struct | `data class` | plain data, crosses by value (`Session`) |
| `uniffi::Enum` | `sealed class` / enum | branch on it (`ErrorCode`, `LogLevel`) |
| `uniffi::Error` enum | `Exception` subclass | **`CoreError` → `CoreException`** in Kotlin |
| `#[uniffi::export(callback_interface)] trait` | an interface you implement | Kotlin → Rust (`LogSink`, `KeyStore`) |
| `Result<T, CoreError>` | `T` or throws `CoreException` | errors become thrown exceptions |

Two directions cross the boundary:

- **Kotlin calls Rust:** `coreLogin(...)`, `restoreSession()`, `client.logout()`.
- **Rust calls Kotlin (callbacks):** the core calls your `LogSink.log(...)` and
  `KeyStore.put/get/delete(...)`.

**Design the boundary coarse** (Gotcha #7). Don't make chatty per-item FFI calls in a tight
loop; batch ("give me the timeline page", "run one sync and return the diff"). Crossing the
FFI has real cost.

**Keep the surface small and stable.** Every change here regenerates bindings on every
platform — treat it like a public API.

---

## Part 5 — The Android layer

The native side owns UI, OS integration, and a *thin* view-model. Nothing more. Read these
five files:

**`PigeonApp.kt`** (an `Application`) installs the core's two host callbacks **exactly
once, at process start** — the `LogcatSink` and the `AndroidKeyStore`. Everything
downstream (restore, login, persistence) depends on these being in place, so they belong
here, not in an Activity.

**`LogcatSink.kt`** is the whole of Gotcha #2 in eight lines: it maps a core `LogLevel` to
the matching `Log.e/w/i/d/v` with tag `pigeon/<target>`. (When you ran the app on the
emulator, the line `pigeon/pigeon_mobile_core: PigeonApp: core callbacks installed` in
logcat *is* this class firing — proof the Rust→Kotlin callback round-trips on-device.)

**`AndroidKeyStore.kt`** implements the core's `KeyStore` over
`EncryptedSharedPreferences` (values encrypted at rest with a master key in the Android
Keystore). **This is where the session token lives on device.** The app hands the core
opaque bytes and never inspects them; backend failures become `KeyStoreException.Backend`
(→ `CoreError.Storage`).

**`AuthViewModel.kt`** is the model citizen of "thin view-model". It owns *no* protocol or
crypto logic. It:
- holds the opaque `PigeonClient?` handle (never unwrapping the token);
- exposes a `StateFlow<AuthState>` (a sealed interface: `Restoring` / `SignedOut` /
  `Submitting` / `SignedIn`);
- on init, calls `restoreSession()` (offline-first launch);
- `login`/`register` call the core's suspend fns and stash the returned handle;
- `logout` handles the subtle case where the server revoke fails but the session is still
  live (stay signed in, show a retryable error).

**`AuthError.kt`** is the *only* place error text is decided, and it branches on the
**typed `ErrorCode`**, never on strings (e.g. `UserInUse` → "That username is already
taken."). It's a pure function so it's unit-tested without a device (`AuthErrorTest.kt`).
It handles *every* variant — a federated, offline-prone client will hit them all.

`MainActivity.kt` just renders view-model state and routes between `AuthScreen` and
`HomeScreen`. That's the entire "logic" on the native side.

---

## Part 6 — The build pipeline

`./gradlew assembleDebug` is one command that does four things, wired in
`android/app/build.gradle.kts`:

```
1. cargoNdkBuild          cargo-ndk cross-compiles the core → libpigeon_mobile_core.so
   (Exec task)            for arm64-v8a + x86_64
        │
2. generateUniffiBindings runs the uniffi-bindgen bin against the built .so
   (Exec task)            → uniffi/pigeon_mobile_core/pigeon_mobile_core.kt
        │  (both hooked into preBuild)
3. Kotlin/Java compile    the generated bindings are added to the Kotlin sources via the
                          AGP Variant API (variant.sources.kotlin); the .so via jniLibs
        │
4. package                → app-debug.apk, bundling the .so per ABI
```

**The toolchain (pinned in `android/`):** Gradle **9.6.1** (wrapper), AGP **9.2.1**, Kotlin
**2.2.10**, `compileSdk`/`targetSdk` **36**, `minSdk` **24**, NDK **r28c** on the host.

A few things that trip people up (all consequences of AGP 9's **built-in Kotlin**):

- There is **no** `org.jetbrains.kotlin.android` plugin — AGP 9 provides Kotlin itself.
  Only the Compose compiler plugin is applied, and **its version must match AGP's bundled
  Kotlin (2.2.10)**.
- Generated Kotlin is registered via `variant.sources.kotlin` (the Variant API), *not* the
  legacy `sourceSets[...].java.srcDir` — under built-in Kotlin the latter no longer reaches
  the Kotlin compiler.
- Building needs a **JDK with a compiler** (a JRE has no `javac`). Gradle 9 is required to
  *run* on JDK 25 (Gradle 8.x cannot).

> ⚠ The Docker dev container / CI (`docker/Dockerfile`) still pins the *old* toolchain
> (JDK 17 / SDK 34 / NDK 26) and must be bumped to satisfy AGP 9 before the containerised
> android lane will build again. See ROADMAP "Toolchain upgrade".

---

## Part 7 — Flows, end to end

Trace these with the files open — they're the payoff of Parts 3–5.

### 7.1 Login

```
AuthScreen (tap "Log in")
  → AuthViewModel.login(server, user, pw)      state → Submitting
    → coreLogin(...)  [suspend, crosses FFI]
      → session::login (Rust)
        → Api::new + Api::login → POST /_pigeon/client/v1/login
        → finish_login: ks_put(session blob) via the KeyStore callback → AndroidKeyStore
        → returns Arc<PigeonClient>  (token stays inside)
  ← AuthViewModel holds the handle, calls .session()   state → SignedIn(session)
  → MainActivity renders HomeScreen
```

A server rejection (e.g. wrong password → `P_FORBIDDEN`) throws `CoreException.Api` across
the FFI; the VM catches it, `AuthError` maps the typed code to "Incorrect username or
password.", state → `SignedOut(error)`.

### 7.2 Restore on launch (offline-first)

```
PigeonApp.onCreate installs callbacks
MainActivity → AuthViewModel.init → coreRestoreSession()   state → Restoring
  → session::restore_session (Rust)
    → ks_get(blob)  → none? → None (SignedOut)
    → GET /account/whoami
        200            → SignedIn
        UnknownToken   → wipe blob, None (SignedOut)
        network error  → restore anyway (SignedIn, offline)
```

### 7.3 Logout

```
HomeScreen (tap "Sign out") → AuthViewModel.logout()   SignedIn(signingOut=true)
  → PigeonClient.logout() (Rust)
    → POST /logout  (best-effort — failure is logged, not fatal)
    → ks_delete(blob)   (a real keystore fault → CoreException.Storage)
  ← success → drop handle → SignedOut
  ← keystore fault → stay SignedIn(error) so the user can retry
```

---

## Part 8 — Testing (three layers)

Match the test to what you changed. From fastest/cheapest to most thorough:

1. **Rust unit tests** (`#[cfg(test)]` in `api.rs`, `lib.rs`) — pure functions:
   error-code round-trips, `parse_auth`/`parse_error`, the log-sink callback. No runtime,
   no network. `cd core && cargo test`.

2. **Mock-HTTP tests** (in `core`, using `wiremock` — no Docker) — assert request shapes,
   bearer injection, `P_*`-error → typed-code mapping, and the offline-first restore
   branches, all against a canned HTTP server. This is how you test `session.rs`/`api.rs`
   behaviour without a real homeserver.

3. **The oneshot end-to-end test** (`e2e/`, needs Docker) — drives the core's *actual FFI
   functions* against a *real* `pigeon` server (in-process, real Postgres via
   `testcontainers`). It proves the client half agrees with the real wire contract, not a
   mock. This is the **gold standard for a protocol flow**, and it lives outside `core/` so
   the fast lane never needs Docker. `cd e2e && cargo test`.

4. **JVM unit tests** (`android/.../auth/AuthErrorTest.kt`) — the error mapper is pure, so
   it's exhaustively tested on the JVM (including an unknown code) with no device.

**Crypto rule (for M3+):** any crypto code requires *negative* tests in the same change —
wrong recovery key, tampered ciphertext, replay.

---

## Part 9 — The gotchas, in context

These are the CLAUDE.md gotchas, grounded in the code you just read. Re-read that list;
here's where each one bites:

1. **Secrets barely leave the core.** The token lives in `PigeonClient.api` and the
   keystore; the UI gets `Session` (no token) and an opaque handle. Never add an FFI
   method that returns the token as a string.
2. **No plaintext/secrets in logs.** `LogcatSink` forwards whatever the core emits — so the
   core must never `emit` a token, handle, or message body.
3. **Decryption advances the MLS ratchet (M3).** You can't re-decrypt the same ciphertext
   on demand — store decrypted plaintext locally after first decrypt.
4. **Canonical JSON for anything signed.** `serde_json` is *not* canonical; use
   `pigeon-core::canonical` for anything the server verifies. (Not exercised yet, but it
   will be.)
5. **Sync tokens are opaque and composite (M2).** Store and return them verbatim; never
   parse or synthesise one.
6. **Cancel the `/sync` long-poll on background (M2).** Leaking sync tasks per screen
   drains battery and sockets.
7. **The FFI boundary is a real cost — design it coarse.** Batch, don't chatter.
8. **To-device delivery is at-least-once (M3).** Welcomes/key material can arrive twice —
   be idempotent.
9. **Don't reimplement protocol logic natively "just for one screen."** Every native
   shortcut is a future federation/crypto bug.
10. **Binary size & ABI matrix.** Each ABI bundles a full `.so`; confirm the emulator ABI
    (`x86_64`) is built or on-device testing breaks.

And two learned in this repo specifically:

- **Never name a `uniffi::Error` field `message`** (`lib.rs:29`) — it collides with
  `Throwable.message` and breaks the Kotlin bindings.
- **Pigeon has no client-side server discovery.** `.well-known/pigeon/server` is
  *federation-only*. Clients take the homeserver URL directly (as the reference CLI does).

---

## Part 10 — Where to make changes + exercises

### Where things go

- **A new protocol call / endpoint** → `core/src/api.rs` (a named endpoint), driven from a
  feature module. *First* read the same flow in `../pigeon/clients/cli`.
- **New session/auth behaviour** → `core/src/session.rs`.
- **A new feature's logic (sync, rooms, e2ee)** → a *new* core module (`sync.rs`,
  `rooms.rs`, `store.rs`, `e2ee.rs` — per ARCHITECTURE), exposed through a small FFI type.
- **Anything the UI shows** → the Android layer (a Compose screen + a thin view-model
  method). If a view-model method starts parsing events or building request bodies, you're
  in the wrong layer.
- **A new error the UI must handle** → add a `CoreError` variant (field named `reason`!),
  map it in `From`, and add a branch in `AuthError.kt`.

### Decision checklist before writing code

1. **Core (Rust) or native (UI)?** Default to core for anything logic/crypto/protocol.
2. If it crosses the FFI, **design the boundary type first** (small, typed, stable).
3. Read the reference CLI for the flow before implementing it.
4. Finish the stage: endpoints **and** error paths **and** tests, in the same change (the
   Completeness Rule). Don't leave a buildable piece as a `TODO`.
5. Keep the docs in sync in the same commit (the Documentation Sync Rule).

### Exercises to build intuition

1. **Trace a wrong password.** Start at `AuthScreen`, follow the call into `session::login`
   → `Api::login` → `send` → `parse_error`, and back out to the message the user sees. Name
   every type it crosses.
2. **Add a `CoreError` variant** end-to-end: add it in `lib.rs`, map it in `From<ApiError>`,
   add a Kotlin branch in `AuthError.kt`, and a test in `AuthErrorTest.kt`. Watch the
   binding regenerate.
3. **Find where the token is.** Grep for `access_token`. Convince yourself it never appears
   in a `uniffi::Record`, a log line, or a Kotlin type. That's Gotcha #1 holding.
4. **Read the offline-first branch** in `restore_session` and explain *why* a network error
   restores the session but an `UnknownToken` clears it.

---

## Appendix A — Where the project is (phase status)

The current phase is authoritative in **CLAUDE.md** ("Current Phase") and **ROADMAP.md** —
check there, not here, before assuming what's in scope.

- **M0 — toolchain & skeleton: complete.** The Rust→cargo-ndk→UniFFI→Kotlin→APK pipeline
  round-trips; the app runs on an emulator; CI covers the core lane.
- **M1 — session/identity: complete (M1.1–M1.5).** The HTTP client (`api.rs`), async
  register/login/restore returning an in-core `PigeonClient` (`session.rs`), keystore
  persistence + offline-first restore, the Compose auth UI, and logout — all validated by
  mock-HTTP tests and the `e2e/` oneshot lane.
- **M2 — sync + plaintext rooms: active, not started.** The `/sync` long-poll loop + state
  diffing (`sync.rs`), the local store (`store.rs` — SQLite, decide `sqlx` vs `rusqlite`),
  a room list + timeline + plaintext send (`rooms.rs`), and the Compose UI over them. **Do
  not pull M3 (e2ee) work forward.**
- **M3 e2ee → M4 media/push → M5 iOS → M6 hardening:** later; see ROADMAP.

**Phase discipline (CLAUDE.md):** don't add a later phase's features while in the current
one. A *stub* for genuinely blocked later-phase work is fine; its *implementation* is not.
But don't confuse "blocked" with "deferred" — if a piece of the current stage is buildable
now, finish it now.

---

## Appendix B — Glossary

- **Homeserver** — the Pigeon server that hosts your account. The client's single point of
  contact; lives in `../pigeon`.
- **Client–Server API** — the `/_pigeon/client/v1/*` HTTP surface the app speaks (bearer
  token). The *only* surface the client uses.
- **Federation** — server-to-server traffic. The client never speaks it.
- **UniFFI** — Mozilla's tool that generates Kotlin/Swift bindings from annotated Rust. The
  Rust↔native seam.
- **The core** — `pigeon-mobile-core` (`core/`), the shared Rust crate holding all
  non-UI logic.
- **`PigeonClient`** — the opaque, in-core handle representing a logged-in session; owns the
  token, hands the UI only `Session`.
- **`Session`** — the non-secret identity (`user_id`, `device_id`, `server`) — safe in UI
  state.
- **Host callback** — a trait the native side implements and the core calls (`LogSink`,
  `KeyStore`). Installed once at startup.
- **`P_*` error codes** — the server's typed error set (`P_FORBIDDEN`, `P_USER_IN_USE`, …).
  Branch on the code, never the text.
- **MLS** — Messaging Layer Security (RFC 9420), the E2EE protocol, used via `openmls`
  inside `pigeon-crypto`. Arrives in M3.
- **`cargo-ndk`** — cross-compiles the Rust core to a per-ABI Android `.so`.
- **`10.0.2.2`** — from inside the Android emulator, the host machine's loopback (how the
  app reaches a homeserver running on your laptop).
```
