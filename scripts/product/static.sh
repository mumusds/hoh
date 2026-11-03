#!/usr/bin/env bash
# Check "static": formatting and lints with warnings denied.
set -Eeuo pipefail
cd "$(dirname -- "${BASH_SOURCE[0]}")/../.."
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
# Dependency policy (deny.toml): licenses, banned crates and sources; advisories need the network and run in CI.
cargo deny --locked check licenses bans sources
# Per-test evidence (CR-003): static AC scans (e.g. AC-062-05) append to test-results.jsonl from M2-T01 on.
if [[ -n "${HOH_EVIDENCE_DIR:-}" ]]; then : >> "$HOH_EVIDENCE_DIR/test-results.jsonl"; fi
