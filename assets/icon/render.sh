#!/usr/bin/env bash
#
# Render the raster app icons from pigeon-icon.svg (macOS only: uses Quick Look
# to rasterise the SVG and `sips` to resize — no extra tools to install).
#
#   iOS:     ios/Pigeon/Assets.xcassets/AppIcon.appiconset/AppIcon.png (1024, opaque)
#   Android: android/app/src/main/res/mipmap-*/ic_launcher.png — the legacy icon
#            for API 24-25 only; API 26+ uses the adaptive vector drawables.
#
# The Android vectors are NOT generated: they're the same paths as the SVG,
# translated by hand. Change a shape in the SVG -> mirror it there too.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

qlmanage -t -s 1024 -o "$TMP" "$HERE/pigeon-icon.svg" >/dev/null 2>&1
SRC="$TMP/pigeon-icon.svg.png"
[ -f "$SRC" ] || { echo "Quick Look failed to render the SVG" >&2; exit 1; }

# App Store icons must be opaque (no alpha channel). JPEG can't carry alpha, so
# a max-quality JPEG round-trip flattens it; the icon is a full-bleed square, so
# nothing transparent is lost.
IOS="$ROOT/ios/Pigeon/Assets.xcassets/AppIcon.appiconset/AppIcon.png"
sips -s format jpeg -s formatOptions 100 "$SRC" --out "$TMP/flat.jpg" >/dev/null
sips -s format png "$TMP/flat.jpg" --out "$IOS" >/dev/null

RES="$ROOT/android/app/src/main/res"
for spec in mdpi:48 hdpi:72 xhdpi:96 xxhdpi:144 xxxhdpi:192; do
  dpi="${spec%%:*}"; px="${spec##*:}"
  mkdir -p "$RES/mipmap-$dpi"
  sips -z "$px" "$px" "$IOS" --out "$RES/mipmap-$dpi/ic_launcher.png" >/dev/null
done
echo "rendered: $IOS + $RES/mipmap-*/ic_launcher.png"
