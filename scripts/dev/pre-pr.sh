#!/usr/bin/env bash
# Run locally what CI's product, bundle and deployment jobs run (.github/workflows/bundle-ci.yml), before a PR.
# The privileged fault matrix (docs/09 item 7: db_faults --ignored on real tmpfs/read-only mounts, sudo -n) only ran in
# CI and let three changes reach a PR red (F7-51); this runs it too, and like CI it fails, never skips, without sudo.
# Signals are reset to their defaults for the test runs (a backgrounded shell ignores SIGINT; M2QA-001).
# Usage: bash scripts/dev/pre-pr.sh [--no-deployment]   (set CARGO_TARGET_DIR to your worktree's target first)
# Test runs take one machine-wide slot (flock on HOH_TEST_SLOT, default /home/ai/dev/.hoh-test-slot.lock); builds run
# outside it. Several sessions running the workspace suite at once drove the load to 50 and exhausted ephemeral ports,
# which failed unrelated timing and bind tests (FLK-05 evidence, 2026-11-23); queueing the runs keeps them comparable to CI.
set -euo pipefail
cd "$(dirname "$0")/../.."
deployment=1
[ "${1:-}" = "--no-deployment" ] && deployment=0
run() {  # name, command...
  local name=$1; shift
  printf '== %s\n' "$name"
  local start=$SECONDS
  if python3 -c 'import os, signal, sys
signal.signal(signal.SIGINT, signal.SIG_DFL); signal.signal(signal.SIGQUIT, signal.SIG_DFL)
os.execvp(sys.argv[1], sys.argv[1:])' "$@"; then
    printf '== %s: ok (%ss)\n' "$name" "$((SECONDS - start))"
  else
    local code=$?
    printf '== %s: FAILED exit %s (%ss)\n' "$name" "$code" "$((SECONDS - start))"
    exit "$code"
  fi
}
slot=${HOH_TEST_SLOT:-/home/ai/dev/.hoh-test-slot.lock}
locked() {  # name, command...: run under the machine-wide test slot
  local name=$1; shift
  printf '== %s: waiting for the test slot %s\n' "$name" "$slot"
  run "$name" flock "$slot" "$@"
}
run bundle bash scripts/check.sh --bundle
run static bash scripts/product/static.sh
run build cargo test --workspace --locked --no-run
locked workspace cargo test --workspace --locked
sudo -n true || { echo '== fault matrix: FAILED: sudo -n is not available (CI fails here too, it never skips)'; exit 1; }
locked fault-matrix-db cargo test -p hoh-app --locked --test db_faults -- --ignored
locked fault-matrix-selftest cargo test -p hoh-core --locked --lib -- --ignored --exact \
  persistence::selftest::tests::a_file_system_that_fills_before_the_probes_commit_is_db_full
run deny cargo deny --locked check
if [ "$deployment" = 1 ]; then
  locked deployment bash scripts/product/deployment.sh
fi
echo '== pre-pr: all ok'
