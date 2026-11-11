"""A fake `pi --mode rpc` replaying recorded RPC records (M1-T05 tests; test code, not part of the product).

Usage: pi_rpc.py FRAMES COMMANDS LOGDIR -- PI-ARGV...

FRAMES is the recorded stdout (one JSON record per line); COMMANDS maps each recorded command id to its type
({"s1": "get_state", "p1": "prompt"}), in the order recorded. Records are sent in order. A response to the k-th recorded
command of a type waits until the client has sent its k-th command of that type, and goes out with the client's id;
other records go out as soon as everything before them has. When every record is out the fake waits for stdin to
close. With `--session-dir D --session-id I` in PI-ARGV it finds or makes the session file as pi 1.5.0 does (the first
*.jsonl of D whose header has id I and this cwd, else a new one) and puts I and that file in place of the recorded
session id and file.

Pseudo-records (not sent as such): {"hoh_wait": {"command": T, "count": N}} waits until N commands of type T came;
{"hoh_batch": [R, ...]} sends records in one write; {"hoh_split": [R, N]} sends R in N writes; {"hoh_stderr": BYTES}
writes that many bytes to stderr; {"hoh_hold": PATH} waits until PATH exists; {"hoh_exit": CODE} exits at once; a {"hoh_keep_session": true} anywhere keeps the recorded session id; a {"hoh_new_file": true} anywhere makes
a new session file even if one is found; a {"hoh_lose_session": true} anywhere deletes the found file and reports a new
path it never writes; a {"hoh_lazy_session": true} anywhere writes a new session file only once the first prompt comes
(pi 1.5.0 persists a session only when it has a message: SessionManager._persist and _hasConversation).
LOGDIR gets received.jsonl (every client line) and sent.jsonl (the bytes sent). Records are split on LF only:
str.splitlines() would also split on U+2028/U+2029 inside JSON strings, which is what this fake exists to test.
"""
import json
import os
import sys
import time
from pathlib import Path


HEADER_SCAN = 1 << 20


def header_of(path: Path):
    """pi's readSessionHeader: within the first MiB, skip blank lines and lines that are not JSON or are JSON that is
    falsy in JS; the first other value decides (a session header with a string id, or not a session)."""
    data = path.read_bytes()[:HEADER_SCAN + 1]
    ended = len(data) <= HEADER_SCAN
    lines = data[:HEADER_SCAN].decode('utf-8', 'replace').split('\n')
    if not ended:
        lines.pop()
    for line in lines:
        if not line.strip().strip('\ufeff').strip():
            continue
        try:
            value = json.loads(line)
        except ValueError:
            continue
        if value in (None, False, 0, '') and not isinstance(value, (dict, list)):
            continue
        return value if isinstance(value, dict) and value.get('type') == 'session' and isinstance(value.get('id'), str) else None
    return None


def main() -> int:
    frames_path, commands_path, logdir = sys.argv[1], sys.argv[2], Path(sys.argv[3])
    argv = sys.argv[sys.argv.index('--') + 1:] if '--' in sys.argv else []
    logdir.mkdir(parents=True, exist_ok=True)
    recorded = json.loads(Path(commands_path).read_text(encoding='utf-8'))
    ordinal = {cid: [c for c, t in recorded.items() if t == kind].index(cid) for cid, kind in recorded.items()}
    text = Path(frames_path).read_text(encoding='utf-8')
    session_id = argv[argv.index('--session-id') + 1] if '--session-id' in argv else None
    keep = '"hoh_keep_session"' in text  # a pi that reports another session than the one asked for
    fresh = '"hoh_new_file"' in text  # a pi that does not find the session and makes a new file for the id
    lose = '"hoh_lose_session"' in text  # the session file vanishes; pi names a new path it has not written yet
    lazy = '"hoh_lazy_session"' in text  # a new session is written to its file only once a prompt comes
    text = '\n'.join(l for l in text.split('\n')
                     if not any(k in l for k in ('"hoh_keep_session"', '"hoh_new_file"', '"hoh_lose_session"',
                                                 '"hoh_lazy_session"')))
    unwritten = None  # (path, header) of a new session not written yet (lazy)
    if session_id and not keep:
        recorded_state = next((json.loads(l)['data'] for l in text.split('\n')
                               if l.strip() and json.loads(l).get('command') == 'get_state' and 'data' in json.loads(l)), None)
        if '--session-dir' in argv:
            # As pi 1.5.0 SessionManager.findById: the *.jsonl files in name order, the first whose header (see
            # header_of) has this id and a cwd that resolves, by name, to this directory; else a new file.
            d = Path(argv[argv.index('--session-dir') + 1])
            d.mkdir(parents=True, exist_ok=True)
            cwd = os.getcwd()  # physical, as process.cwd()
            found = None
            for f in sorted(p for p in d.iterdir() if p.name.endswith('.jsonl')):
                if not f.is_file():
                    continue
                header = header_of(f)
                if header and header.get('id') == session_id and isinstance(header.get('cwd'), str) and header['cwd'] \
                        and os.path.normpath(os.path.join(cwd, os.path.expanduser(header['cwd']))) == cwd:
                    found = f
                    break
            if lose and found is not None:
                found.unlink()
                found = d / f'2026-11-04T09-09-09-000Z_{session_id}.jsonl'
            elif found is None or fresh:
                found = d / f'2026-11-04T00-00-{len(list(d.glob("*.jsonl"))):02d}-000Z_{session_id}.jsonl'
                header = json.dumps({'type': 'session', 'version': 3, 'id': session_id,
                                     'timestamp': '2026-11-04T00:00:00.000Z', 'cwd': cwd}) + '\n'
                if lazy:
                    unwritten = (found, header)
                else:
                    found.write_text(header)
            if recorded_state and recorded_state.get('sessionFile'):
                text = text.replace(json.dumps(recorded_state['sessionFile'])[1:-1], str(found))
        if recorded_state and recorded_state.get('sessionId'):
            text = text.replace(recorded_state['sessionId'], session_id)
    frames = [l for l in text.split('\n') if l.strip()]
    received = open(logdir / 'received.jsonl', 'a', encoding='utf-8')
    sent = open(logdir / 'sent.jsonl', 'ab')
    by_type: dict[str, list] = {}
    out = sys.stdout.buffer

    def write(data: bytes) -> None:
        out.write(data)
        out.flush()
        sent.write(data)
        sent.flush()

    def line(record: dict) -> bytes:
        return (json.dumps(record, ensure_ascii=False, separators=(',', ':')) + '\n').encode('utf-8')

    def answer(record: dict) -> bool:
        cid = record.get('id')
        kind, k = recorded.get(cid), ordinal.get(cid)
        ids = by_type.get(kind, [])
        if k is None or len(ids) <= k:
            return False
        record['id'] = ids[k]
        return True

    def pump() -> int | None:
        while frames:
            r = json.loads(frames[0])
            if 'hoh_wait' in r:
                if len(by_type.get(r['hoh_wait']['command'], [])) < r['hoh_wait']['count']:
                    return None
            elif 'hoh_hold' in r:
                while not Path(r['hoh_hold']).exists():
                    time.sleep(0.01)
            elif 'hoh_exit' in r:
                return int(r['hoh_exit'])
            elif 'hoh_stderr' in r:
                chunk = b'e' * 65536
                left = int(r['hoh_stderr'])
                while left > 0:
                    sys.stderr.buffer.write(chunk[:left])
                    left -= len(chunk)
                sys.stderr.buffer.flush()
            elif 'hoh_batch' in r:
                batch = r['hoh_batch']
                if not all(answer(x) for x in batch if x.get('type') == 'response'):
                    return None
                write(b''.join(line(x) for x in batch))
            elif 'hoh_split' in r:
                record, n = r['hoh_split']
                if record.get('type') == 'response' and not answer(record):
                    return None
                data = line(record)
                step = max(1, len(data) // n)
                for i in range(0, len(data), step):
                    write(data[i:i + step])
                    time.sleep(0.02)
            else:
                if r.get('type') == 'response' and not answer(r):
                    return None
                write(line(r))
            frames.pop(0)
        return None

    code = pump()
    if code is not None:
        return code
    while True:
        message = sys.stdin.readline()
        if not message:
            return 0
        received.write(message if message.endswith('\n') else message + '\n')
        received.flush()
        try:
            command = json.loads(message)
        except ValueError:
            continue
        if command.get('type') != 'extension_ui_response' and 'type' in command:
            by_type.setdefault(command['type'], []).append(command.get('id'))
        if command.get('type') == 'prompt' and unwritten is not None:
            unwritten[0].write_text(unwritten[1])
            unwritten = None
        code = pump()
        if code is not None:
            return code


if __name__ == '__main__':
    sys.exit(main())
