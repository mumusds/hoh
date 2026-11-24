#!/usr/bin/env bash
# Run locally what CI's product, bundle and deployment jobs run (.github/workflows/bundle-ci.yml), before a PR.
# The privileged fault matrix (docs/09 item 7: db_faults --ignored on real tmpfs/read-only mounts, sudo -n) only ran in
# CI and let three changes reach a PR red (F7-51); this runs it too, and like CI it fails, never skips, without sudo.
# Signals are reset to their defaults for the test runs (a backgrounded shell ignores SIGINT; M2QA-001).
# Usage: bash scripts/dev/pre-pr.sh [--no-deployment]   (set CARGO_TARGET_DIR to your worktree's target first)
# Test runs take one machine-wide slot (flock on HOH_TEST_SLOT, default /home/ai/dev/.hoh-test-slot.lock); builds run
# outside it. Several sessions running the workspace suite at once drove the load to 50 and exhausted ephemeral ports,
# which failed unrelated timing and bind tests (FLK-05 evidence, 2026-11-23); queueing the runs keeps them comparable to CI.
# The bundle steps are CI's bundle job: shellcheck, the bootstrap dry run, then the bundle self-test with CI=1 as in CI, so
# tests/bundle/test_record.py fails rather than skips its scope tests without a systemd user manager (PREPR-01, PREPR-02).
# Prerequisite: a user manager for this user (`sudo loginctl enable-linger "$USER"`; scripts/dev/test-infra.sh).
# CI runs that job on Python 3.11 to 3.14; pre-pr runs it on this machine's python3 only (PREPR-03), so a construct
# another of those versions rejects can still turn CI red.
set -euo pipefail
cd "$(dirname "$0")/../.."
usage='Usage: bash scripts/dev/pre-pr.sh [--no-deployment]'
deployment=1
case "$#:${1:-}" in
  0:) ;;
  1:--no-deployment) deployment=0 ;;
  *) printf '%s\n' "$usage" >&2; exit 64 ;;
esac
run() {  # name, command...
  local name=$1; shift
  printf '== %s\n' "$name"
  local start=$SECONDS
  if python3 -c 'import os, signal, sys
signal.signal(signal.SIGINT, signal.SIG_DFL); signal.signal(signal.SIGQUIT, signal.SIG_DFL)
os.execvp(sys.argv[1], sys.argv[1:])' "$@" 9>&-; then
    printf '== %s: ok (%ss)\n' "$name" "$((SECONDS - start))"
  else
    local code=$?
    printf '== %s: FAILED exit %s (%ss)\n' "$name" "$code" "$((SECONDS - start))"
    exit "$code"
  fi
}
# The slot is held on this script's own descriptor 9, which no step inherits (9>&- above): it is released when the
# step ends, when pre-pr exits on a failure, or when pre-pr is killed, and no left-over descendant can keep it (PPS-02).
# pre-pr adds no signal handling: as before the slot, a pre-pr killed mid-step leaves that step's processes to their own
# signals (Ctrl+C reaches the whole foreground group); they no longer hold the slot. A pre-pr killed while queued dies at
# once; its waiting flock helper then takes and drops the slot without running anything.
slot=${HOH_TEST_SLOT:-/home/ai/dev/.hoh-test-slot.lock}
exec 9>"$slot"
locked() {  # name, command...: run under the machine-wide test slot
  local name=$1; shift
  if ! flock -n 9; then
    printf '== %s: waiting for the test slot %s\n' "$name" "$slot"
    flock 9
  fi
  run "$name" "$@"
  flock -u 9
}
run shellcheck shellcheck -S warning scripts/*.sh scripts/product/*.sh scripts/dev/*.sh
run bootstrap-dry-run bash scripts/bootstrap.sh --yolo --dry-run
run bundle env CI=1 bash scripts/check.sh --bundle
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
