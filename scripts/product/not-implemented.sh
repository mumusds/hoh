#!/usr/bin/env bash
# Placeholder for checks whose product capability does not exist yet. It must fail (exit 3)
# so run_check and release_gate can never count it as evidence. Replace the plan entry with
# the real command in the task that implements the check.
set -Eeuo pipefail
check="${1:?check id}"; milestone="${2:?milestone}"
echo "NOT_IMPLEMENTED: check '${check}' has no product implementation yet (planned in ${milestone})." >&2
exit 3
