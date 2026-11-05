#!/usr/bin/env bash
# Check "tui-pty" (mode product): the TUI in real pseudo-terminals, crates/hoh-app/tests/tui_pty.rs (ADR-014): the
# Workspace opening offline, the input history across sessions, and the terminal restored (stty -g, alternate screen,
# cursor) after a detach, SIGTERM, SIGHUP and a panic. Under tools/run_check.py the per-test results are written to
# $HOH_EVIDENCE_DIR/test-results.jsonl, also when tests fail; tests whose names carry no TEST id count but add no line
# (TEST-001-01/02 arrive with M2-T13). Fails if no test passed.
set -Eeuo pipefail
cd "$(dirname -- "${BASH_SOURCE[0]}")/../.."
log="$(mktemp)"; trap 'rm -f "$log"' EXIT
rc=0
cargo test -p hoh-app --locked --test tui_pty 2>&1 | tee "$log" || rc=$?
if [[ -n "${HOH_EVIDENCE_DIR:-}" ]]; then
  python3 scripts/product/test_results.py --log "$log" --out "$HOH_EVIDENCE_DIR/test-results.jsonl"
fi
[[ "$rc" -eq 0 ]] || exit "$rc"
passed="$(grep -Eo 'test result: [a-z]+\. [0-9]+ passed' "$log" | awk '{s+=$4} END {print s+0}')"
if [[ "$passed" -eq 0 ]]; then
  echo "NO_TESTS: the tui-pty check passed 0 executed tests; an empty or all-ignored suite is not evidence." >&2
  exit 3
fi
echo "tui-pty: $passed tests passed." >&2
