#!/usr/bin/env bash
# Checks "independent-qa" and "independent-review" (docs/adr/ADR-027.md decision 11): fresh native QA or review
# sessions through tools/agent_task.py on the frozen candidate, with principle one's judgement of its records. Run it
# through tools/run_check.py, which sets HOH_EVIDENCE_DIR and HOH_CANDIDATE_FILE. Exit 0 = report PASS without
# critical/high findings, 3 = every attempt blocked from outside (no CLI, authentication, quota, capacity), else 1.
set -Eeuo pipefail
cd "$(dirname -- "${BASH_SOURCE[0]}")/../.."
exec python3 scripts/product/independent.py "${1:?usage: independent.sh <qa|reviewer> <profile>}" \
  "${2:?usage: independent.sh <qa|reviewer> <profile>}"
