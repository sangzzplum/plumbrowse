#!/usr/bin/env bash
# Собирает PlumBrowser и запускает как .app (без привязки к терминалу).
# Windows: scripts\run-as-app.ps1
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

if [ "${RELEASE:-0}" = "1" ]; then
  echo "→ cargo build --release..."
  cargo build --release
  TARGET_DIR="$ROOT/target/release"
else
  echo "→ cargo build (dev)..."
  cargo build
  TARGET_DIR="$ROOT/target/debug"
fi

BIN="$TARGET_DIR/plumbrowser"
APP="$TARGET_DIR/PlumBrowser.app"
ICON_PNG="$ROOT/assets/plumnet.png"

mkdir -p "$APP/Contents/MacOS"
cp "$BIN" "$APP/Contents/MacOS/plumbrowser"
chmod +x "$APP/Contents/MacOS/plumbrowser"

ICON_PLIST=""
if [ -f "$ICON_PNG" ] && command -v iconutil >/dev/null && command -v sips >/dev/null; then
  echo "→ building AppIcon.icns..."
  ICONSET="$TARGET_DIR/AppIcon.iconset"
  rm -rf "$ICONSET"
  mkdir -p "$ICONSET"
  for size in 16 32 128 256 512; do
    sips -z "$size" "$size" "$ICON_PNG" --out "$ICONSET/icon_${size}x${size}.png" >/dev/null
    size2=$((size * 2))
    sips -z "$size2" "$size2" "$ICON_PNG" --out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null
  done
  mkdir -p "$APP/Contents/Resources"
  iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/AppIcon.icns"
  rm -rf "$ICONSET"
  ICON_PLIST=$'  <key>CFBundleIconFile</key>\n  <string>AppIcon</string>\n  <key>CFBundleIconName</key>\n  <string>AppIcon</string>\n'
else
  echo "→ icon skipped (need assets/plumnet.png + sips + iconutil)"
fi

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key>
  <string>ru</string>
  <key>CFBundleExecutable</key>
  <string>plumbrowser</string>
  <key>CFBundleIdentifier</key>
  <string>ru.plumbrowse.browser</string>
  <key>CFBundleInfoDictionaryVersion</key>
  <string>6.0</string>
  <key>CFBundleName</key>
  <string>PlumBrowser</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleShortVersionString</key>
  <string>0.1.0</string>
  <key>CFBundleVersion</key>
  <string>0.1.0</string>
${ICON_PLIST}  <key>LSMinimumSystemVersion</key>
  <string>11.0</string>
  <key>NSHighResolutionCapable</key>
  <true/>
</dict>
</plist>
PLIST

echo "→ open $APP"
open "$APP"
