#!/usr/bin/env bash
#
# Build the real Pigeon app and run its unit tests on an iOS simulator (M5.3).
# Currently: the KeychainKeyStore suite — exercising the real iOS Keychain, the
# store where the session token + MLS device state live (Gotcha #1). As M5.4
# lands view-model tests they run here too. The iOS analogue of Android's
# `./gradlew testDebugUnitTest`.
#
# Signing note: a generic-password Keychain item needs the app signed with a
# keychain-access-group entitlement (`Pigeon/Pigeon.entitlements`), else SecItem
# returns errSecMissingEntitlement (-34018) on the simulator. So we do NOT pass
# CODE_SIGNING_ALLOWED=NO; we ad-hoc sign (`CODE_SIGN_IDENTITY=-`, no
# team/profile needed for the simulator).
#
# macOS + Xcode + a simulator runtime only; cannot run in the Linux dev container.
set -euo pipefail

IOS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$IOS_DIR"

PROJECT="Pigeon.xcodeproj"
SCHEME="Pigeon"
DERIVED="build/DerivedData"
SIM_NAME="pigeon-sim"            # a dedicated device, created if absent

echo "==> Selecting a simulator"
sim_udid="$(xcrun simctl list devices --json \
  | /usr/bin/python3 -c 'import json,sys
d=json.load(sys.stdin)["devices"]
for rt,devs in d.items():
    for x in devs:
        if x["name"]=="'"$SIM_NAME"'" and x.get("isAvailable"):
            print(x["udid"]); sys.exit(0)')" || true

if [ -z "${sim_udid:-}" ]; then
  # Newest iOS runtime that supports a plain-numbered iPhone ("iPhone 17"), paired
  # with the newest such iPhone *that runtime* supports. Some runtimes are
  # device-specific (e.g. one that only supports a single model), so the runtime
  # and device type must be chosen together or `simctl create` fails with
  # "Incompatible device".
  read -r runtime devtype < <(xcrun simctl list runtimes --json \
    | /usr/bin/python3 -c 'import json,sys
def ver(r): return tuple(int(p) for p in r["version"].split("."))
def num(t):
    n=t["name"].replace("iPhone","").strip()
    return int(n) if n.isdigit() else None
rs=[r for r in json.load(sys.stdin)["runtimes"] if r.get("isAvailable") and r["name"].startswith("iOS")]
for r in sorted(rs,key=ver,reverse=True):
    ts=[t for t in r.get("supportedDeviceTypes",[]) if num(t) is not None]
    if ts:
        print(r["identifier"], max(ts,key=num)["identifier"]); break') || true
  [ -n "${runtime:-}" ] || { echo "error: no iOS simulator runtime with a supported iPhone" >&2; exit 1; }
  sim_udid="$(xcrun simctl create "$SIM_NAME" "$devtype" "$runtime")"
fi
echo "    using simulator $sim_udid"

echo "==> Building + testing $SCHEME (ad-hoc signed for the Keychain entitlement)"
xcodebuild -project "$PROJECT" -scheme "$SCHEME" -configuration Debug \
  -destination "id=$sim_udid" -derivedDataPath "$DERIVED" \
  CODE_SIGN_IDENTITY="-" CODE_SIGN_STYLE=Manual PROVISIONING_PROFILE_SPECIFIER="" \
  test

echo "PASS: Pigeon unit tests green on the simulator."
