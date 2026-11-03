#!/usr/bin/env bash
# Checks "unit" and "integration": run one class of Rust tests and fail if none ran, so an empty
# suite can never count as evidence. unit = library and binary unit tests; integration = tests/ targets.
set -Eeuo pipefail
cd "$(dirname -- "${BASH_SOURCE[0]}")/../.."
case "${1:-}" in
  unit) selector=(--lib --bins);;
  integration) selector=(--test '*');;
  *) echo 'usage: cargo-tests.sh unit|integration' >&2; exit 64;;
esac
log="$(mktemp)"; trap 'rm -f "$log"' EXIT
cargo test --workspace --locked "${selector[@]}" 2>&1 | tee "$log"
total="$(grep -Eo '^running [0-9]+ tests?' "$log" | awk '{s+=$2} END {print s+0}')"
if [[ "$total" -eq 0 ]]; then
  echo "NO_TESTS: the $1 check ran 0 tests; an empty suite is not evidence." >&2
  exit 3
fi
echo "$1: $total tests ran." >&2
