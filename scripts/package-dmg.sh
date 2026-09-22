#!/usr/bin/env bash
#
# Build a distributable DMG from the packaged .app.
#
# `pnpm tauri build --bundles dmg` drives Tauri's own `bundle_dmg.sh`, which
# arranges the window with Finder AppleScript and therefore fails in any
# session without a controllable Finder (remote shells, CI, sandboxes).
# `hdiutil` needs no Finder and produces a perfectly ordinary installer, so the
# DMG is created here instead.
#
#   pnpm tauri build          # produces the .app
#   ./scripts/package-dmg.sh  # wraps it into a .dmg
#
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

APP_NAME="DataSync Studio"
APP_PATH="$ROOT/src-tauri/target/release/bundle/macos/$APP_NAME.app"
DMG_DIR="$ROOT/src-tauri/target/release/bundle/dmg"

if [[ ! -d "$APP_PATH" ]]; then
  echo "未找到 $APP_PATH" >&2
  echo "请先运行: pnpm tauri build" >&2
  exit 1
fi

VERSION="$(/usr/libexec/PlistBuddy -c 'Print CFBundleShortVersionString' \
  "$APP_PATH/Contents/Info.plist" 2>/dev/null || echo "0.0.0")"
ARCH="$(uname -m)"
DMG_PATH="$DMG_DIR/${APP_NAME}_${VERSION}_${ARCH}.dmg"

mkdir -p "$DMG_DIR"
rm -f "$DMG_PATH"

# Stage the app next to an /Applications symlink so the mounted volume can be
# used the usual drag-and-drop way.
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
cp -R "$APP_PATH" "$STAGE/"
ln -s /Applications "$STAGE/Applications"

echo "正在打包 $APP_NAME $VERSION ($ARCH) …"
# `-quiet` is deliberately not used: a silent failure here leaves the user with
# no DMG and no explanation.
hdiutil create \
  -volname "$APP_NAME" \
  -srcfolder "$STAGE" \
  -ov \
  -format UDZO \
  "$DMG_PATH"

# Ad-hoc sign so Gatekeeper does not report a damaged bundle on first open.
codesign --force --deep --sign - "$APP_PATH" >/dev/null 2>&1 || true

SIZE="$(du -h "$DMG_PATH" | cut -f1)"
echo "已生成: $DMG_PATH ($SIZE)"
echo
echo "分发提示：这是未做公证（notarization）的构建。对方首次打开时若被 Gatekeeper"
echo "拦截，可右键选择「打开」，或执行："
echo "  xattr -d com.apple.quarantine \"/Applications/$APP_NAME.app\""
