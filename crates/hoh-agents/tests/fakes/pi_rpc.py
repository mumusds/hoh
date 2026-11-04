"""A fake `pi --mode rpc` replaying recorded RPC records (M1-T05 tests; test code, not part of the product).

Usage: pi_rpc.py FRAMES COMMANDS LOGDIR -- PI-ARGV...

FRAMES is the recorded stdout (one JSON record per line); COMMANDS maps each recorded command id to its type
({"s1": "get_state", "p1": "prompt"}), in the order recorded. Records are sent in order. A response to the k-th recorded
command of a type waits until the client has sent its k-th command of that type, and goes out with the client's id;
other records go out as soon as everything before them has. When every record is out the fake waits for stdin to
close. With `--session-dir D --session-id I` in PI-ARGV it keeps a session file for I in D (as pi does) and puts I in
place of the recorded session id.

Pseudo-records (not sent as such): {"hoh_wait": {"command": T, "count": N}} waits until N commands of type T came;
{"hoh_batch": [R, ...]} sends records in one write; {"hoh_split": [R, N]} sends R in N writes; {"hoh_stderr": BYTES}
writes that many bytes to stderr; {"hoh_hold": PATH} waits until PATH exists; {"hoh_exit": CODE} exits at once.
LOGDIR gets received.jsonl (every client line) and sent.jsonl (the bytes sent). Records are split on LF only:
str.splitlines() would also split on U+2028/U+2029 inside JSON strings, which is what this fake exists to test.
"""
import json
import os
import sys
import time
from pathlib import Path


def main() -> int:
    frames_path, commands_path, logdir = sys.argv[1], sys.argv[2], Path(sys.argv[3])
    argv = sys.argv[sys.argv.index('--') + 1:] if '--' in sys.argv else []
    logdir.mkdir(parents=True, exist_ok=True)
    recorded = json.loads(Path(commands_path).read_text(encoding='utf-8'))
    ordinal = {cid: [c for c, t in recorded.items() if t == kind].index(cid) for cid, kind in recorded.items()}
    text = Path(frames_path).read_text(encoding='utf-8')
    session_id = argv[argv.index('--session-id') + 1] if '--session-id' in argv else None
    if session_id:
        old = next((json.loads(l)['data']['sessionId'] for l in text.split('\n')
                    if l.strip() and json.loads(l).get('command') == 'get_state' and 'data' in json.loads(l)), None)
        if old:
            text = text.replace(old, session_id)
        if '--session-dir' in argv:
            d = Path(argv[argv.index('--session-dir') + 1])
            d.mkdir(parents=True, exist_ok=True)
            if not any(p.name.endswith(f'_{session_id}.jsonl') for p in d.iterdir()):
                (d / f'2026-11-04T00-00-00-000Z_{session_id}.jsonl').write_text('{"type":"session"}\n')
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
        code = pump()
        if code is not None:
            return code


if __name__ == '__main__':
    sys.exit(main())
