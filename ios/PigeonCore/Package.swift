// swift-tools-version:5.9
//
// PigeonCore — the shared Rust core, packaged for Swift (M5.1).
//
// This exposes `pigeon-mobile-core` to the (future, M5.2+) SwiftUI app as a
// Swift Package: `import PigeonCore` gives the same API the Android app drives
// through the generated Kotlin — `login`, `PigeonClient`, `SyncObserver`, etc.
// No protocol or crypto code is written in Swift; this is only the generated
// UniFFI binding plus the compiled Rust `xcframework` (mirrors matrix-rust-sdk's
// Swift packaging).
//
// Both products of this package are produced by `ios/build-core.sh` (macOS):
//   - `PigeonCoreFFI.xcframework` — the compiled core (device + simulator slices)
//   - `Sources/PigeonCore/pigeon_mobile_core.swift` — the generated bindings
// They are git-ignored build artifacts; run the script before building the app.
import PackageDescription

let package = Package(
    name: "PigeonCore",
    platforms: [.iOS(.v15)],
    products: [
        .library(name: "PigeonCore", targets: ["PigeonCore"]),
    ],
    targets: [
        // The compiled Rust core as a binary xcframework (built by build-core.sh).
        .binaryTarget(
            name: "PigeonCoreFFI",
            path: "PigeonCoreFFI.xcframework"
        ),
        // The generated Swift bindings, linked against the xcframework's C shim.
        .target(
            name: "PigeonCore",
            dependencies: ["PigeonCoreFFI"],
            path: "Sources/PigeonCore"
        ),
    ]
)
