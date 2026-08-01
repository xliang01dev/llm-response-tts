#!/usr/bin/env bash
# Dev build for kokoros-player. CoreAudio-linked binaries get SIGKILLed by macOS when executed
# from a non-boot volume, so a freshly built player can't just be run in place if this repo lives
# on one (e.g. /Volumes/...). If we're already on the boot volume, running it in place is
# fine and no copy is needed; otherwise this copies it to /tmp/kokoros-rust so it can run.
# /tmp (unlike ~/tmp) is on the boot volume and gets auto-purged by macOS after a few days.
set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
workspace_dir=$(cd "$script_dir/.." && pwd)

cargo build --release --manifest-path "$workspace_dir/Cargo.toml" -p kokoros-player

boot_dev=$(stat -f '%d' /)
here_dev=$(stat -f '%d' "$script_dir")

if [ "$boot_dev" = "$here_dev" ]; then
  echo "==> Already on the boot volume, run $workspace_dir/target/release/kokoros-player directly"
else
  dest="/tmp/kokoros-rust"
  mkdir -p "$dest"
  cp "$workspace_dir/target/release/kokoros-player" "$dest/kokoros-player"
  echo "==> Not on the boot volume - copied to $dest/kokoros-player"
fi
