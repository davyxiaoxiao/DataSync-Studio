#!/usr/bin/env bash
#
# Regenerate the application icon set from the master artwork.
#
#   ./scripts/build-icon.sh
#
# `assets/icon-master.png` is committed because it is the output of a manual
# step, not something reproducible from source: the artwork is generated, then
# its white background is cut out and a macOS margin is added. Keeping the
# finished master means the icon set can be rebuilt without redoing that.
#
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

MASTER="assets/icon-master.png"

if [[ ! -f "$MASTER" ]]; then
  echo "未找到 $MASTER" >&2
  exit 1
fi

# A square, transparent-cornered source is required: macOS does not mask app
# icons itself, so an opaque white background would show up as a white square
# in the Dock.
python3 - "$MASTER" <<'PY'
import sys
from PIL import Image

path = sys.argv[1]
image = Image.open(path).convert("RGBA")
if image.width != image.height:
    raise SystemExit("图标源必须是正方形")
if image.size != (1024, 1024):
    image = image.resize((1024, 1024), Image.LANCZOS)
corner = image.getpixel((0, 0))[3]
if corner != 0:
    raise SystemExit(f"图标源四角必须透明，当前 alpha={corner}")
image.save(path)
print(f"图标源检查通过: {image.size} {image.mode}")
PY

pnpm tauri icon "$MASTER"
echo "已生成图标集: src-tauri/icons/"
