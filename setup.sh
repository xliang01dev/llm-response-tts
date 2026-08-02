#!/usr/bin/env bash
# One-shot setup: installs rust via Homebrew, installs the host-side binaries via `cargo
# install` (so they land on PATH in ~/.cargo/bin, not tied to this checkout - see README's
# Setup step 3), builds the Docker stack, and starts it. Safe to re-run after a git pull to
# rebuild everything - it won't touch an existing docker/.env or overwrite it.
set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
cd "$script_dir"

echo "==> Checking prerequisites"

if ! command -v docker >/dev/null 2>&1; then
  echo "docker not found. Install Docker Desktop first: https://www.docker.com/products/docker-desktop/" >&2
  exit 1
fi

if ! command -v brew >/dev/null 2>&1; then
  echo "Homebrew not found. Install it first: https://brew.sh" >&2
  exit 1
fi

if ! command -v rustc >/dev/null 2>&1 || ! command -v cargo >/dev/null 2>&1; then
  echo "==> Installing rust via Homebrew"
  brew install rust
else
  echo "==> rust already installed, skipping"
fi

echo "==> Wiring up the pre-commit hook (.githooks/pre-commit - runs scripts/test.sh)"
git config core.hooksPath .githooks

echo "==> Setting up docker/.env"
if [ ! -f docker/.env ]; then
  echo "LLM_RESPONSE_TTS_BEARER_TOKEN=$(openssl rand -hex 32)" > docker/.env
  echo "    created docker/.env with a fresh bearer token"
else
  echo "    docker/.env already exists, leaving it as-is"
fi

echo "==> Installing host binaries"
# Both installed via `cargo install`, which always lands in ~/.cargo/bin - a fixed, global
# location, so the Claude Code hook (see step 4) can reference them by name alone rather
# than a path tied to this specific checkout. This matters even more for player: it links
# cpal (CoreAudio) for audio output, and if this repo lives on a non-boot volume (e.g. an
# external or secondary drive, as /Volumes/... paths do on macOS), the OS kills
# CoreAudio-linked binaries executed from it with SIGKILL (Code Signature Invalid) -
# ~/.cargo/bin is on the boot volume, so this works regardless of where the repo lives.
cargo install --path host/tools --force
cargo install --path host/player --force

echo "==> Checking that ~/.cargo/bin is on PATH"
# Homebrew's rust formula (used above), unlike rustup, doesn't add ~/.cargo/bin to PATH -
# and the hook (step 4) references the installed binaries by name alone, so this is required
# for it to actually find them.
if [[ ":$PATH:" == *":$HOME/.cargo/bin:"* ]]; then
  echo "    already on PATH, skipping"
else
  case "$SHELL" in
    */zsh) shell_rc="$HOME/.zshrc" ;;
    */bash) shell_rc="$HOME/.bash_profile" ;;
    *) shell_rc="$HOME/.profile" ;;
  esac
  if ! grep -q '\.cargo/bin' "$shell_rc" 2>/dev/null; then
    echo 'export PATH="$HOME/.cargo/bin:$PATH"' >> "$shell_rc"
  fi
  echo "    added ~/.cargo/bin to PATH in $shell_rc - open a new terminal (or run"
  echo "    'source $shell_rc') before opening Claude Code, so the hook can find it"
fi

echo "==> Building and starting the Docker stack"
docker compose up -d --build

echo "==> Verifying the stack responds"
token=$(grep LLM_RESPONSE_TTS_BEARER_TOKEN docker/.env | cut -d= -f2-)
enqueue() {
  curl -s -o /dev/null -w '%{http_code}' -X POST -H "Authorization: Bearer $token" \
    -H "Content-Type: application/json" \
    -d '{"text":"setup verification","session":"setup-verify","output_dir":"/tmp/llm-response-tts/output/setup-verify"}' \
    http://127.0.0.1:3000/ 2>/dev/null || echo "000"
}
sleep 2
code=$(enqueue)
if [ "$code" != "202" ]; then
  # nginx can end up holding a stale connection to an old container IP if it was already
  # running from a previous setup.sh run while kokoros/ingress just got rebuilt above -
  # recreating it re-resolves the upstream and clears this up.
  echo "    stack didn't respond as expected (http $code) - recreating nginx and retrying"
  docker compose up -d --force-recreate nginx
  sleep 2
  code=$(enqueue)
  if [ "$code" != "202" ]; then
    echo "    still not responding (http $code) - check 'docker compose logs' for details" >&2
    exit 1
  fi
fi
echo "    stack is responding correctly"
"$HOME/.cargo/bin/llm-response-tts-clear-all-speech" >/dev/null 2>&1 || true

echo
echo "==> Done. Open Claude Code in this directory and talk to it normally - responses"
echo "    should play back as audio. First run will prompt you to trust this project's"
echo "    .claude/settings.json since hooks execute shell commands - approve it."
