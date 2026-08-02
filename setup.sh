#!/usr/bin/env bash
# One-shot setup: installs rust via Homebrew, wires up the pre-commit test hook, installs the
# host-side binaries via `cargo install` (so they land on PATH in ~/.cargo/bin, not tied to
# this checkout - see README's Setup step 3), builds the Docker stack, and starts it. Safe to
# re-run after a git pull to rebuild everything - it won't touch an existing docker/.env or
# overwrite it.
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
    */fish) shell_rc="$HOME/.config/fish/config.fish" ;;
    *)
      echo "    unrecognized \$SHELL ($SHELL) - add ~/.cargo/bin to PATH manually"
      shell_rc=""
      ;;
  esac
  if [ -n "$shell_rc" ]; then
    if grep -q '\.cargo/bin' "$shell_rc" 2>/dev/null; then
      echo "    already in $shell_rc, just not in this shell's PATH yet - open a new"
      echo "    terminal (or run 'source $shell_rc') before opening Claude Code"
    else
      if [[ "$shell_rc" == *config.fish ]]; then
        mkdir -p "$(dirname "$shell_rc")"
        echo 'set -gx PATH $HOME/.cargo/bin $PATH' >> "$shell_rc"
      else
        echo 'export PATH="$HOME/.cargo/bin:$PATH"' >> "$shell_rc"
      fi
      echo "    added ~/.cargo/bin to PATH in $shell_rc - open a new terminal (or run"
      echo "    'source $shell_rc') before opening Claude Code, so the hook can find it"
    fi
  fi
fi

echo "==> Building and starting the Docker stack"
docker compose up -d --build

echo "==> Waiting for kokoros to finish loading its TTS models"
# kokoros is backend-only (no published port), so it can't be polled directly from the host -
# watch its own log line instead. Without this, a worker can grab a job and try to synthesize
# before kokoros has bound its port, which fails outright with no retry (no .wav ever
# appears for that job).
for _ in $(seq 1 60); do
  docker compose logs kokoros 2>/dev/null | grep -q "OpenAI-compatible HTTP server" && break
  sleep 1
done

echo "==> Waiting for the stack to accept requests"
# nginx itself can be up (and pass a container-Running check) well before ingress behind it
# has bound its port, since nginx resolves that upstream at startup regardless of whether the
# app inside is actually listening yet - so poll the real proxied path instead, until it
# stops 502ing.
token=$(grep LLM_RESPONSE_TTS_BEARER_TOKEN docker/.env | cut -d= -f2-)
code="000"
for _ in $(seq 1 30); do
  code=$(curl -s -o /dev/null -w '%{http_code}' -X POST -H "Authorization: Bearer $token" \
    -H "Content-Type: application/json" -d '{}' http://127.0.0.1:3000/ 2>/dev/null || echo "000")
  [ "$code" != "502" ] && [ "$code" != "000" ] && break
  sleep 1
done
if [ "$code" = "502" ] || [ "$code" = "000" ]; then
  echo "    stack still not reachable (http $code) after 30s - check 'docker compose logs'" >&2
  exit 1
fi

echo "==> Sending a test message - turn your speakers on"
sound_output_base="${LLM_RESPONSE_TTS_SOUND_OUTPUT:-/tmp/llm-response-tts/output}"
mkdir -p "$sound_output_base"
marker=$(mktemp)
echo '{"final":true,"message_id":"setup-test","delta":"You have successfully installed llm-response-tts"}' | "$HOME/.cargo/bin/llm-response-tts-ingest"

wav=""
for _ in $(seq 1 30); do
  wav=$(find "$sound_output_base" -name '*.wav' -newer "$marker" 2>/dev/null | head -n1)
  [ -n "$wav" ] && break
  sleep 0.5
done
rm -f "$marker"
if [ -z "$wav" ]; then
  echo "    no .wav appeared under $sound_output_base within 15s - check 'docker compose logs' for details" >&2
  exit 1
fi

for _ in $(seq 1 15); do
  [ ! -f "$wav" ] && break
  sleep 1
done
if [ -f "$wav" ]; then
  echo "    playback did not succeed - check that player is running" >&2
  exit 1
fi
echo "==> Done - see https://github.com/xliang01dev/llm-response-tts#how-to-hook-up-to-a-coding-agent to wire it into your coding agent"
