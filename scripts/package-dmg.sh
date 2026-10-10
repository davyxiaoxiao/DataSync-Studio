#!/usr/bin/env bash
#
# Build a distributable DMG from the packaged .app, ad-hoc signed.
#
# `pnpm tauri build --bundles dmg` drives Tauri's own `bundle_dmg.sh`, which
# arranges the window with Finder AppleScript and therefore fails in any
# session without a controllable Finder (remote shells, CI, sandboxes).
# `hdiutil` needs no Finder and produces a perfectly ordinary installer, so the
# DMG is created here instead.
#
#   pnpm tauri build          # produces the .app
#   ./scripts/package-dmg.sh  # signs it and wraps it into a .dmg
#
# About the signing: on Apple silicon every executable needs at least an ad-hoc
# signature to run at all, so this is not optional. Ad-hoc is *not* a
# substitute for a Developer ID signature plus notarization — it does not stop
# Gatekeeper from warning about a downloaded copy — but without it the app is
# reported as damaged rather than merely unidentified.
#
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

APP_NAME="DataSync Studio"
APP_PATH="$ROOT/src-tauri/target/release/bundle/macos/$APP_NAME.app"
DMG_DIR="$ROOT/src-tauri/target/release/bundle/dmg"
# A copy that survives a cleanup of the (large, ignored) build directory.
# Deliberately not `dist/`: that is Vite's output directory, and its next build
# empties it — which would delete the deliverable.
DIST_DIR="$ROOT/release"

if [[ ! -d "$APP_PATH" ]]; then
  echo "未找到 $APP_PATH" >&2
  echo "请先运行: pnpm tauri build" >&2
  exit 1
fi

# ---------------------------------------------------------------------------
# Sign, and prove that it worked
# ---------------------------------------------------------------------------
#
# Signing has to happen before the DMG is built: the volume holds a *copy*, so
# signing the original afterwards leaves the shipped app unsigned. That is
# exactly the bug this ordering fixes.
#
# `--deep` is not used. Apple discourages it because it signs nested code with
# the wrong flags; each Mach-O binary is signed on its own instead, innermost
# first, and the bundle is signed last so its seal covers them.

sign_nested() {
  # Frameworks and helper executables, if a future build ever adds any.
  local binary
  while IFS= read -r -d '' binary; do
    if file "$binary" | grep -q "Mach-O"; then
      codesign --force --sign - --timestamp=none "$binary"
    fi
  done < <(find "$APP_PATH/Contents" -type f \
    \( -path "*/Frameworks/*" -o -path "*/MacOS/*" -o -name "*.dylib" \) \
    ! -path "*/MacOS/$APP_NAME" -print0 2>/dev/null)
}

echo "正在做 ad-hoc 签名 …"
sign_nested
codesign --force --sign - --timestamp=none "$APP_PATH"

# An unchecked signature is not a signature: a broken seal still reports a
# "Signature=adhoc" line, so the bundle is verified explicitly.
if ! codesign --verify --strict --verbose=2 "$APP_PATH" 2>&1 | sed 's/^/  /'; then
  echo "签名校验失败" >&2
  exit 1
fi
if ! codesign --verify --deep --strict "$APP_PATH" >/dev/null 2>&1; then
  echo "深度校验失败（嵌套代码未正确签名）" >&2
  exit 1
fi
echo "  签名有效"

# ---------------------------------------------------------------------------
# Package
# ---------------------------------------------------------------------------

VERSION="$(/usr/libexec/PlistBuddy -c 'Print CFBundleShortVersionString' \
  "$APP_PATH/Contents/Info.plist" 2>/dev/null || echo "0.0.0")"
ARCH="$(uname -m)"
DMG_PATH="$DMG_DIR/${APP_NAME}_${VERSION}_${ARCH}.dmg"

mkdir -p "$DMG_DIR" "$DIST_DIR"
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

# The image itself can carry a signature too, which lets `hdiutil verify` and
# Gatekeeper check it before anything is mounted.
codesign --force --sign - --timestamp=none "$DMG_PATH"
codesign --verify --strict "$DMG_PATH"

# The build directory is ignored and large enough to be cleaned out from under
# us; the deliverable is small, so a copy lives outside it.
DIST_PATH="$DIST_DIR/${APP_NAME}_${VERSION}_${ARCH}.dmg"
cp -f "$DMG_PATH" "$DIST_PATH"

SIZE="$(du -h "$DMG_PATH" | cut -f1)"
echo "已生成: $DMG_PATH ($SIZE)"
echo "已复制: $DIST_PATH"
echo
echo "签名：ad-hoc（无 Developer ID、无公证）。这在 Apple 芯片上是必须的——"
echo "没有它系统会直接报「已损坏」而不是「无法验证开发者」。"
echo
echo "分发提示：对方首次打开仍可能被 Gatekeeper 询问，右键选择「打开」即可，"
echo "或执行："
echo "  xattr -d com.apple.quarantine \"/Applications/$APP_NAME.app\""
echo
echo "要彻底消除这个提示，需要 Developer ID 证书签名后再公证（notarize）："
echo "  codesign --sign \"Developer ID Application: …\" --options runtime …"
echo "  xcrun notarytool submit \"$DMG_PATH\" --keychain-profile … --wait"
echo "  xcrun stapler staple \"$DMG_PATH\""
