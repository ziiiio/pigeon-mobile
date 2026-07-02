# ROADMAP — Pigeon Mobile

The development plan for the Pigeon mobile client. **Android ships first; iOS follows** by reusing the same shared Rust core. Read [`CLAUDE.md`](CLAUDE.md) for the rules and [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for the structure.

## Guiding principles

1. **Logic in Rust, UI in native.** The shared `pigeon-mobile-core` crate owns crypto, protocol, session, and sync. Each phase grows the core first, then the UI that drives it. (CLAUDE.md "Cardinal Rule".)
2. **Small, independently-tested, independently-committed slices** — the same discipline the homeserver was built with. A stage isn't done until its tests are green and its docs are in sync.
3. **The server's wire contract is fixed; we consume it.** No protocol invention. The reference CLI (`../pigeon/clients/cli`) is the canonical example for every flow.
4. **Offline-first from the start.** Reads from local store; the network reconciles in the background. The mobile network is hostile (flaky, slow, backgrounded).
5. **Don't pull work forward.** Phase M(N) refuses Phase M(N+1) features unless explicitly asked.

## Phase overview

| Phase | Theme | Gate |
|-------|-------|------|
| **M0** | Toolchain & skeleton | Rust→UniFFI→Kotlin round-trips on an emulator; CI green |
| **M1** | Identity: register/login/session | Real account login on a live homeserver |
| **M2** | Plaintext messaging: rooms, sync, timeline, send | Two devices chat in an unencrypted room |
| **M3** | E2EE (the headline) | Two devices exchange real MLS-encrypted messages |
| **M4** | Media, backup/restore, polish, push | Encrypted media + device recovery + notifications |
| **M5** | iOS | The iOS app reaches Android feature parity over the same core |
| **M6** | Hardening & release | Store-ready: perf, security pass, accessibility, CI/CD |

**Ordering rule:** M0 gates everything (no features until the toolchain round-trips). M1→M2 prove the core⇄UI boundary on real flows before M3 layers crypto on top. M5 (iOS) starts only once Android is feature-complete enough to mirror. M6 is continuous but formalized at the end.

---

## Phase M0 — Toolchain & skeleton

The whole project's risk is concentrated here: proving the Rust-core-via-UniFFI pipeline works before any feature depends on it. Don't skip ahead.

- [x] **M0.1 — `pigeon-mobile-core` crate.** ✅ `core/` is a `lib`+`cdylib`+`staticlib` crate depending on `pigeon-core` and `pigeon-crypto` by **path** (`../../pigeon/crates/*`). Builds and `cargo test`s in the dev container; a `self_test_crypto` test creates a real `pigeon-crypto` `Device` and asserts its 32-byte Ed25519 key, proving the whole `openmls` chain links and runs inside the core.
- [x] **M0.2 — UniFFI scaffolding.** ✅ UniFFI 0.28 in **proc-macro/library mode** (`uniffi::setup_scaffolding!()`, no `.udl`). Exposes `core_version()` + `self_test_crypto()` and a `CoreError` enum. Kotlin bindings generate from the built cdylib via the crate's own `uniffi-bindgen` bin (`coreVersion()` / `selfTestCrypto()` confirmed in the output).
- [x] **M0.3 — Android cross-compile.** ✅ `cargo-ndk` builds the `.so` for `arm64-v8a` + `x86_64` (verified: genuine aarch64 / x86-64 ELF). Toolchain pinned in `docker/Dockerfile`: NDK `26.3.11579264`, Rust targets `aarch64/x86_64/armv7-linux-android*`, JDK 17, Android platform/build-tools 34. **Host-arch note:** the NDK ships only x86_64 Linux toolchain binaries, so on Apple Silicon the Android lane runs in an **amd64 (Rosetta-emulated) container** (`android` service in compose); core dev stays on the native-arch `dev` service. min SDK 24 / target+compile SDK 34 (in `android/app/build.gradle.kts`).
- [x] **M0.4 — Hello-core Android app.** ✅ A bare Jetpack Compose app (`android/app`, `MainActivity.kt`) installs the M0.7 log sink, then calls `coreVersion()` + `selfTestCrypto()` through the generated UniFFI bindings and renders the results. `assembleDebug` produces a debug APK bundling both ABIs' `.so`. **Visual gate passed:** installed on an `arm64-v8a` API-36 emulator (host, Apple Silicon), the app renders `pigeon-mobile-core 0.1.0` (`core_version()`) and `pigeon-crypto Ed25519 key: 32 bytes` (`self_test_crypto()`) — values computed in Rust — and logcat shows the M0.7 log sink (`pigeon/pigeon_mobile_core: Hello-core: log sink installed`). No crash. The full Rust→cargo-ndk→UniFFI→Kotlin→Compose pipeline round-trips on a real device.
- [x] **M0.5 — Build glue.** ✅ `android/app/build.gradle.kts` wires two `Exec` tasks — `cargoNdkBuild` (cargo-ndk `.so` for `arm64-v8a` + `x86_64`) → `generateUniffiBindings` (UniFFI Kotlin codegen from the built cdylib) — into `preBuild`, so `./gradlew assembleDebug` rebuilds core, regenerates bindings, and packages the `.so` per ABI in one command.
- [x] **M0.6 — CI.** ✅ `.github/workflows/ci.yml`: a **core** lane (`cargo fmt --check`, `clippy -D warnings`, `test`) and an **android** lane (NDK + cargo-ndk + `./gradlew assembleDebug`), both checking out `pigeon` as a sibling for the path-deps. *(Placeholder `your-org/pigeon` repo ref to set before first run.)*
- [x] **M0.7 — FFI logging callback.** ✅ A host-installed `LogSink` callback interface + `set_log_sink`/`emit_test_log` in the core; the Hello-core app implements it as a Logcat forwarder. The core never assumes a platform logger.

**Exit criteria:** an emulator app displays a value computed in Rust; CI builds both lanes; the build is one command. No protocol code yet. **✅ MET:** the app displays Rust-computed values on an arm64 emulator (M0.4 above), `assembleDebug` is one command (M0.5), and CI covers both lanes (M0.6). **M0 is closed; M1 is active.**

---

## Phase M1 — Identity (register / login / session)

First real protocol surface. Keeps crypto out of scope — just HTTP + token lifecycle.

- [x] **M1.1 — HTTP client in the core.** ✅ `api.rs` (reqwest + rustls, versions/features mirroring the server): an `Api` client with homeserver base-URL config, bearer-token injection (`set_token`), and generic `get`/`post`/`put` verb helpers. `P_`-error bodies map to a typed `ApiError`/`ErrorCode` (all 12 documented codes + `Other` for forward-compat on a server version bump); matches the stable `errcode`, never the human text. Host-side unit tests cover the error mapping + URL normalization (no network). **Server discovery — settled, not deferred:** Pigeon has **no client-side discovery** in its wire contract. `.well-known/pigeon/server` is *federation* host discovery (`{"m.server": host}`, server-to-server, port 8448) — off-limits to a client that speaks only the Client–Server API. There is no `.well-known/pigeon/client`/`m.homeserver` equivalent; the reference CLI takes the homeserver URL directly. So the caller passes a full base URL, matching the CLI exactly — there is nothing correct to build here unless/until the server grows a client-discovery surface. *(Named endpoints + FFI exposure land in M1.2.)*
- [x] **M1.2 — register / login.** ✅ `session.rs`: async FFI `register(server, username, password)` / `login(server, user, password)` calling `/register` + `/login` (password flow) exactly as the reference CLI does, returning an opaque `PigeonClient` object. **The access token stays in the core** (inside the client's `Api`); the UI gets only the non-secret `Session` record (`user_id`, `device_id`, `server`) — Gotcha #1. `ApiError` maps to a typed `CoreError` (`Api{code}`/`Network`/`Protocol`) so the UI branches on the `P_` code. First async FFI surface (UniFFI tokio runtime; generates Kotlin `suspend` fns). **Validated** by mock-HTTP tests (`wiremock`, no Docker): request shapes, bearer injection, `P_`-error → typed code, and the full FFI path (`session::login`/`register` → `Session`/`CoreError`, incl. network-failure typing). **Still open:** the real oneshot-homeserver e2e — it needs Docker (the server harness spins Postgres via testcontainers), which the dev container lacks, so it belongs in a **separate Docker-gated CI lane** (M1 exit gate; placement TBD). *(Token persistence = M1.3; logout FFI = M1.5.)*
- [ ] **M1.3 — session persistence.** Store the token + device_id in the **platform keystore** (referenced from the core), not the app DB in clear. Restore session on launch.
- [ ] **M1.4 — auth UI.** Compose screens: pick homeserver, register, login, logout. Loading/error states for every `P_` error variant.
- [ ] **M1.5 — logout & token invalidation.** Clear local session + keystore; call the server logout if it exists.

**Exit criteria:** install the app, register or log into a real running homeserver, stay logged in across restarts, log out cleanly. End-to-end test against a oneshot homeserver.

---

## Phase M2 — Plaintext messaging (rooms, sync, timeline, send)

The core messaging loop, unencrypted. Proves the sync engine and the offline-first store before crypto complicates it.

- [ ] **M2.1 — local store.** `store.rs` (SQLite): rooms, timeline events, sync token, membership. Decide `sqlx` vs `rusqlite` and flag the dep. Schema is append-friendly.
- [ ] **M2.2 — the sync loop.** `sync.rs`: long-poll `/sync`, persist the opaque composite token verbatim, diff into the store, expose a change stream over the FFI (UniFFI callback/observer or async stream). Honor cancellation (background/screen-close). (CLAUDE.md Gotcha #6.)
- [ ] **M2.3 — room list + create/join.** Core: list rooms from the store; create room; join room. UI: room-list screen with live updates from the sync stream.
- [ ] **M2.4 — timeline + pagination.** Core: paginated timeline reads from the store; backfill older messages via the server's messages endpoint. UI: a chat screen rendering the timeline, scroll-to-load-older.
- [ ] **M2.5 — send (plaintext).** Core: send a `p.room.message`; **local echo** + a send queue with retry (offline-first). UI: composer.
- [ ] **M2.6 — invites & membership.** Invite a user; accept/decline an invite; show membership changes in the timeline.

**Exit criteria:** two devices (or device + CLI) hold a conversation in an **unencrypted** room with live updates, pagination, offline send-and-retry. Integration test against a oneshot homeserver.

---

## Phase M3 — End-to-end encryption (the headline feature)

Wire `pigeon-crypto` (MLS) into the core and make encrypted rooms transparent in the UI. This is why the shared-Rust-core architecture exists. **Do not start until M1–M2 are solid** — debugging the core⇄UI boundary *and* MLS at once is a trap.

- [ ] **M3.1 — device identity & key publishing.** On login, generate the MLS device identity + KeyPackages via `pigeon-crypto`; upload via `/keys/upload`. Persist MLS state (the `pigeon-crypto` export-storage blob) at rest under the keystore. (Server 3F.7a/b.)
- [ ] **M3.2 — key query / claim.** Core wraps `/keys/query` and `/keys/claim` for establishing groups. (Server 3F.2/3F.3.)
- [ ] **M3.3 — to-device + Welcome handling.** Surface `to_device` from `/sync`; process inbound `p.mls.welcome` to join groups idempotently (at-least-once — Gotcha #8). (Server 3F.4/3F.7c.)
- [ ] **M3.4 — encrypted room creation + invite-with-Welcome.** Create an encrypted room (the `p.room.encryption` marker, group_id = room_id); on invite, claim KeyPackages, `add_member`, ship the `Welcome` over `/sendToDevice`. (Server 3F.6a/3F.7d.)
- [ ] **M3.5 — encrypted send/receive.** Encrypt to `p.room.encrypted`; decrypt inbound transparently and **cache the plaintext locally on first decrypt** (the ratchet advances and is persisted — Gotcha #3). UI shows encrypted rooms identically to plaintext, with an encryption indicator.
- [ ] **M3.6 — encrypted-room UX.** Lock indicators, "encryption enabled" event, graceful `[unable to decrypt]` fallback, device-list visibility.

**Exit criteria:** two devices exchange **real MLS-encrypted** messages through a homeserver that only ever sees ciphertext (assert the stored event has no plaintext — mirror the server's `e2ee_message` test). Negative tests: outsider can't decrypt; tampered ciphertext fails cleanly. **This satisfies the project's headline success criterion.**

---

## Phase M4 — Media, backup/restore, polish, push

Round out the client to "daily-driver" quality.

- [ ] **M4.1 — media upload/download.** `media.rs`: upload (`/_pigeon/media/v1/upload`, respect the size cap → handle 413), download by `pigeon://` URI, thumbnails. Image picker + viewer UI.
- [ ] **M4.2 — encrypted media.** Client-side encrypt before upload / decrypt after download — the server stores opaque ciphertext (no special server path; CLAUDE.md). Inline encrypted images in encrypted rooms.
- [ ] **M4.3 — encrypted device backup & restore.** Wrap `pigeon-crypto`'s recovery-key backup (server 3F.7e): `backup` produces a recovery key + stores the encrypted blob via the key-backup endpoints; `restore` on a fresh device recovers identity + groups. UI: show/save the recovery key, restore flow. Document the forward-secrecy caveat.
- [ ] **M4.4 — push notifications.** FCM integration; wake/sync on push; notify on new messages **without leaking plaintext** in the notification path until decrypted locally. (Confirm the homeserver's push contract first — may be out of this repo's scope.)
- [ ] **M4.5 — polish.** Read markers/receipts (if the server supports them), typing indicators (ditto), profile/display name + avatar, settings, theming, accessibility pass.

**Exit criteria:** send/receive encrypted media; lose and recover a device via the recovery key; receive a push and see a (locally-decrypted) notification. Each with tests.

---

## Phase M5 — iOS

Reuse the **same** core; add the second native UI. The payoff of the architecture.

- [ ] **M5.1 — core as `xcframework`.** Cross-compile the Rust core for `aarch64-apple-ios` (+ simulator targets); package via UniFFI's Swift bindings into an `xcframework`. CI lane for it.
- [ ] **M5.2 — Swift bindings smoke test.** A Hello-core SwiftUI app calls `core_version()` (mirror of M0.4).
- [ ] **M5.3 — iOS OS integration.** Keychain for secrets, APNs for push, native pickers/sharing, background-refresh-aware sync.
- [ ] **M5.4 — feature parity.** SwiftUI screens for M1–M4 flows, driven by the shared core. No new core logic should be needed; any that is, is a sign of a leaky boundary — fix it in the core for both platforms.

**Exit criteria:** the iOS app reaches Android feature parity with **no protocol/crypto code written in Swift** — only UI + OS glue.

---

## Phase M6 — Hardening & release

Continuous, but formalized for store readiness.

- [ ] **M6.1 — performance.** FFI-call batching audit (Gotcha #7), sync/timeline/decrypt benchmarks, binary-size trim (strip, per-ABI bundles), cold-start time.
- [ ] **M6.2 — security pass.** Confirm no plaintext/keys in logs/crash/analytics; keystore usage review; cert/TLS posture; dependency scan (`cargo audit`/`cargo deny` on the core, plus the Android/iOS dep trees); run the server repo's `/security-review` over the diff. An *external* audit is organizational, not a code deliverable.
- [ ] **M6.3 — reliability.** Robust offline/retry behavior, sync-resume after long offline, graceful protocol-error handling, crash-free-session target.
- [ ] **M6.4 — accessibility & i18n.** Screen-reader support, dynamic type, RTL, string externalization.
- [ ] **M6.5 — CI/CD & store.** Signed release builds, Play Store (then App Store) pipelines, staged rollout, privacy declarations (be precise — it's E2EE; the server can't read content).

**Exit criteria:** a signed, store-submittable build with security and accessibility passes done and the release pipeline working.

---

## Cross-cutting success criteria (the honest framing)

- **"It's a real E2EE client"** — *validated* by M3's end-to-end ciphertext-only test, not asserted.
- **"Android + iOS share a core"** — *demonstrated* when M5 reaches parity with zero protocol code in Swift. If Swift needs protocol logic, the boundary leaked — fix the core.
- **"Production-ready"** — store-submittable build + security/accessibility passes (M6). An *external* security audit and real-world uptime are organizational milestones beyond code.
- **Protocol fidelity** — every flow matches the reference CLI's sequence of calls; the federation never sees plaintext. This is non-negotiable and gated per-phase, not deferred to M6.

## Out of scope (flag, don't half-build)

- **MLS membership churn** beyond the server's documented cap (member removal, concurrent/conflicting commits, multi-device epoch reconciliation) — inherits the server repo's 3F scope limit. The app targets small, add-mostly groups until the protocol grows.
- **Running federation on-device.** The app is a Client–Server API consumer only.
- **A web client.** Separate effort; not this repo.
- **Server-side push infrastructure.** This repo integrates against whatever the homeserver exposes; it doesn't build the push fan-out.