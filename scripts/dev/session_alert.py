"""Write down why a main development session's turn or session ended (Claude Code StopFailure / SessionEnd hook
payload on stdin), with a recovery point, in .hoh-dev/session-alerts.jsonl; for a StopFailure also print an OSC 9
desktop notification. Run by scripts/dev/session-alert.sh from the project root.

The event itself always goes down: every part of the recovery point is collected on its own, and one that cannot be
collected is recorded as unavailable instead of losing the alert (SAR-02). The notification does not depend on the
record being written."""
import datetime, json, os, re, subprocess, sys

MESSAGE_LIMIT = 300
FIELD_LIMIT = 1000
PAYLOAD_LIMIT = 1 << 20
BACKGROUND_LIMIT = 10


def text(value, limit=FIELD_LIMIT):
    """A payload field as recorded: strings cut to `limit`, anything else as JSON text, missing as None."""
    if value is None:
        return None
    if not isinstance(value, str):
        value = json.dumps(value, ensure_ascii=False)
    return value[:limit]


def safe(collect):
    try:
        return collect()
    except Exception as e:  # noqa: BLE001 - a recovery point part must never lose the alert
        return f'unavailable: {type(e).__name__}'


def run(*argv):
    return subprocess.run(argv, capture_output=True, text=True, timeout=3).stdout.strip()


def running_tasks():
    with open('.hoh-dev/task-state.json') as f:
        tasks = json.load(f).get('tasks', {})
    return [name for name, task in tasks.items() if task.get('status') == 'running']


def newest_evidence():
    entries = [os.path.join('.hoh-dev/evidence', e) for e in os.listdir('.hoh-dev/evidence')]
    return max(entries, key=os.path.getmtime) if entries else None


# A python interpreter however it is spelled (path, version suffix, options), running a check, a delegated session or
# a mutation run.
BACKGROUND = re.compile(
    r'^\S+ \S+ (\S*/)?python[0-9.]*( -\S+)* \S*(tools/(run_check|agent_task)\.py|mutate\w*\.py)( |$)')


def background():
    """Development work still running, the newest last."""
    lines = run('ps', '-eo', 'pid,etime,args', '--sort=start_time').splitlines()[1:]
    found = [line[:160] for line in (' '.join(l.split()) for l in lines) if BACKGROUND.search(line)]
    return found[-BACKGROUND_LIMIT:]


def next_step():
    with open('.hoh-dev/PROGRESS.md') as f:
        found = re.search(r'^## Next\n(.*?)(?=^## |\Z)', f.read(), re.S | re.M)
    return [line.strip() for line in found.group(1).splitlines() if line.strip()][:5] if found else []


def main():
    try:
        event = json.loads(sys.stdin.buffer.read(PAYLOAD_LIMIT) or b'{}')
    except ValueError:
        event = {}
    if not isinstance(event, dict):
        event = {}
    name = text(event.get('hook_event_name'), 100) or ''
    error = text(event.get('error'), 100)
    if name == 'StopFailure':
        # OSC 9; only printable ASCII between, so the sequence stays on Claude Code's allowlist.
        note = re.sub(r'[^ -~]', '', f"HoH: the turn ended on an API error ({error or 'unknown'}); "
                                    'see .hoh-dev/session-alerts.jsonl')
        print(json.dumps({'terminalSequence': f'\x1b]9;{note}\x07'}), flush=True)
    record = {
        'at': datetime.datetime.now(datetime.timezone.utc).isoformat(timespec='seconds'),
        'event': name,
        'error': error,
        'error_details': text(event.get('error_details')),
        'message': text(event.get('last_assistant_message'), MESSAGE_LIMIT) if name == 'StopFailure' else None,
        'reason': text(event.get('reason'), 100),
        'session_id': text(event.get('session_id'), 100),
        'recovery': {
            'tasks': safe(running_tasks),
            'branch': safe(lambda: run('git', 'branch', '--show-current')),
            'head': safe(lambda: run('git', 'log', '-1', '--format=%h %s')[:100]),
            'newest_evidence': safe(newest_evidence),
            'background': safe(background),
            'next': safe(next_step),
        },
    }
    os.makedirs('.hoh-dev', exist_ok=True)
    with open('.hoh-dev/session-alerts.jsonl', 'a') as out:
        out.write(json.dumps(record, ensure_ascii=False) + '\n')


if __name__ == '__main__':
    main()
