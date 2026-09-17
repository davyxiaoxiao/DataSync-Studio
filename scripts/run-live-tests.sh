#!/usr/bin/env bash
# Run the live database integration tests.
#
#   cp .env.live.example .env.live   # then fill in your credentials
#   ./scripts/run-live-tests.sh
#
# Only `cargo test --lib` is exercised; the pure unit tests always run and the
# live tests run for whichever engine is configured.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

if [[ -f .env.live ]]; then
  # shellcheck disable=SC1091
  set -a && source .env.live && set +a
else
  echo "提示：未找到 .env.live，live 测试将自动跳过。" >&2
  echo "      cp .env.live.example .env.live 并填入连接信息后可运行真实库测试。" >&2
fi

# Keep cargo/pnpm caches inside the repository when the global ones are not writable.
if ! cargo metadata --no-deps --format-version 1 >/dev/null 2>&1; then
  # shellcheck disable=SC1091
  source ./dev-env.sh
fi

cd src-tauri
cargo test --lib -- --nocapture "$@"
