#!/usr/bin/env bash
# Claude Code StopFailure / SessionEnd hook: write down why the main development session's turn or session ended,
# with a recovery point (the running task, branch and commit, the newest evidence, background work still running, the
# next step from PROGRESS.md), in .hoh-dev/session-alerts.jsonl (git-ignored). The next session start shows what is new
# there (scripts/dev/session-context.sh). Claude Code ignores StopFailure output except a terminal sequence, so a
# StopFailure also asks for a desktop notification (OSC 9) and prints nothing else. Delegated sessions started by
# tools/agent_task.py carry HOH_BOOTSTRAP_PARENT and are left alone (reviewers stay read-only). Never fails the session.
# This records and reminds; it does not retry or resume anything (Claude Code itself retries transient API errors).
set -u
[[ -z "${HOH_BOOTSTRAP_PARENT:-}" ]] || exit 0
root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd -P)" || exit 0
cd "$root" || exit 0
mkdir -p .hoh-dev 2>/dev/null || exit 0
# The program comes on python's stdin, so the hook's payload goes by the environment.
HOH_HOOK_PAYLOAD="$(cat)" python3 - 2>/dev/null <<'PY' || true
import datetime, json, os, re, subprocess

try:
    event = json.loads(os.environ.get('HOH_HOOK_PAYLOAD') or '{}')
except ValueError:
    event = {}
if not isinstance(event, dict):
    event = {}


def run(*argv):
    try:
        return subprocess.run(argv, capture_output=True, text=True, timeout=3).stdout.strip()
    except (OSError, subprocess.SubprocessError):
        return ''


def running_tasks():
    try:
        tasks = json.load(open('.hoh-dev/task-state.json')).get('tasks', {})
    except (OSError, ValueError):
        return []
    return [name for name, task in tasks.items() if task.get('status') == 'running']


def newest_evidence():
    try:
        entries = [os.path.join('.hoh-dev/evidence', e) for e in os.listdir('.hoh-dev/evidence')]
    except OSError:
        return None
    return max(entries, key=os.path.getmtime) if entries else None


def background():
    """Development work still running: checks, delegated sessions, mutation runs."""
    lines = [' '.join(line.split()) for line in run('ps', '-eo', 'pid,etime,args').splitlines()[1:]]
    wanted = re.compile(r'^\S+ \S+ python3? \S*(tools/(run_check|agent_task)\.py|mutate\.py)')
    return [line[:160] for line in lines if wanted.search(line)][:10]


def next_step():
    try:
        text = open('.hoh-dev/PROGRESS.md').read()
    except OSError:
        return []
    found = re.search(r'^## Next\n(.*?)(?=^## |\Z)', text, re.S | re.M)
    return [line.strip() for line in found.group(1).splitlines() if line.strip()][:5] if found else []


name = event.get('hook_event_name') or ''
record = {
    'at': datetime.datetime.now(datetime.timezone.utc).isoformat(timespec='seconds'),
    'event': name,
    'error': event.get('error'),
    'error_details': event.get('error_details'),
    'message': (event.get('last_assistant_message') or '')[:300] if name == 'StopFailure' else None,
    'reason': event.get('reason'),
    'session_id': event.get('session_id'),
    'recovery': {
        'tasks': running_tasks(),
        'branch': run('git', 'branch', '--show-current'),
        'head': run('git', 'log', '-1', '--format=%h %s')[:100],
        'newest_evidence': newest_evidence(),
        'background': background(),
        'next': next_step(),
    },
}
with open('.hoh-dev/session-alerts.jsonl', 'a') as out:
    out.write(json.dumps(record, ensure_ascii=False) + '\n')
if name == 'StopFailure':
    # OSC 9 desktop notification; the text keeps only printable ASCII so the sequence stays on the allowlist.
    text = re.sub(r'[^ -~]', '', f"HoH: the turn ended on an API error ({record['error'] or 'unknown'}); see .hoh-dev/session-alerts.jsonl")
    print(json.dumps({'terminalSequence': f'\x1b]9;{text}\x07'}))
PY
exit 0
