#!/bin/sh
# Builds and runs every test in both Cargo workspaces (host/ and services/ are separate
# workspaces - see their respective Cargo.toml files). Used by .githooks/pre-commit and can
# be run standalone: ./scripts/test.sh
set -e

repo_root=$(cd "$(dirname "$0")/.." && pwd)

for manifest in "$repo_root/host/Cargo.toml" "$repo_root/services/Cargo.toml"; do
  echo "==> Building $(dirname "$manifest")"
  cargo build --release --manifest-path "$manifest"
  echo "==> Testing $(dirname "$manifest")"
  cargo test --manifest-path "$manifest"
done
