#!/usr/bin/env bash
# Check "build": release build of everything that exists so far.
set -Eeuo pipefail
cd "$(dirname -- "${BASH_SOURCE[0]}")/../.."
# shellcheck source=scripts/product/build-commit.sh
source scripts/product/build-commit.sh
cargo build --workspace --release --locked
if [[ -f web/package.json ]]; then
  npm ci --prefix web
  npm run build --prefix web
else
  # M6-T02 (Web GUI) must turn a missing web/ into a failure once the Web GUI is in scope.
  echo 'build: web/ not present yet (Web GUI arrives in M6); Rust workspace built.' >&2
fi
# Release bundle for the release gate (CR-003): only under tools/run_check.py.
if [[ -n "${HOH_EVIDENCE_DIR:-}" ]]; then
  bash scripts/product/bundle.sh "$HOH_EVIDENCE_DIR/hoh-release.tar.gz"
  echo "build: release bundle $(sha256sum "$HOH_EVIDENCE_DIR/hoh-release.tar.gz" | cut -c1-16)…" >&2
fi
