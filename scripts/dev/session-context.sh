#!/usr/bin/env bash
# Claude Code SessionStart hook (startup|resume|compact|clear): print the facts a resumed or compacted
# main development session needs first. Plain stdout becomes model context. Delegated sessions started by
# tools/agent_task.py carry HOH_BOOTSTRAP_PARENT and get nothing, so reviewers/QA stay independent of the
# author's notes. Read-only; never fails the session (exit 0 on any problem).
set -u
[[ -z "${HOH_BOOTSTRAP_PARENT:-}" ]] || exit 0
root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd -P)" || exit 0
cd "$root" || exit 0
echo '[hoh-session] Main development session context (scripts/dev/session-context.sh).'
echo "[hoh-session] git: branch $(git branch --show-current 2>/dev/null || echo '?') at $(git log -1 --format='%h %s' 2>/dev/null | cut -c1-100)"
dirty="$(git status --porcelain 2>/dev/null | wc -l | tr -d ' ')"
[[ "$dirty" == 0 ]] || echo "[hoh-session] git: $dirty uncommitted path(s) -- inspect before continuing; never discard them blindly"
if [[ -f .hoh-dev/task-state.json ]]; then
  python3 - <<'PY' 2>/dev/null || true
import json
d=json.load(open('.hoh-dev/task-state.json'))
running=[f"{t} @{v.get('owner','?')}" for t,v in d.get('tasks',{}).items() if v.get('status')=='running']
blocked=[t for t,v in d.get('tasks',{}).items() if v.get('status')=='blocked']
print('[hoh-session] taskboard: running '+(', '.join(running) or 'none')+'; blocked '+(', '.join(blocked) or 'none'))
PY
fi
if [[ -f .hoh-dev/PROGRESS.md ]]; then
  echo '[hoh-session] .hoh-dev/PROGRESS.md "Next" section:'
  awk '/^## Next/{on=1;next} /^## /{if(on)exit} on' .hoh-dev/PROGRESS.md | head -15 | sed 's/^/[hoh-session]   /'
fi
echo '[hoh-session] Rules: read CLAUDE.md and goal.md; verify facts from files, git and evidence, not from memory or earlier "done" claims; update PROGRESS.md and the taskboard after each step.'
exit 0
