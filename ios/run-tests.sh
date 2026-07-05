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
# returns errSecMissingEntitlement (-34018) on the simulator. So — unlike the
# Hello-core smoke — we do NOT pass CODE_SIGNING_ALLOWED=NO; we ad-hoc sign
# (`CODE_SIGN_IDENTITY=-`, no team/profile needed for the simulator).
#
# macOS + Xcode + a simulator runtime only; cannot run in the Linux dev container.
set -euo pipefail

IOS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$IOS_DIR"

PROJECT="Pigeon.xcodeproj"
SCHEME="Pigeon"
DERIVED="build/DerivedData"
SIM_NAME="pigeon-hellocore-sim"   # reuse the smoke-test device if present

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

echo "==> Building + testing $SCHEME (ad-hoc signed for the Keychain entitlement)"
xcodebuild -project "$PROJECT" -scheme "$SCHEME" -configuration Debug \
  -destination "id=$sim_udid" -derivedDataPath "$DERIVED" \
  CODE_SIGN_IDENTITY="-" CODE_SIGN_STYLE=Manual PROVISIONING_PROFILE_SPECIFIER="" \
  test

echo "PASS: Pigeon unit tests green on the simulator."
