#!/usr/bin/env bash
# Check "adapter-live" (mode live): the native adapters against the installed CLIs, with model calls.
#   TEST-025-04  claude  crates/hoh-agents/tests/claude_live.rs::claude_live_turns_resume_and_cancel
#   TEST-024-04  codex   crates/hoh-agents/tests/codex_live.rs::codex_live_turn_and_cancel
#   TEST-026-04  pi      crates/hoh-agents/tests/pi_live.rs::pi_live_turns_resume_and_cancel
#   TEST-006-03  doctor  crates/hoh-app/tests/doctor_live.rs::doctor_live_versions_whitelist_and_verified_logins
#   TEST-053-05  qa      crates/hoh-app/tests/verification_live.rs::verification_live_candidate_to_qa
# Each is an ignored test whose name carries no TEST id (so the integration check never reports it as skipped); it
# writes $HOH_EVIDENCE_DIR/<agent>-live-verdict.json, which this script turns into test-results.jsonl lines and
# live-sessions.json (contracts/live-session.schema.json). A verdict is PASS, BLOCKED (CLI missing, not logged in, or
# claude run as root), NOT_RUN (pi not installed, AC-026-04) or FAIL; BLOCKED and NOT_RUN are reported SKIPPED, never
# PASS. Exit 0 = all PASS, 1 = any FAIL, 3 = otherwise BLOCKED or NOT_RUN.
# qa is one candidate->QA loop through hoh with the default team (docs/adr/ADR-029.md, AC-053-05): Developer claude,
# QA pi, Reviewer codex; claude in place of a missing pi or codex only under an explicit HOH_LIVE_PROFILE=v0.1. It is
# BLOCKED without claude, as root, or without pi or codex otherwise; its PASS counts only with the native sessions of
# the agents its verdict says it bound.
# The parent agent session's variables are removed first, as the protocol recorder does, so a nested run is a fresh
# native session.
set -Eeuo pipefail
cd "$(dirname -- "${BASH_SOURCE[0]}")/../.."
out="${HOH_EVIDENCE_DIR:-$(mktemp -d)}"
mkdir -p "$out"
unset AI_AGENT CLAUDECODE CLAUDE_PID CLAUDE_EFFORT CLAUDE_CODE_SESSION_ID CLAUDE_CODE_CHILD_SESSION \
  CLAUDE_CODE_MESSAGING_SOCKET CLAUDE_CODE_MESSAGING_TOKEN CLAUDE_CODE_SESSION_ATTENDED CLAUDE_CODE_ENTRYPOINT \
  CLAUDE_CODE_EXECPATH CODEX_SANDBOX CODEX_SANDBOX_NETWORK_DISABLED

# live <agent> <test binary> <test name> [package]
live() {
  local agent="$1" binary="$2" name="$3" package="${4:-hoh-agents}" verdict cli bin codex pi missing=""
  verdict="$out/$agent-live-verdict.json"
  rm -f "$verdict"
  # doctor runs every installed CLI itself ("hoh" stands for the binary under test, always present); qa needs claude for
  # its Developer, and pi and codex for its default QA and Reviewer unless HOH_LIVE_PROFILE=v0.1.
  case "$agent" in doctor) cli=hoh;; qa) cli=claude;; *) cli="$agent";; esac
  if [[ "$cli" == hoh ]]; then bin=hoh; else bin="$(command -v "$cli" || true)"; fi
  codex="$bin" pi="$bin"
  if [[ "$agent" == qa ]]; then
    codex="$(command -v codex || true)" pi="$(command -v pi || true)"
    if [[ "${HOH_LIVE_PROFILE:-}" != v0.1 ]]; then
      [[ -n "$pi" ]] || missing+="pi "
      [[ -n "$codex" ]] || missing+="codex "
    fi
  fi
  if [[ -z "$bin" && "$agent" == pi ]]; then
    printf '{"status": "NOT_RUN", "detail": "pi is not installed", "sessions": []}\n' > "$verdict"
  elif [[ -z "$bin" ]]; then
    printf '{"status": "BLOCKED", "detail": "%s is not installed", "sessions": []}\n' "$cli" > "$verdict"
  elif [[ ("$agent" == claude || "$agent" == qa) && "$(id -u)" == 0 ]]; then
    # AC-025-04 runs as a non-root user: the adapter never bypasses permissions as root.
    printf '{"status": "BLOCKED", "detail": "running as root", "sessions": []}\n' > "$verdict"
  elif [[ -n "$missing" ]]; then
    printf '{"status": "BLOCKED", "detail": "%s not installed (the default QA and Reviewer; HOH_LIVE_PROFILE=v0.1 puts claude in their place)", "sessions": []}\n' \
      "${missing% }" > "$verdict"
  else
    HOH_LIVE_OUT="$out" HOH_LIVE_CLAUDE="$bin" HOH_LIVE_CODEX="$codex" HOH_LIVE_PI="$pi" cargo test -p "$package" --locked \
      --test "$binary" -- --ignored --exact "$name" --nocapture > "$out/$agent-live.log" 2>&1 || true
  fi
}
live claude claude_live claude_live_turns_resume_and_cancel
live codex codex_live codex_live_turn_and_cancel
live pi pi_live pi_live_turns_resume_and_cancel
# doctor runs every installed CLI itself; "hoh" stands for the binary under test (always present).
live doctor doctor_live doctor_live_versions_whitelist_and_verified_logins hoh-app
live qa verification_live verification_live_candidate_to_qa hoh-app

python3 - "$out" <<'PY'
import json, os, sys
from pathlib import Path
out = Path(sys.argv[1])
cases = [('claude', 'TEST-025-04', 'crates/hoh-agents/tests/claude_live.rs::claude_live_turns_resume_and_cancel'),
         ('codex', 'TEST-024-04', 'crates/hoh-agents/tests/codex_live.rs::codex_live_turn_and_cancel'),
         ('pi', 'TEST-026-04', 'crates/hoh-agents/tests/pi_live.rs::pi_live_turns_resume_and_cancel'),
         ('doctor', 'TEST-006-03', 'crates/hoh-app/tests/doctor_live.rs::doctor_live_versions_whitelist_and_verified_logins'),
         ('qa', 'TEST-053-05', 'crates/hoh-app/tests/verification_live.rs::verification_live_candidate_to_qa')]
# The team TEST-053-05 may pass with: the default bindings, or claude in place of QA and Reviewer under v0.1.
team = {'developer': {'claude'}, 'qa': {'pi'}, 'reviewer': {'codex'}}
if os.environ.get('HOH_LIVE_PROFILE') == 'v0.1':
    team = {role: agents | {'claude'} for role, agents in team.items()}
statuses, sessions = [], []
with (out / 'test-results.jsonl').open('a', encoding='utf-8') as results:
    for agent, test_id, name in cases:
        try:
            v = json.loads((out / f'{agent}-live-verdict.json').read_text())
        except (OSError, ValueError) as exc:
            v = {'status': 'FAIL', 'detail': f'no verdict ({exc}); see {agent}-live.log', 'sessions': []}
        status = v.get('status')
        if agent == 'qa' and status == 'PASS':
            # A PASS counts only with the native sessions of the agents it bound (rule 16 judges the whole check):
            # the Developer's and the QA's two sessions at least, every bound agent among them.
            bound = v['agents'] if isinstance(v.get('agents'), dict) else {}
            ran = [s for s in v.get('sessions', []) if s.get('session_id')]
            if (set(bound) != set(team) or any(bound[r] not in team[r] for r in team)
                    or not set(bound.values()) <= {s.get('agent') for s in ran}
                    or len({s['session_id'] for s in ran}) < 2):
                status, v = 'FAIL', {**v, 'detail': f"PASS without the live sessions of its bindings {bound}: {v.get('detail', '')}"}
        statuses.append(status)
        results.write(json.dumps({'schema_version': '1.0', 'test_id': test_id, 'name': name,
                                  'status': {'PASS': 'PASS', 'BLOCKED': 'SKIPPED', 'NOT_RUN': 'SKIPPED'}.get(status, 'FAIL'),
                                  'detail': f"{status}: {v.get('detail', '')}"[:4096]}, ensure_ascii=False) + '\n')
        sessions += [s for s in v.get('sessions', []) if s.get('session_id')]
        print(f"adapter-live: {agent}: {status}: {v.get('detail', '')}", file=sys.stderr)
candidate = os.environ.get('HOH_CANDIDATE_ID')
if sessions and candidate:
    # Whole or not at all (docs/adr/ADR-027.md M2T10D-752): a run killed mid-write leaves no half document.
    staged = out / '.live-sessions.json.tmp'
    staged.write_text(json.dumps({'schema_version': '1.0', 'check_id': os.environ.get('HOH_CHECK_ID', 'adapter-live'),
                                  'harness_candidate_id': candidate, 'sessions': sessions}, indent=1))
    os.replace(staged, out / 'live-sessions.json')
sys.exit(1 if any(s not in ('PASS', 'BLOCKED', 'NOT_RUN') for s in statuses)
         else 3 if {'BLOCKED', 'NOT_RUN'} & set(statuses) else 0)
PY
