#!/usr/bin/env bash
# Checks "unit" and "integration": run one class of Rust tests and fail if none passed, so an empty
# or all-ignored suite can never count as evidence. unit = library/binary unit tests plus doctests;
# integration = tests/ targets. Under tools/run_check.py the per-test results are written to
# $HOH_EVIDENCE_DIR/test-results.jsonl (CR-003), also when tests fail.
set -Eeuo pipefail
cd "$(dirname -- "${BASH_SOURCE[0]}")/../.."
case "${1:-}" in
  unit) selector=(--lib --bins);;
  integration) selector=(--test '*');;
  *) echo 'usage: cargo-tests.sh unit|integration' >&2; exit 64;;
esac
log="$(mktemp)"; trap 'rm -f "$log"' EXIT
rc=0
# The parsed log is cargo's stdout only: its stderr (build output, and diagnostics of the processes the tests start)
# stays on this script's stderr, where it cannot break a libtest result line (M3QA-018, DC-QA018).
# --no-fail-fast: every test binary runs even after one failed, so the results after it are evidence too (QA-C-006);
# cargo still exits non-zero when any failed.
cargo test --workspace --locked --no-fail-fast "${selector[@]}" | tee "$log" || rc=$?
if [[ "$1" == unit ]]; then cargo test --workspace --locked --no-fail-fast --doc | tee -a "$log" || rc=$?; fi
if [[ -n "${HOH_EVIDENCE_DIR:-}" ]]; then
  python3 scripts/product/test_results.py --log "$log" --out "$HOH_EVIDENCE_DIR/test-results.jsonl"
fi
[[ "$rc" -eq 0 ]] || exit "$rc"
# Count executed tests from the summaries; ignored and filtered tests do not count.
passed="$(grep -Eo 'test result: [a-z]+\. [0-9]+ passed' "$log" | awk '{s+=$4} END {print s+0}')"
if [[ "$passed" -eq 0 ]]; then
  echo "NO_TESTS: the $1 check passed 0 executed tests; an empty or all-ignored suite is not evidence." >&2
  exit 3
fi
echo "$1: $passed tests passed." >&2
