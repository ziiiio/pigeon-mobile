# Pigeon Mobile — iOS (Phase M5)

The iOS app reuses the **same shared Rust core** (`pigeon-mobile-core`) as Android;
the only iOS-new work is the SwiftUI UI + Apple OS integration (Keychain, APNs,
pickers). No protocol or crypto code is written in Swift — that all lives once, in
the core, and reaches Swift through UniFFI-generated bindings. This mirrors how the
Android app consumes the core through generated Kotlin.

## Status (M5.3 — core packaged for Swift, Hello-core runs, OS-integration layer built + tested)

- ✅ **Swift bindings generate cleanly** from the core. The full FFI surface comes
  through: `PigeonClient` (with its `async throws` methods), the records
  (`Session`, `Room`, `TimelineEvent`, `ImageContent`), the callback protocols
  (`KeyStore`, `LogSink`, `SyncObserver`), the enums (`ErrorCode`, `LogLevel`,
  `CoreError`), and the free functions (`login`, `register`, `restoreSession`, …).
  This is the platform-independent step and is exercised on any host (the Linux
  core CI lane runs `uniffi-bindgen … --language swift`).
- ✅ **Build pipeline scaffolded**: [`build-core.sh`](build-core.sh) cross-compiles
  the core for the Apple targets, generates the Swift bindings, and assembles the
  `xcframework`; [`PigeonCore/Package.swift`](PigeonCore/Package.swift) packages it
  as a SwiftPM dependency (`import PigeonCore`).
- ⏳ **macOS-gated (not runnable in the Linux dev container):** the `cargo build`
  for the Apple targets (the iOS SDK is needed to link and to C-compile
  `rusqlite`'s bundled SQLite) and `xcodebuild -create-xcframework`. These run on
  the **macOS CI lane** (`.github/workflows/ci.yml` → `ios`) and on a developer
  Mac. The dev container can only generate the bindings, not the xcframework.

## Building the core for iOS (on macOS with Xcode)

```sh
ios/build-core.sh          # release by default; PROFILE=debug for a debug build
```

This produces:
- `ios/PigeonCore/PigeonCoreFFI.xcframework` — the compiled core (device arm64 +
  a fat simulator slice: arm64 + x86_64), and
- `ios/PigeonCore/Sources/PigeonCore/pigeon_mobile_core.swift` — the generated
  bindings.

Both are git-ignored build artifacts (like Android's `.so` + generated Kotlin);
run the script before opening the app.

## Next (M5.2 → M5.4, macOS-gated)

- ✅ **M5.2 — Hello-core smoke app (built + run on a simulator).** A SwiftUI app
  ([`HelloCore/`](HelloCore/)) calling `coreVersion()` / `selfTestCrypto()` — the
  mirror of Android's M0.4 — proving the bindings load and run on-device. It is
  packaged by a checked-in Xcode project ([`HelloCore.xcodeproj`](HelloCore.xcodeproj),
  hand-written so no `xcodegen`/`tuist` is needed) consuming the local `PigeonCore`
  Swift package. **To build + run it (macOS):**

  ```sh
  ios/build-core.sh        # produce the xcframework + Swift bindings first
  ios/run-hellocore.sh     # build the app, boot a simulator, run + assert
  ```

  `run-hellocore.sh` boots a simulator, installs, launches, and asserts from the
  app's `os_log` output that the `LogSink` round-trips and both Rust-computed
  values returned through the bindings. It backs the **macOS CI lane**. Verified
  green on an `iPhone 16` simulator (iOS 18.4). Still **not runnable in the Linux
  dev container** (no simulator) — that only generates the bindings.
- ✅ **M5.3 — Apple OS integration (built + tested).** The real `Pigeon` app
  ([`Pigeon/`](Pigeon/), [`Pigeon.xcodeproj`](Pigeon.xcodeproj)) lands the
  OS-integration layer over the shared core: `KeychainKeyStore` (the `KeyStore`
  over the iOS Keychain), `OsLogSink` (`LogSink` over `os_log`), `PhotoPicker`
  (`PhotosPicker` → bytes for the core to encrypt+upload), and `SyncController`
  (background-refresh-aware sync lifecycle). `PigeonApp` installs the three host
  callbacks at launch, mirroring Android's `PigeonApp`. Run + test on macOS:

  ```sh
  ios/build-core.sh        # xcframework + bindings (if not already built)
  ios/run-tests.sh         # build the app + run the Keychain suite on a simulator
  ```

  `KeychainKeyStoreTests` (7 tests) exercises the **real iOS Keychain** — the app
  is ad-hoc signed with a `keychain-access-groups` entitlement so `SecItem`
  works on the simulator. Wired into the macOS CI lane. **APNs push is blocked**
  (no homeserver push contract — inherits M4.4).
- **M5.4** — SwiftUI screens for the M1–M4 flows, driven by the shared core, to
  reach Android feature parity. No new core logic should be needed; any that is
  signals a leaky boundary to fix in the core for both platforms.

These require Xcode (Swift compiler + `xcodebuild` + a simulator), so they are
built and validated on macOS, not in the Linux dev container.
