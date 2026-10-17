#!/bin/bash
# Builds dist/DevDisk.app (release, ad-hoc signed). Read-only on your system
# apart from writing into ./target and ./dist.
#
#   ./bundle.sh                # this Mac's architecture
#   UNIVERSAL=1 ./bundle.sh    # Apple silicon + Intel (needs both rustup targets)
#   PACKAGE=1 ./bundle.sh      # also writes dist/DevDisk-<version>.zip and .dmg
#   SIGN_IDENTITY="Developer ID Application: …" ./bundle.sh   # real signature
set -euo pipefail
cd "$(dirname "$0")"

VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
APP="dist/DevDisk.app"

if [ "${UNIVERSAL:-0}" = 1 ]; then
  cargo build --release --target aarch64-apple-darwin
  cargo build --release --target x86_64-apple-darwin
  BIN="target/universal/devdisk"
  mkdir -p target/universal
  lipo -create -output "$BIN" \
    target/aarch64-apple-darwin/release/devdisk \
    target/x86_64-apple-darwin/release/devdisk
else
  cargo build --release
  BIN="target/release/devdisk"
fi

# AppIcon.icns is generated (not committed) from assets/icon_1024.png.
if [ ! -f assets/AppIcon.icns ] || [ assets/icon_1024.png -nt assets/AppIcon.icns ]; then
  SET="$(mktemp -d)/AppIcon.iconset"
  mkdir -p "$SET"
  for s in 16 32 128 256 512; do
    sips -z $s $s assets/icon_1024.png --out "$SET/icon_${s}x${s}.png" >/dev/null
    sips -z $((s * 2)) $((s * 2)) assets/icon_1024.png --out "$SET/icon_${s}x${s}@2x.png" >/dev/null
  done
  iconutil -c icns "$SET" -o assets/AppIcon.icns
fi

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/DevDisk"
cp assets/AppIcon.icns "$APP/Contents/Resources/AppIcon.icns"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>DevDisk</string>
  <key>CFBundleDisplayName</key><string>DevDisk</string>
  <key>CFBundleIdentifier</key><string>com.entange.devdisk</string>
  <key>CFBundleExecutable</key><string>DevDisk</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${VERSION}</string>
  <key>CFBundleVersion</key><string>${VERSION}</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.utilities</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

if [ -n "${SIGN_IDENTITY:-}" ]; then
  codesign --force --options runtime --timestamp --sign "$SIGN_IDENTITY" "$APP"
else
  codesign --force --sign - "$APP" >/dev/null 2>&1 || true
fi
echo "Built $APP ($VERSION)"

if [ "${PACKAGE:-0}" = 1 ]; then
  ZIP="dist/DevDisk-${VERSION}.zip"
  DMG="dist/DevDisk-${VERSION}.dmg"
  rm -f "$ZIP" "$DMG"
  ditto -c -k --keepParent "$APP" "$ZIP"

  STAGE="$(mktemp -d)"
  cp -R "$APP" "$STAGE/"
  ln -s /Applications "$STAGE/Applications"
  hdiutil create -volname DevDisk -srcfolder "$STAGE" -ov -format UDZO "$DMG" >/dev/null
  rm -rf "$STAGE"
  if [ -n "${SIGN_IDENTITY:-}" ]; then
    codesign --force --timestamp --sign "$SIGN_IDENTITY" "$DMG"
  fi
  echo "Packaged $ZIP and $DMG"
fi
