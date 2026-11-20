#!/usr/bin/env bash
# The debug-only test seams (docs/adr/ADR-031.md, 设计修正 2, verification: hold points that wait on marker files;
# ADR-014 decision 8: the TUI's panic and restore gates) are not in the release binary named by $1: none of their names
# is in it. A binary that is not there, or a grep that fails, is a failure, never a pass without reading (CLT-1256).
set -Eeuo pipefail
bin="${1:?usage: release_seams.sh <release binary>}"
if [[ ! -f "$bin" || ! -r "$bin" ]]; then
  echo "release-seams: no release binary to read at $bin" >&2
  exit 1
fi
for seam in HOH_DEBUG_HOLD_DIR HOH_DEBUG_TUI_PANIC HOH_DEBUG_TUI_RESTORE_GATE; do
  rc=0
  grep -aq "$seam" "$bin" || rc=$?
  case "$rc" in
    0) echo "release-seams: the release binary holds the debug-only seam $seam" >&2; exit 1 ;;
    1) ;;
    *) echo "release-seams: grep could not read $bin (exit $rc)" >&2; exit 1 ;;
  esac
done
echo "release-seams: $bin holds none of the debug-only seams" >&2
