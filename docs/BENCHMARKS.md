# Size & performance baselines (mobile client)

The mobile counterpart of the homeserver's
[`../pigeon/docs/BENCHMARKS.md`](../../pigeon/docs/BENCHMARKS.md). Mobile cost shows up
mostly as **binary size** (each Android ABI bundles a full copy of the core, Gotcha #10)
and **build and test time**, so this file records those with the commands to reproduce
them. Re-measure after a dependency bump, a profile change, or anything that might grow
the core, and add the new figures under a dated heading rather than overwriting the old
ones.

> **Read these as same-machine baselines.** Absolute numbers depend on the toolchain and
> the host. What matters is the trend between runs, and the gap between what ships today
> and what a release build would ship.

**Not measured yet:** runtime performance on a device (cold start, sync-batch fold time,
encrypt/decrypt latency, store query time). The core has no micro-benchmark harness. The
server measures its hot paths with `criterion`. Adding it here means a new dev-dependency,
which needs approval first (`CLAUDE.md`, "What Claude Should Not Do"). Until then the
numbers below are size and wall-clock only.

## Running them

```bash
# Android .so sizes: the debug build (dev profile, debug info stripped)
docker compose exec android bash -c 'cd android && ./gradlew assembleDebug'
ls -l android/app/build/rustJniLibs/debug/*/libpigeon_mobile_core.so
ls -l android/app/build/outputs/apk/debug/app-debug.apk

# Android .so size: what a release build would ship (arm64-v8a)
docker compose exec android bash -c 'cd core && cargo ndk -t arm64-v8a -o /tmp/relso build --release \
  && ls -l /tmp/relso/arm64-v8a/libpigeon_mobile_core.so \
  && $ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-strip -o /tmp/s.so /tmp/relso/arm64-v8a/libpigeon_mobile_core.so \
  && ls -l /tmp/s.so && gzip -9c /tmp/s.so | wc -c'

# iOS: the xcframework (release by default) and the built app
ios/build-core.sh && ios/run-tests.sh
ls -l ios/PigeonCore/PigeonCoreFFI.xcframework/ios-arm64/libpigeon_mobile_core.a
du -sh ios/build/DerivedData/Build/Products/Debug-iphonesimulator/Pigeon.app

# Test-suite wall clock (warm, i.e. already compiled)
(cd core && time cargo test -q)
(cd e2e && cargo test)       # prints "finished in …"
```

## Baseline: 2026-11-16

Environment: Apple Silicon Mac. The Android build runs in the repo's `linux/amd64` Docker
container under Rosetta (cargo-ndk, the container's NDK). iOS uses Xcode 27.2.

### Android core (`libpigeon_mobile_core.so`)

| Build | arm64-v8a | Notes |
|---|---:|---|
| Debug, unstripped (before `6298ee9`) | ~145 MB | the ~450 MB APK that wouldn't fit on a default emulator |
| **Debug, DWARF stripped (what ships today)** | **27.5 MiB** (28 838 000 B) | `CARGO_PROFILE_DEV_STRIP=debuginfo`. x86_64: 27.6 MiB |
| Release | 11.3 MiB (11 834 480 B) | `cargo ndk … build --release`, symbols kept |
| Release, fully stripped | 8.1 MiB (8 447 656 B) | `llvm-strip`. UniFFI library-mode bindgen needs the symbols, so strip *after* generating bindings |
| Release, stripped, gzip -9 | 3.6 MiB (3 818 589 B) | rough per-ABI download cost |

| APK | Size |
|---|---:|
| `app-debug.apk` (both ABIs, `.so` stored uncompressed) | 65.1 MiB (68 267 099 B) |

**Takeaways:** (1) A release core is about **2.4× smaller** than the debug one shipped
today, and 3.4× smaller once fully stripped. The Gradle build always runs the debug
cross-build, even for a release APK (see [`OPERATIONS.md`](OPERATIONS.md), "Release
readiness"), so fixing that is the largest size win available. (2) Per-ABI app bundles
(an AAB) would stop every phone from downloading both the arm64 and the x86_64 copy.

### Android update: 2026-11-19 (container rebuilt with NDK r28c)

The `android` container image had been built before the NDK r28c pin and carried NDK 26.3,
so AGP's `stripDebugDebugSymbols` silently skipped. Rebuilt with r28c, it now strips the
packaged copy. Cargo's own output in `build/rustJniLibs` grew a little with the newer
toolchain (27.5 → 30.7 MiB, still DWARF-stripped), which doesn't ship.

| Artifact | arm64-v8a | x86_64 |
|---|---:|---|
| `build/rustJniLibs` (cargo, DWARF stripped; not shipped) | 30.7 MiB (32 169 104 B) | 30.8 MiB (32 279 280 B) |
| **`.so` inside `app-debug.apk` (AGP-stripped)** | **16.1 MiB** (16 877 520 B) | 18.8 MiB (19 730 928 B) |
| `app-debug.apk` (both ABIs) | 44.8 MiB (46 985 328 B), down from 65.1 MiB | |

Measured with `unzip -lv android/app/build/outputs/apk/debug/app-debug.apk` after a clean
`assembleDebug` (delete the old APK first: incremental packaging appends to it).

### Android release build: 2026-11-19

`assembleRelease` now packages a release core (`cargoNdkBuildRelease`: `--release` with
`CARGO_PROFILE_RELEASE_STRIP=debuginfo`), instead of the debug one. Cold build in the
container: 9 min 25 s (Rosetta); the debug core is not compiled.

| Artifact | arm64-v8a | x86_64 |
|---|---:|---|
| `build/rustJniLibs/release` (cargo; not shipped) | 11.8 MiB (12 334 024 B) | 11.6 MiB (12 186 184 B) |
| **`.so` inside `app-release-unsigned.apk` (AGP-stripped)** | **8.0 MiB** (8 404 536 B) | 8.7 MiB (9 162 912 B) |
| `app-release-unsigned.apk` (both ABIs) | 24.1 MiB (25 286 519 B), vs 44.8 MiB debug | |

```bash
docker compose exec android bash -c 'cd android && ./gradlew assembleRelease'
unzip -lv android/app/build/outputs/apk/release/app-release-unsigned.apk | grep core.so
```

### iOS core

| Artifact | Size | Notes |
|---|---:|---|
| `libpigeon_mobile_core.a`, device slice (release) | 50.5 MiB (52 933 640 B) | a static archive *before* linking. Most of it is dead-stripped when the app links |
| `PigeonCoreFFI.xcframework`, total | 150 MiB | device slice plus the fat arm64/x86_64 simulator slice. A build artifact, git-ignored |
| `Pigeon.app` (simulator, debug) | 16 MiB | includes the linked core (`Pigeon.debug.dylib`, 16.7 MB) |

A release device build (`.ipa`) hasn't been measured. That needs the M6 signing setup.

### Build & test time (warm)

| What | Time | Notes |
|---|---:|---|
| `cargo test` in `core/` (117 tests) | ~1.5 s | unit 78 + `http_mock` 32 + `session_persist` 7 |
| `cargo test` in `e2e/` (3 tests) | ~8.7 s | real homeserver + Postgres in Docker, already compiled |
| `./gradlew testDebugUnitTest` (20 JVM tests) | ~46 s | in the container, including the debug cross-build and bindgen it depends on |
| Android release cross-build, arm64-v8a only | ~2 m 50 s | in the Rosetta container |
