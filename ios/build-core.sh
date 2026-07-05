#!/usr/bin/env bash
#
# Build pigeon-mobile-core as an iOS `xcframework` + Swift bindings (M5.1).
#
# This is the iOS analogue of Android's `cargoNdkBuild` + `generateUniffiBindings`
# Gradle tasks: it cross-compiles the shared Rust core for the Apple targets,
# generates the UniFFI **Swift** bindings from the built library, and packages a
# static `xcframework` the Swift package (`ios/PigeonCore`) links against.
#
# ── Where this runs ──────────────────────────────────────────────────────────
# **macOS with Xcode is REQUIRED** for the cross-compile and `xcodebuild` steps:
#   - the Apple targets need the iOS SDK to link (and to C-compile `rusqlite`'s
#     bundled SQLite), which only exists in Xcode;
#   - `xcodebuild -create-xcframework` is macOS-only.
# It cannot run in the Linux dev container. The ONE platform-independent step —
# generating the Swift bindings from the compiled library metadata — is verified
# in the Linux CI/core lane (see `--language swift` below); everything else is
# gated to the macOS CI lane (`.github/workflows/ci.yml` → `ios`).
#
# Usage (on macOS, from the repo root or anywhere):  ios/build-core.sh
set -euo pipefail

CORE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../core" && pwd)"
IOS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BUILD_DIR="$IOS_DIR/build"
PKG_SOURCES="$IOS_DIR/PigeonCore/Sources/PigeonCore"
XCFRAMEWORK="$IOS_DIR/PigeonCore/PigeonCoreFFI.xcframework"

LIB_NAME="libpigeon_mobile_core.a"          # from crate-type = ["staticlib", …]
PROFILE="${PROFILE:-release}"                 # debug|release; release for shipping
CARGO_FLAG=$([ "$PROFILE" = release ] && echo "--release" || echo "")

# Pin the Apple deployment-target floor so the compiled objects match the app's
# minimum (and `PigeonCore/Package.swift`'s `.iOS(.v15)`). Without this, the
# Rust/C objects inherit the installed SDK's default (e.g. iOS 26), and linking
# an iOS-15 app against them spews "built for newer version" warnings.
export IPHONEOS_DEPLOYMENT_TARGET="${IPHONEOS_DEPLOYMENT_TARGET:-15.0}"

# The Apple targets: a device slice (arm64) and a simulator slice (arm64 + x86_64
# lipo'd into one fat lib, so the xcframework's simulator slice runs on both
# Apple-Silicon and Intel Macs).
DEVICE_TARGET="aarch64-apple-ios"
SIM_TARGETS=("aarch64-apple-ios-sim" "x86_64-apple-ios")

echo "==> Ensuring Rust Apple targets are installed"
rustup target add "$DEVICE_TARGET" "${SIM_TARGETS[@]}"

echo "==> Cross-compiling the core for iOS ($PROFILE)"
cd "$CORE_DIR"
for target in "$DEVICE_TARGET" "${SIM_TARGETS[@]}"; do
  cargo build $CARGO_FLAG --target "$target"
done

echo "==> Generating the UniFFI Swift bindings"
# Read the metadata from any built library (target-independent); this is the
# exact step the Linux core lane runs to verify the Swift surface compiles.
# Clear only the generated binding (a git-ignored artifact); keep the tracked
# `.gitkeep` so the SwiftPM target dir survives a clean checkout.
mkdir -p "$PKG_SOURCES"
rm -f "$PKG_SOURCES"/pigeon_mobile_core.swift
cargo run --bin uniffi-bindgen -- generate \
  --library "target/$DEVICE_TARGET/$PROFILE/libpigeon_mobile_core.dylib" \
  --language swift \
  --out-dir "$BUILD_DIR/bindings"
# The generated `.swift` is the module source; the `.h` + `.modulemap` are the C
# shim that the xcframework's headers expose to Swift.
cp "$BUILD_DIR/bindings/pigeon_mobile_core.swift" "$PKG_SOURCES/pigeon_mobile_core.swift"

echo "==> Assembling the simulator fat static lib (arm64 + x86_64)"
mkdir -p "$BUILD_DIR/sim"
lipo -create \
  "target/aarch64-apple-ios-sim/$PROFILE/$LIB_NAME" \
  "target/x86_64-apple-ios/$PROFILE/$LIB_NAME" \
  -output "$BUILD_DIR/sim/$LIB_NAME"

echo "==> Laying out the headers (UniFFI FFI header + module map)"
# xcframework header dirs need a `module.modulemap`; UniFFI emits it named
# `<crate>FFI.modulemap`, so rename it into place next to the header.
HEADERS="$BUILD_DIR/headers"
rm -rf "$HEADERS"
mkdir -p "$HEADERS"
cp "$BUILD_DIR/bindings/pigeon_mobile_coreFFI.h" "$HEADERS/"
cp "$BUILD_DIR/bindings/pigeon_mobile_coreFFI.modulemap" "$HEADERS/module.modulemap"

echo "==> Packaging the xcframework"
rm -rf "$XCFRAMEWORK"
xcodebuild -create-xcframework \
  -library "target/$DEVICE_TARGET/$PROFILE/$LIB_NAME" -headers "$HEADERS" \
  -library "$BUILD_DIR/sim/$LIB_NAME" -headers "$HEADERS" \
  -output "$XCFRAMEWORK"

echo "==> Done. Built $XCFRAMEWORK and Swift bindings in $PKG_SOURCES"
echo "    The Swift package 'ios/PigeonCore' now links against them (import PigeonCore)."
