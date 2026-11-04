#!/usr/bin/env bash
# Check "static": formatting and lints with warnings denied.
set -Eeuo pipefail
cd "$(dirname -- "${BASH_SOURCE[0]}")/../.."
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
# Dependency policy (deny.toml): licenses, banned crates and sources; advisories need the network and run in CI.
cargo deny --locked check licenses bans sources
# AC-062-05: SQL writes only in the Persistence module; under run_check its TEST-062-05 result goes to
# test-results.jsonl (CR-003 per-test evidence).
if [[ -n "${HOH_EVIDENCE_DIR:-}" ]]; then
  python3 scripts/product/persistence_scan.py --out "$HOH_EVIDENCE_DIR/test-results.jsonl"
else
  python3 scripts/product/persistence_scan.py
fi
