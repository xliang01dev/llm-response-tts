#!/usr/bin/env bash
# Dev build for llm-response-tts-player. CoreAudio-linked binaries get SIGKILLed by macOS when
# executed from a non-boot volume, so a freshly built player can't just be run in place if this
# repo lives on one (e.g. /Volumes/...). If we're already on the boot volume, running it in place
# is fine and no copy is needed; otherwise this copies it to /tmp/llm-response-tts so it can run.
# /tmp (unlike ~/tmp) is on the boot volume and gets auto-purged by macOS after a few days.
#
# This is separate from `cargo install --path host/player` (see setup.sh) so that iterating on
# player doesn't clobber the real install in ~/.cargo/bin, which may be backing an already-running
# player process. ingest checks /tmp before falling back to ~/.cargo/bin (see its comment).
set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
workspace_dir=$(cd "$script_dir/.." && pwd)

cargo build --release --manifest-path "$workspace_dir/Cargo.toml" -p llm-response-tts-player

boot_dev=$(stat -f '%d' /)
here_dev=$(stat -f '%d' "$script_dir")

if [ "$boot_dev" = "$here_dev" ]; then
  echo "==> Already on the boot volume, run $workspace_dir/target/release/llm-response-tts-player directly"
else
  dest="/tmp/llm-response-tts"
  mkdir -p "$dest"
  cp "$workspace_dir/target/release/llm-response-tts-player" "$dest/llm-response-tts-player"
  echo "==> Not on the boot volume - copied to $dest/llm-response-tts-player"
fi
