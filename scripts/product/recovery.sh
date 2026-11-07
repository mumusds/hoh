#!/usr/bin/env bash
# Check "recovery" (mode product): kill -9 matrices and the reconcile at a host's start, crates/hoh-app/tests/
# recovery.rs (REQ-036, REQ-041; ADR-019): each host is a process of its own built in the test target, its faults
# made by test hooks (CR-006). Under tools/run_check.py the per-test results are written to
# $HOH_EVIDENCE_DIR/test-results.jsonl, also when tests fail. Fails if no test passed.
set -Eeuo pipefail
cd "$(dirname -- "${BASH_SOURCE[0]}")/../.."
log="$(mktemp)"; trap 'rm -f "$log"' EXIT
rc=0
cargo test -p hoh-app --locked --test recovery 2>&1 | tee "$log" || rc=$?
if [[ -n "${HOH_EVIDENCE_DIR:-}" ]]; then
  python3 scripts/product/test_results.py --log "$log" --out "$HOH_EVIDENCE_DIR/test-results.jsonl"
fi
[[ "$rc" -eq 0 ]] || exit "$rc"
passed="$(grep -Eo 'test result: [a-z]+\. [0-9]+ passed' "$log" | awk '{s+=$4} END {print s+0}')"
if [[ "$passed" -eq 0 ]]; then
  echo "NO_TESTS: the recovery check passed 0 executed tests; an empty or all-ignored suite is not evidence." >&2
  exit 3
fi
echo "recovery: $passed tests passed." >&2
