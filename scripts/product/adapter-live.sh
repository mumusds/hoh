#!/usr/bin/env bash
# Check "adapter-live" (mode live, TEST-025-04): the Claude adapter against the installed `claude` CLI, with model
# calls. Runs crates/hoh-agents/tests/claude_live.rs (an ignored test whose name carries no TEST id, so the integration
# check never reports TEST-025-04 as skipped) and turns its verdict into $HOH_EVIDENCE_DIR/test-results.jsonl
# and live-sessions.json (contracts/live-session.schema.json). Exit 0 = PASS, 3 = BLOCKED (claude missing, not logged
# in, or run as root; never reported as PASS), 1 = FAIL. The parent agent session's variables are removed first, as the
# protocol recorder does, so a nested run is a fresh native session.
set -Eeuo pipefail
cd "$(dirname -- "${BASH_SOURCE[0]}")/../.."
out="${HOH_EVIDENCE_DIR:-$(mktemp -d)}"
mkdir -p "$out"
unset AI_AGENT CLAUDECODE CLAUDE_PID CLAUDE_EFFORT CLAUDE_CODE_SESSION_ID CLAUDE_CODE_CHILD_SESSION \
  CLAUDE_CODE_MESSAGING_SOCKET CLAUDE_CODE_MESSAGING_TOKEN CLAUDE_CODE_SESSION_ATTENDED CLAUDE_CODE_ENTRYPOINT \
  CLAUDE_CODE_EXECPATH CODEX_SANDBOX CODEX_SANDBOX_NETWORK_DISABLED
verdict="$out/claude-live-verdict.json"
rm -f "$verdict"
claude_bin="$(command -v claude || true)"
if [[ -z "$claude_bin" ]]; then
  printf '{"status": "BLOCKED", "detail": "claude is not installed", "sessions": []}\n' > "$verdict"
elif [[ "$(id -u)" == 0 ]]; then
  # AC-025-04 runs as a non-root user: the adapter never bypasses permissions as root.
  printf '{"status": "BLOCKED", "detail": "running as root", "sessions": []}\n' > "$verdict"
else
  HOH_LIVE_OUT="$out" HOH_LIVE_CLAUDE="$claude_bin" cargo test -p hoh-agents --locked --test claude_live -- \
    --ignored --exact claude_live_turns_resume_and_cancel --nocapture > "$out/claude-live.log" 2>&1 || true
fi
python3 - "$verdict" "$out" <<'PY'
import json, os, sys
from pathlib import Path
verdict, out = Path(sys.argv[1]), Path(sys.argv[2])
try:
    v = json.loads(verdict.read_text())
except (OSError, ValueError) as exc:
    v = {'status': 'FAIL', 'detail': f'no verdict ({exc}); see claude-live.log', 'sessions': []}
status = v.get('status')
line = {'schema_version': '1.0', 'test_id': 'TEST-025-04', 'name': 'crates/hoh-agents/tests/claude_live.rs::claude_live_turns_resume_and_cancel',
        'status': {'PASS': 'PASS', 'BLOCKED': 'SKIPPED'}.get(status, 'FAIL'), 'detail': f"{status}: {v.get('detail', '')}"[:4096]}
with (out / 'test-results.jsonl').open('a', encoding='utf-8') as f:
    f.write(json.dumps(line, ensure_ascii=False) + '\n')
sessions = [s for s in v.get('sessions', []) if s.get('session_id')]
candidate = os.environ.get('HOH_CANDIDATE_ID')
if sessions and candidate:
    (out / 'live-sessions.json').write_text(json.dumps({'schema_version': '1.0', 'check_id': os.environ.get('HOH_CHECK_ID', 'adapter-live'),
                                                        'harness_candidate_id': candidate, 'sessions': sessions}, indent=1))
print(f"adapter-live: {status}: {v.get('detail', '')}", file=sys.stderr)
sys.exit({'PASS': 0, 'BLOCKED': 3}.get(status, 1))
PY
