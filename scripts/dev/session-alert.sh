#!/usr/bin/env bash
# Claude Code StopFailure / SessionEnd hook: write down why the main development session's turn ended on an API error
# (StopFailure) or the session ended (SessionEnd), with a recovery point (the running task, branch and commit, the
# newest evidence, background work still running, the next step from PROGRESS.md), in .hoh-dev/session-alerts.jsonl
# (git-ignored); scripts/dev/session_alert.py does the work. The next session start shows what is new there
# (scripts/dev/session-context.sh). A normal end of a turn (Stop) has no hook: nothing is recorded. Claude Code ignores
# StopFailure output except a terminal sequence, so a StopFailure also asks for a desktop notification (OSC 9) and
# prints nothing else. Sessions tools/agent_task.py starts for delegated work (HOH_DELEGATED_TASK, or a
# HOH_BOOTSTRAP_PARENT that is not the top-level session's HOH_TOP_LEVEL_SESSION) are left alone (reviewers stay
# read-only); HOH_BOOTSTRAP_PARENT alone does not make a session delegated (SAR-01, SAR-08). Never fails the session. This records and reminds; it does not retry or resume anything
# (Claude Code itself retries transient API errors).
set -u
# Delegated: marked so, or started inside another session (its parent is not the top-level session, SAR-08).
[[ -z "${HOH_DELEGATED_TASK:-}" ]] || exit 0
[[ -z "${HOH_BOOTSTRAP_PARENT:-}" || "${HOH_BOOTSTRAP_PARENT}" == "${HOH_TOP_LEVEL_SESSION:-}" ]] || exit 0
root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd -P)" || exit 0
cd "$root" || exit 0
# The payload stays on stdin: python reads it there, so its size is not limited by the environment (SAR-02).
python3 -I scripts/dev/session_alert.py 2>/dev/null || true
exit 0
