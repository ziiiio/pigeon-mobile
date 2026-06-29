# Pigeon Mobile

The official mobile client for [**Pigeon**](../pigeon) — a federated, end-to-end-encrypted instant messaging system. **Android first, iOS to follow.**

Pigeon Mobile is a thin native UI over a **shared Rust core**. Everything that isn't UI — MLS end-to-end encryption, session/auth, the sync loop, key management, encrypted backup — lives once in `pigeon-mobile-core` (Rust) and is called from Kotlin (and later Swift) through [UniFFI](https://mozilla.github.io/uniffi-rs/)-generated bindings. The crypto engine is the homeserver's own `pigeon-crypto` crate, reused verbatim — so private keys and protocol logic exist in exactly one place. This is the same architecture that powers Element's mobile apps over `matrix-rust-sdk`.

```
   pigeon-mobile-core (Rust)  ──UniFFI──▶  Android (Kotlin / Compose)
   reuses pigeon-crypto + pigeon-core ──▶  iOS (Swift / SwiftUI, later)
                  │ HTTPS, token auth
                  ▼
         Pigeon homeserver  /_pigeon/client/v1/*
```

## Status

🚧 **Phase M0 (foundations) — not yet usable.** The Rust core builds and tests against the reused homeserver crypto crate, and UniFFI generates Kotlin bindings from it (verified in the Docker dev container). Still to come in M0: the Android NDK cross-compile, the Hello-core Compose app, build glue, and CI.

See [`ROADMAP.md`](ROADMAP.md) for the phase plan (M0 toolchain → M1 identity → M2 plaintext messaging → M3 E2EE → M4 media/backup/push → M5 iOS → M6 hardening).

## Why a shared Rust core?

The client must do MLS (RFC 9420) encryption **on-device**, and private keys must never leave it. A working, tested MLS engine already exists in Rust (`pigeon-crypto`). Reimplementing that in Kotlin and again in Swift would mean writing security-critical crypto twice, risking federation incompatibility. Sharing the Rust core keeps crypto and protocol logic in one audited place and makes iOS mostly UI work. The native side owns only UI, navigation, and OS integration (notifications, keystore, pickers).

## Repository layout

```
core/        # pigeon-mobile-core — the shared Rust crate (UniFFI); reuses pigeon-crypto + pigeon-core
android/     # the Android app (Gradle, Kotlin, Jetpack Compose)
ios/         # the iOS app (Phase M5; Swift, SwiftUI)
docs/        # ARCHITECTURE.md
CLAUDE.md    # rulebook for contributors / AI assistants
ROADMAP.md   # the phase plan
```

## Building

The [`pigeon`](../pigeon) homeserver repo must be checked out as a **sibling directory** — the core depends on its `pigeon-core` and `pigeon-crypto` crates by path.

### Dev container (recommended)

A Docker dev environment gives a reproducible toolchain and a persistent container for fast build/test cycles. It bind-mounts the parent `projects/` directory so the `../pigeon` path-deps resolve unchanged.

```bash
docker compose build          # build the dev image (Rust toolchain + rustfmt/clippy)
docker compose up -d          # start the persistent container

# Build + test the core:
docker compose exec -w /workspace/pigeon-mobile/core dev cargo test

# Generate Kotlin bindings from the built cdylib:
docker compose exec -w /workspace/pigeon-mobile/core dev \
  cargo run --bin uniffi-bindgen -- generate \
    --library target/debug/libpigeon_mobile_core.so \
    --language kotlin --out-dir target/bindings/kotlin
```

### Host (alternative)

Needs Rust plus — for the Android steps (M0.3+) — the Android SDK + NDK and [`cargo-ndk`](https://github.com/bbqsrc/cargo-ndk).

```bash
cd core && cargo test                 # build + test the core (no device needed)
cd android && ./gradlew assembleDebug # Android app (once the app exists, M0.4+)
```

## Running against a homeserver

You need a running Pigeon homeserver to connect to. From the [`pigeon`](../pigeon) repo:

```bash
cargo run -p server -- --config config.toml
```

Then point the app at that server's address on first launch. The reference CLI in `../pigeon/clients/cli` exercises every flow this app implements and is the canonical example for each protocol interaction.

## Documentation

- [`CLAUDE.md`](CLAUDE.md) — conventions, gotchas, decisions, the current-phase guard.
- [`ROADMAP.md`](ROADMAP.md) — the full phased plan and success criteria.
- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) — the structural map: core modules, the FFI surface, the build pipeline, end-to-end flows.
- [`../pigeon`](../pigeon) — the homeserver and the **authoritative protocol source**.

## Security

End-to-end encrypted rooms are encrypted on-device via MLS; the homeserver (and every server in the federation) sees only ciphertext. Private keys never leave the device and live inside the Rust core, protected at rest by the platform keystore. Plaintext, tokens, and keys must never appear in logs, metrics, or crash/analytics reports. See [`CLAUDE.md`](CLAUDE.md) gotchas for the full list.

## License

TBD.