#!/usr/bin/env bash
# Checks "unit" and "integration": run one class of Rust tests and fail if none passed, so an empty
# or all-ignored suite can never count as evidence. unit = library/binary unit tests plus doctests;
# integration = tests/ targets.
set -Eeuo pipefail
cd "$(dirname -- "${BASH_SOURCE[0]}")/../.."
case "${1:-}" in
  unit) selector=(--lib --bins);;
  integration) selector=(--test '*');;
  *) echo 'usage: cargo-tests.sh unit|integration' >&2; exit 64;;
esac
log="$(mktemp)"; trap 'rm -f "$log"' EXIT
cargo test --workspace --locked "${selector[@]}" 2>&1 | tee "$log"
if [[ "$1" == unit ]]; then cargo test --workspace --locked --doc 2>&1 | tee -a "$log"; fi
# Count executed tests from the summaries; ignored and filtered tests do not count.
passed="$(grep -Eo 'test result: [a-z]+\. [0-9]+ passed' "$log" | awk '{s+=$4} END {print s+0}')"
if [[ "$passed" -eq 0 ]]; then
  echo "NO_TESTS: the $1 check passed 0 executed tests; an empty or all-ignored suite is not evidence." >&2
  exit 3
fi
echo "$1: $passed tests passed." >&2
