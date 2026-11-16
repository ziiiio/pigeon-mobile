# App icon

`pigeon-icon.svg` is the **single source** for the Android and iOS app icons, so
both apps show the same icon. It is a full-bleed 1024×1024 square, and each
platform applies its own mask (iOS rounded square, Android launcher shape).

| Platform | Asset | How it's made |
|---|---|---|
| iOS | `ios/Pigeon/Assets.xcassets/AppIcon.appiconset/AppIcon.png` | `./render.sh` (opaque 1024 PNG; Xcode derives every size) |
| Android 8+ (API 26+) | `android/app/src/main/res/drawable/ic_launcher_{background,foreground,monochrome}.xml` + `mipmap-anydpi-v26/ic_launcher.xml` | the SVG's paths, **translated by hand** (vector drawables) |
| Android 7 (API 24–25) | `android/app/src/main/res/mipmap-*dpi/ic_launcher.png` | `./render.sh` |

**Keeping them identical.** The Android vectors use a 1536 viewport (the 108dp
adaptive layer). Launchers show only the middle 72dp, so the SVG's canvas sits at
offset 256: the visible part is exactly the square iOS shows. The bird stays
within a radius-469 circle around the centre (Android's 66dp safe zone, measured
against that 72dp visible area), so no launcher mask clips it. The monochrome layer (Android 13 themed icons) is the bird silhouette.

**Changing the icon:** edit the SVG, mirror the shape changes in the three
Android drawables, then run `./render.sh` (macOS: Quick Look + `sips`, no
installs) and commit the regenerated PNGs.
