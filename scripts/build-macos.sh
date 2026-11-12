#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
# macOS -> macOS: builds Vessel.app for Apple Silicon and for Intel, and
# packages each as its own .dmg:
#   dist/Vessel-<version>-macos-apple-silicon.dmg
#   dist/Vessel-<version>-macos-intel.dmg
#
#   ARCHS=aarch64-apple-darwin scripts/build-macos.sh   # just one
#
# Needs: Xcode command line tools (xcode-select --install), Rust via rustup.
# GPUI compiles its Metal shaders at runtime, so full Xcode isn't required.
set -euo pipefail

cd "$(dirname "$0")/.."
[[ "$(uname)" == "Darwin" ]] || { echo "build-macos.sh must run on macOS"; exit 1; }

VERSION=${VESSEL_VERSION:-$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)}
DIST=dist
read -r -a ARCHS <<< "${ARCHS:-aarch64-apple-darwin x86_64-apple-darwin}"
export MACOSX_DEPLOYMENT_TARGET=${MACOSX_DEPLOYMENT_TARGET:-11.0}

echo "==> Building Vessel $VERSION for macOS (${ARCHS[*]})"
for target in "${ARCHS[@]}"; do
  rustup target add "$target" >/dev/null
  cargo build --release --target "$target"
done

ICONSET=$(mktemp -d)/Vessel.iconset
mkdir -p "$ICONSET"
for size in 16 32 128 256 512; do
  sips -z $size $size assets/icon.png --out "$ICONSET/icon_${size}x${size}.png" >/dev/null
  sips -z $((size * 2)) $((size * 2)) assets/icon.png --out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null
done

mkdir -p "$DIST"
for target in "${ARCHS[@]}"; do
case "$target" in
  aarch64-*) LABEL=apple-silicon ;;
  x86_64-*) LABEL=intel ;;
  *) LABEL=$target ;;
esac
APP="$DIST/$LABEL/Vessel.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "target/$target/release/vessel" "$APP/Contents/MacOS/vessel"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Vessel</string>
  <key>CFBundleDisplayName</key><string>Vessel</string>
  <key>CFBundleIdentifier</key><string>io.github.p1e2r.vessel</string>
  <key>CFBundleExecutable</key><string>vessel</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleIconFile</key><string>Vessel.icns</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>LSMinimumSystemVersion</key><string>$MACOSX_DEPLOYMENT_TARGET</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
  <key>NSLocalNetworkUsageDescription</key>
  <string>Vessel connects to SSH, Remote Desktop and VNC hosts on your local network.</string>
</dict>
</plist>
PLIST

iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/Vessel.icns"

echo "==> Signing $LABEL (ad-hoc; set SIGN_IDENTITY to sign with a Developer ID)"
codesign --force --deep --sign "${SIGN_IDENTITY:--}" "$APP"
codesign --verify --strict "$APP"

echo "==> Packaging .dmg"
DMG="$DIST/Vessel-$VERSION-macos-$LABEL.dmg"
rm -f "$DMG"
STAGE=$(mktemp -d)
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
hdiutil create -volname "Vessel" -srcfolder "$STAGE" -ov -format UDZO "$DMG" >/dev/null
rm -rf "$STAGE"

echo "==> macOS ($LABEL): $DMG"
done
