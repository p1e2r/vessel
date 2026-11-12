#!/usr/bin/env bash
# Build Vessel.app for macOS.
#
#   ./build-mac.sh            # this Mac's architecture
#   ./build-mac.sh arm64      # Apple Silicon
#   ./build-mac.sh x86_64     # Intel
#   ./build-mac.sh universal  # both, merged with lipo
#
# Output: dist/Vessel.app (and dist/Vessel-<arch>.zip)
set -euo pipefail
cd "$(dirname "$0")"

ARCH="${1:-native}"
APP="dist/Vessel.app"
VERSION="$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)"

if ! command -v cargo >/dev/null; then
  echo "Rust is not installed. Get it from https://rustup.rs" >&2
  exit 1
fi
if ! xcode-select -p >/dev/null 2>&1; then
  echo "Installing Xcode Command Line Tools (needed for the linker)…"
  xcode-select --install || true
  exit 1
fi

build_target() {
  local triple="$1"
  rustup target add "$triple" >/dev/null 2>&1 || true
  cargo build --release --target "$triple"
}

case "$ARCH" in
  native)
    cargo build --release
    BIN="target/release/vessel"
    ;;
  arm64)
    build_target aarch64-apple-darwin
    BIN="target/aarch64-apple-darwin/release/vessel"
    ;;
  x86_64)
    build_target x86_64-apple-darwin
    BIN="target/x86_64-apple-darwin/release/vessel"
    ;;
  universal)
    build_target aarch64-apple-darwin
    build_target x86_64-apple-darwin
    mkdir -p target/universal
    lipo -create -output target/universal/vessel \
      target/aarch64-apple-darwin/release/vessel \
      target/x86_64-apple-darwin/release/vessel
    BIN="target/universal/vessel"
    ;;
  *)
    echo "unknown arch: $ARCH (use arm64, x86_64, universal)" >&2
    exit 1
    ;;
esac

echo "Bundling $APP …"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/vessel"

# Icon: assets/icon.png -> AppIcon.icns
ICONSET="$(mktemp -d)/AppIcon.iconset"
mkdir -p "$ICONSET"
for s in 16 32 128 256 512; do
  sips -z $s $s assets/icon.png --out "$ICONSET/icon_${s}x${s}.png" >/dev/null
  sips -z $((s*2)) $((s*2)) assets/icon.png --out "$ICONSET/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/AppIcon.icns"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Vessel</string>
  <key>CFBundleDisplayName</key><string>Vessel</string>
  <key>CFBundleIdentifier</key><string>dev.vessel.terminal</string>
  <key>CFBundleExecutable</key><string>vessel</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${VERSION}</string>
  <key>CFBundleVersion</key><string>${VERSION}</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
  <key>NSLocalNetworkUsageDescription</key>
  <string>Vessel connects to SSH, Remote Desktop and VNC hosts on your local network.</string>
</dict>
</plist>
PLIST

# Ad-hoc sign so Gatekeeper and the Keychain treat it as one stable app on this Mac.
codesign --force --deep --sign - "$APP" >/dev/null 2>&1 || echo "(codesign skipped)"

(cd dist && rm -f "Vessel-$ARCH.zip" && ditto -c -k --keepParent Vessel.app "Vessel-$ARCH.zip")

echo
echo "Done: $APP"
echo "Run it:      open $APP"
echo "Install it:  cp -R $APP /Applications/"
