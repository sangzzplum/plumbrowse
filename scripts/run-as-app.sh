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

mkdir -p "$APP/Contents/MacOS"
cp "$BIN" "$APP/Contents/MacOS/plumbrowser"
chmod +x "$APP/Contents/MacOS/plumbrowser"

cat > "$APP/Contents/Info.plist" <<'PLIST'
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
  <key>LSMinimumSystemVersion</key>
  <string>11.0</string>
  <key>NSHighResolutionCapable</key>
  <true/>
</dict>
</plist>
PLIST

echo "→ open $APP"
open "$APP"
