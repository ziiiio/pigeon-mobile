#!/usr/bin/env bash
#
# Build the Hello-core smoke app (M5.2) and RUN it on an iOS simulator, asserting
# the Rust core → xcframework → UniFFI → Swift → SwiftUI pipeline round-trips
# on-device. This is the iOS mirror of Android's `assembleDebug` + emulator check.
#
# It is driven both by a developer (`ios/run-hellocore.sh`) and by the macOS CI
# lane (`.github/workflows/ci.yml` → `ios`). It asserts, from the app's os_log
# output, that:
#   - the host LogSink installed (the M0.7 callback round-trips), and
#   - `coreVersion()` and `selfTestCrypto()` both returned through the bindings
#     (the whole pipeline, not just a launch).
#
# Prereq: `ios/build-core.sh` has produced the xcframework + Swift bindings.
# (CI runs it in the preceding step; run it yourself first if building locally.)
#
# ── Where this runs ──────────────────────────────────────────────────────────
# macOS with Xcode + an iOS simulator runtime. It cannot run in the Linux dev
# container (no simulator). Everything here is `xcrun simctl` / `xcodebuild`.
set -euo pipefail

IOS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$IOS_DIR"

PROJECT="HelloCore.xcodeproj"
SCHEME="HelloCore"
BUNDLE_ID="com.pigeon.mobile.hellocore"
DERIVED="build/DerivedData"
SUBSYSTEM="com.pigeon.mobile"
SIM_NAME="pigeon-hellocore-sim"          # a dedicated device, created if absent
LOG_MARKER="log sink installed"
VERSION_MARKER="pigeon-mobile-core"
CRYPTO_MARKER="crypto self-test ok, key=32 bytes"

if [ ! -d "PigeonCore/PigeonCoreFFI.xcframework" ]; then
  echo "error: PigeonCore/PigeonCoreFFI.xcframework missing — run ios/build-core.sh first" >&2
  exit 1
fi

# ── Pick a simulator runtime + device type available on this host ────────────
# Names of concrete devices vary across Xcode versions / CI images, so create a
# dedicated device from whatever iOS runtime and a common iPhone device type are
# installed. Reuse it across runs if it already exists.
echo "==> Selecting a simulator"
sim_udid="$(xcrun simctl list devices --json \
  | /usr/bin/python3 -c 'import json,sys
d=json.load(sys.stdin)["devices"]
for rt,devs in d.items():
    for x in devs:
        if x["name"]=="'"$SIM_NAME"'" and x.get("isAvailable"):
            print(x["udid"]); sys.exit(0)')" || true

if [ -z "${sim_udid:-}" ]; then
  runtime="$(xcrun simctl list runtimes --json \
    | /usr/bin/python3 -c 'import json,sys
r=[x for x in json.load(sys.stdin)["runtimes"] if x.get("isAvailable") and "iOS" in x["name"]]
print(sorted(r,key=lambda x:x["version"])[-1]["identifier"] if r else "")')"
  [ -n "$runtime" ] || { echo "error: no available iOS simulator runtime" >&2; exit 1; }
  devtype="$(xcrun simctl list devicetypes --json \
    | /usr/bin/python3 -c 'import json,sys
t=[x for x in json.load(sys.stdin)["devicetypes"] if x["name"].startswith("iPhone")]
# Prefer a plain "iPhone <n>" over Pro/Plus/mini for stability.
plain=[x for x in t if x["name"].replace("iPhone","").strip().isdigit()]
print((sorted(plain,key=lambda x:int(x["name"].split()[-1]))[-1] if plain else t[-1])["identifier"])')"
  echo "    creating $SIM_NAME ($devtype on $runtime)"
  sim_udid="$(xcrun simctl create "$SIM_NAME" "$devtype" "$runtime")"
fi
echo "    using simulator $sim_udid"

# ── Build the app for that simulator ─────────────────────────────────────────
echo "==> Building $SCHEME for the simulator"
xcodebuild -project "$PROJECT" -scheme "$SCHEME" -configuration Debug \
  -destination "id=$sim_udid" -derivedDataPath "$DERIVED" \
  CODE_SIGNING_ALLOWED=NO build

APP="$DERIVED/Build/Products/Debug-iphonesimulator/HelloCore.app"
[ -d "$APP" ] || { echo "error: built app not found at $APP" >&2; exit 1; }

# ── Boot, install, capture logs across a launch ──────────────────────────────
echo "==> Booting + installing"
xcrun simctl bootstatus "$sim_udid" -b >/dev/null
xcrun simctl install "$sim_udid" "$APP"
xcrun simctl terminate "$sim_udid" "$BUNDLE_ID" 2>/dev/null || true

logfile="$(mktemp -t hellocore-oslog.XXXXXX)"
echo "==> Launching + capturing os_log"
xcrun simctl spawn "$sim_udid" log stream --level info --style compact \
  --predicate "subsystem == \"$SUBSYSTEM\"" > "$logfile" 2>&1 &
stream_pid=$!
sleep 1
xcrun simctl launch "$sim_udid" "$BUNDLE_ID" >/dev/null

# Wait (up to ~15s) for the three expected records to land.
ok=0
for _ in $(seq 1 15); do
  if grep -q "$LOG_MARKER" "$logfile" \
     && grep -q "$VERSION_MARKER" "$logfile" \
     && grep -q "$CRYPTO_MARKER" "$logfile"; then
    ok=1; break
  fi
  sleep 1
done
kill "$stream_pid" 2>/dev/null || true

echo "---- captured core-subsystem log ----"
grep -iE "$LOG_MARKER|$VERSION_MARKER|$CRYPTO_MARKER|FAILED" "$logfile" || true
echo "-------------------------------------"

if [ "$ok" -ne 1 ]; then
  echo "FAIL: expected log records not observed (LogSink + coreVersion + selfTestCrypto)" >&2
  rm -f "$logfile"
  exit 1
fi

rm -f "$logfile"
echo "PASS: Hello-core ran on the simulator — LogSink round-trips and both"
echo "      coreVersion() and selfTestCrypto() returned through the bindings."
