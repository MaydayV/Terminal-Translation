#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT_DIR"

echo "[smoke] running core tests"
cargo test -p tetr-core

echo "[smoke] building tetr-cli"
cargo build -p tetr-cli

echo "[smoke] UI dependencies"
pnpm --dir apps/tetr-ui install

echo "[smoke] UI build"
pnpm --dir apps/tetr-ui build

echo

echo "Manual run:"
echo "  TETR_PROVIDER=mock cargo run -p tetr-cli -- --no-ui"
