# Pigeon Mobile — iOS (Phase M5)

The iOS app reuses the **same shared Rust core** (`pigeon-mobile-core`) as Android;
the only iOS-new work is the SwiftUI UI + Apple OS integration (Keychain, APNs,
pickers). No protocol or crypto code is written in Swift — that all lives once, in
the core, and reaches Swift through UniFFI-generated bindings. This mirrors how the
Android app consumes the core through generated Kotlin.

## Status (M5.1 — core packaged for Swift)

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

- **M5.2** — a Hello-core SwiftUI smoke app calling `coreVersion()` /
  `selfTestCrypto()` (the mirror of Android's M0.4), to prove the bindings load and
  run on a simulator/device. **Reference source is written** in
  [`HelloCore/`](HelloCore/) (`HelloCoreApp.swift` + `ContentView.swift`), with
  every call matched to the generated Swift signatures. **To run it (macOS):**
  `ios/build-core.sh`, then in Xcode create an iOS App target, add the `PigeonCore`
  Swift package (`ios/PigeonCore`) as a dependency, add the two `HelloCore/*.swift`
  files, and run on a simulator. It is **not compiled in the Linux dev container**
  (no Xcode/Swift), so its on-device run is still pending a Mac.
- **M5.3** — Apple OS integration: Keychain-backed `KeyStore`, `os_log`-backed
  `LogSink`, `PickVisualMedia`-style photo picker, background-refresh-aware sync.
  (APNs push mirrors Android's M4.4 and is **blocked** until the homeserver exposes
  a push contract.)
- **M5.4** — SwiftUI screens for the M1–M4 flows, driven by the shared core, to
  reach Android feature parity. No new core logic should be needed; any that is
  signals a leaky boundary to fix in the core for both platforms.

These require Xcode (Swift compiler + `xcodebuild` + a simulator), so they are
built and validated on macOS, not in the Linux dev container.
