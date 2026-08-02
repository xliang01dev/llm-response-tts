#!/usr/bin/env bash
# Reverses setup.sh: stops and removes the Docker stack, uninstalls the host binaries via
# `cargo uninstall`, and unwires the pre-commit hook. Leaves docker/.env (has your bearer
# token), any PATH edits to your shell rc, and runtime state under /tmp alone - see the
# final message for how to remove those too if you want a completely clean slate.
set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
cd "$script_dir"

echo "==> Stopping and removing the Docker stack"
if command -v docker >/dev/null 2>&1; then
  docker compose down
else
  echo "    docker not found, skipping"
fi

echo "==> Uninstalling host binaries"
if command -v cargo >/dev/null 2>&1; then
  cargo uninstall llm-response-tts-tools 2>/dev/null || echo "    llm-response-tts-tools not installed, skipping"
  cargo uninstall llm-response-tts-player 2>/dev/null || echo "    llm-response-tts-player not installed, skipping"
else
  echo "    cargo not found, skipping"
fi

echo "==> Unwiring the pre-commit hook"
if [ "$(git config --get core.hooksPath 2>/dev/null || true)" = ".githooks" ]; then
  git config --unset core.hooksPath
else
  echo "    core.hooksPath isn't set to .githooks, skipping"
fi

echo
echo "==> Done. Left in place, remove manually if you want a completely clean slate:"
echo "    - docker/.env (your bearer token)"
echo "    - runtime state under /tmp/llm-response-tts"
echo "    - the ~/.cargo/bin PATH export setup.sh may have added to your shell rc"
