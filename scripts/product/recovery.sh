#!/usr/bin/env bash
# Check "recovery" (mode product): kill -9 matrices and the reconcile at a host's start, crates/hoh-app/tests/
# recovery.rs (REQ-036, REQ-041; ADR-019): each host is a process of its own built in the test target, its faults
# made by test hooks (CR-006). Then recovery.rs again, with control.rs, inside a delegated scope of the user's systemd
# manager (`systemd-run --user --scope -p Delegate=yes`), where a host in the test process tracks its attempts by
# cgroups of their own: the production path (CR-011, docs/adr/ADR-021.md decision 8d), which the first run, in /proc
# mode, never takes (F2). A test of one mode stays in it either way (M2T06R-452). The second run needs the user's
# systemd manager, as recovery.rs's test_005_04 does: without it the check fails. Under tools/run_check.py the
# per-test results of both runs are written to $HOH_EVIDENCE_DIR/test-results.jsonl (a test counts in each), also
# when tests fail. Fails if either run fails, or if no test passed.
set -Eeuo pipefail
cd "$(dirname -- "${BASH_SOURCE[0]}")/../.."
log="$(mktemp)"; trap 'rm -f "$log"' EXIT
rc=0
# The parsed log is cargo's stdout only: its stderr (build output, and diagnostics of the processes the tests start)
# stays on this script's stderr, where it cannot break a libtest result line (M3QA-018, DC-QA018).
cargo test -p hoh-app --locked --test recovery | tee "$log" || rc=$?
systemd-run --user --scope -p Delegate=yes --quiet --collect -- \
  cargo test -p hoh-app --locked --test recovery --test control | tee -a "$log" || rc=$?
if [[ -n "${HOH_EVIDENCE_DIR:-}" ]]; then
  python3 scripts/product/test_results.py --log "$log" --out "$HOH_EVIDENCE_DIR/test-results.jsonl"
fi
[[ "$rc" -eq 0 ]] || exit "$rc"
python3 scripts/product/test_results.py --log "$log" --check-stdout
passed="$(grep -Eo 'test result: [a-z]+\. [0-9]+ passed' "$log" | awk '{s+=$4} END {print s+0}')"
if [[ "$passed" -eq 0 ]]; then
  echo "NO_TESTS: the recovery check passed 0 executed tests; an empty or all-ignored suite is not evidence." >&2
  exit 3
fi
echo "recovery: $passed tests passed, in /proc mode and in a delegated scope." >&2
