#!/usr/bin/env bash
# Check "traceability" (docs/adr/ADR-027.md decision 10): judge every execution record of the frozen candidate by
# principle one and each TEST of the profile, after running this check's own self-tests. Run it through
# tools/run_check.py, which sets HOH_EVIDENCE_DIR and HOH_CANDIDATE_FILE. Exit 0 iff every TEST is satisfied and no
# record of the candidate is invalid.
set -Eeuo pipefail
cd "$(dirname -- "${BASH_SOURCE[0]}")/../.."
exec python3 scripts/product/traceability.py "${1:?usage: traceability.sh <profile>}"
