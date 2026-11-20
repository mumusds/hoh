#!/usr/bin/env bash
# Check "build": release build of everything that exists so far.
set -Eeuo pipefail
cd "$(dirname -- "${BASH_SOURCE[0]}")/../.."
# shellcheck source=scripts/product/build-commit.sh
source scripts/product/build-commit.sh
executable="$(bash scripts/product/release-exe.sh)"
# The debug-only test seams (docs/adr/ADR-031.md, 设计修正 2, verification: hold points that wait on marker files;
# ADR-014 decision 8: the TUI's panic and restore gates) are not in the release binary: none of their names is in it.
for seam in HOH_DEBUG_HOLD_DIR HOH_DEBUG_TUI_PANIC HOH_DEBUG_TUI_RESTORE_GATE; do
  if grep -aq "$seam" "$executable"; then
    echo "build: the release binary holds the debug-only seam $seam" >&2
    exit 1
  fi
done
if [[ -f web/package.json ]]; then
  npm ci --prefix web
  npm run build --prefix web
else
  # M6-T02 (Web GUI) must turn a missing web/ into a failure once the Web GUI is in scope.
  echo 'build: web/ not present yet (Web GUI arrives in M6); Rust workspace built.' >&2
fi
# Release bundle for the release gate (CR-003): only under tools/run_check.py.
if [[ -n "${HOH_EVIDENCE_DIR:-}" ]]; then
  bash scripts/product/bundle.sh "$HOH_EVIDENCE_DIR/hoh-release.tar.gz" "$executable"
  echo "build: release bundle $(sha256sum "$HOH_EVIDENCE_DIR/hoh-release.tar.gz" | cut -c1-16)…" >&2
fi
