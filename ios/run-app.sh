#!/usr/bin/env bash
#
# Build the real Pigeon app (M5.4) and LAUNCH it on an iOS simulator, asserting
# it boots into the SwiftUI UI without crashing. This is the app-level analogue
# of `run-hellocore.sh`: `run-tests.sh` proves the app *compiles* and its unit
# suites pass, but only actually launching it proves the SwiftUI graph
# (RootView → AuthViewModel → the M1–M4 screens) renders on-device and the core's
# host callbacks install at process start. The mirror of Android's emulator run.
#
# What it asserts, from the app's os_log:
#   - "PigeonApp: core callbacks installed" — the LogSink/KeyStore/store-dir
#     callbacks installed at launch (the whole OS-integration layer is wired), and
#   - the app process is still alive a few seconds after launch — i.e. it reached
#     and held the auth screen rather than crashing on the way up.
#
# The full login → rooms → send/receive flow needs a live homeserver and is
# covered by the core's e2e suite (the iOS app drives the exact same FFI as
# Android); this script is the iOS-specific "the app runs" gate.
#
# Signing: like run-tests.sh (and unlike the Hello-core smoke), we ad-hoc sign so
# the Keychain entitlement is present — session restore reads the Keychain at
# launch, which returns errSecMissingEntitlement on an unsigned build.
#
# macOS + Xcode + a simulator runtime only; cannot run in the Linux dev container.
set -euo pipefail

IOS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$IOS_DIR"

PROJECT="Pigeon.xcodeproj"
SCHEME="Pigeon"
BUNDLE_ID="com.pigeon.mobile"
DERIVED="build/DerivedData"
SUBSYSTEM="com.pigeon.mobile"
SIM_NAME="pigeon-hellocore-sim"   # reuse the smoke-test device if present
INSTALL_MARKER="PigeonApp: core callbacks installed"

if [ ! -d "PigeonCore/PigeonCoreFFI.xcframework" ]; then
  echo "error: PigeonCore/PigeonCoreFFI.xcframework missing — run ios/build-core.sh first" >&2
  exit 1
fi

# ── Pick/create a simulator (same logic as run-hellocore.sh / run-tests.sh) ───
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
plain=[x for x in t if x["name"].replace("iPhone","").strip().isdigit()]
print((sorted(plain,key=lambda x:int(x["name"].split()[-1]))[-1] if plain else t[-1])["identifier"])')"
  sim_udid="$(xcrun simctl create "$SIM_NAME" "$devtype" "$runtime")"
fi
echo "    using simulator $sim_udid"

# ── Build (ad-hoc signed for the Keychain entitlement) ───────────────────────
echo "==> Building $SCHEME for the simulator"
xcodebuild -project "$PROJECT" -scheme "$SCHEME" -configuration Debug \
  -destination "id=$sim_udid" -derivedDataPath "$DERIVED" \
  CODE_SIGN_IDENTITY="-" CODE_SIGN_STYLE=Manual PROVISIONING_PROFILE_SPECIFIER="" \
  build

APP="$DERIVED/Build/Products/Debug-iphonesimulator/Pigeon.app"
[ -d "$APP" ] || { echo "error: built app not found at $APP" >&2; exit 1; }

# ── Boot, install, launch, capture logs ──────────────────────────────────────
echo "==> Booting + installing"
xcrun simctl bootstatus "$sim_udid" -b >/dev/null
xcrun simctl install "$sim_udid" "$APP"
xcrun simctl terminate "$sim_udid" "$BUNDLE_ID" 2>/dev/null || true

logfile="$(mktemp -t pigeon-oslog.XXXXXX)"
echo "==> Launching + capturing os_log"
xcrun simctl spawn "$sim_udid" log stream --level info --style compact \
  --predicate "subsystem == \"$SUBSYSTEM\"" > "$logfile" 2>&1 &
stream_pid=$!
sleep 1
# --console-pty keeps launch attached; we background it and check liveness below.
app_pid="$(xcrun simctl launch "$sim_udid" "$BUNDLE_ID" | awk '{print $NF}')"
echo "    launched pid $app_pid"

# Wait (up to ~15s) for the callbacks-installed record.
ok=0
for _ in $(seq 1 15); do
  if grep -q "$INSTALL_MARKER" "$logfile"; then ok=1; break; fi
  sleep 1
done

# Liveness: the process must still be running (didn't crash reaching the UI).
alive=0
if xcrun simctl spawn "$sim_udid" launchctl print "system/UIKitApplication:$BUNDLE_ID" >/dev/null 2>&1; then
  alive=1
elif kill -0 "$app_pid" 2>/dev/null; then
  alive=1
fi

kill "$stream_pid" 2>/dev/null || true

echo "---- captured core-subsystem log ----"
grep -iE "$INSTALL_MARKER|error|FAILED|crash" "$logfile" || true
echo "-------------------------------------"

if [ "$ok" -ne 1 ]; then
  echo "FAIL: '$INSTALL_MARKER' not observed — the core callbacks did not install" >&2
  rm -f "$logfile"; exit 1
fi
if [ "$alive" -ne 1 ]; then
  echo "FAIL: the app is no longer running — it crashed on the way to the UI" >&2
  rm -f "$logfile"; exit 1
fi

rm -f "$logfile"
echo "PASS: Pigeon launched on the simulator — core callbacks installed and the"
echo "      app reached and held its SwiftUI UI (auth screen) without crashing."
