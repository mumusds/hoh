"""A fake `codex app-server` replaying recorded JSON-RPC frames (M1-T04 tests; test code, not part of the product).

Usage: app_server.py FRAMES REQUESTS LOGDIR [--exit-when-done]

FRAMES is the recorded server output (one JSON frame per line, ids as recorded); REQUESTS maps each recorded request
id to its method ({"0": "initialize", ...}). Frames are sent in order. A response to the k-th recorded request of a
method waits until the client has sent its k-th request of that method, and goes out with the client's id; notifications and server
requests go out as soon as everything before them has. When every frame is out the fake waits for stdin to close
(or exits at once with --exit-when-done, a stream cut short). A frame {"hoh_sleep": SECONDS} is not sent: the fake
pauses there; {"hoh_wait": {"method": M, "count": N}} is not sent either: the fake waits there until the client
has sent N requests of method M; {"hoh_close_stdin": true} closes the fake's stdin (it sends what follows, then
exits); {"hoh_batch": [FRAME, ...]} sends those frames in one write. In LOGDIR it keeps received.jsonl (every client line),
sent.jsonl (the exact bytes sent) and trace.jsonl (both directions in order).
"""
import json
import os
import sys
import time
from pathlib import Path


def main() -> int:
    frames_path, requests_path, logdir = sys.argv[1], sys.argv[2], Path(sys.argv[3])
    exit_when_done = '--exit-when-done' in sys.argv[4:]
    logdir.mkdir(parents=True, exist_ok=True)
    # LF only: str.splitlines() would also split on U+2028/U+2029 inside JSON strings.
    frames = [line for line in Path(frames_path).read_text(encoding='utf-8').split('\n') if line.strip()]
    recorded = {int(k): v for k, v in json.loads(Path(requests_path).read_text(encoding='utf-8')).items()}
    received = open(logdir / 'received.jsonl', 'a', encoding='utf-8')
    sent = open(logdir / 'sent.jsonl', 'ab')
    trace = open(logdir / 'trace.jsonl', 'a', encoding='utf-8')
    by_method: dict[str, list] = {}  # client request ids per method, in arrival order
    # A recorded request answers the client request of its method with the same position (the k-th of that method).
    ordinal = {rid: sorted(r for r, m in recorded.items() if m == method).index(rid) for rid, method in recorded.items()}
    received_methods: list[str] = []  # methods of every client request so far
    closed = []  # set once stdin is closed by {"hoh_close_stdin": true}
    out = sys.stdout.buffer

    def emit(*batch: dict) -> None:
        """Send frames in one write (one frame, or a batch the client reads together)."""
        data = b''.join((json.dumps(f, ensure_ascii=False, separators=(',', ':')) + '\n').encode('utf-8') for f in batch)
        out.write(data)
        out.flush()
        sent.write(data)
        sent.flush()
        for frame in batch:
            trace.write(json.dumps({'dir': 'out', 'method': frame.get('method'), 'id': frame.get('id'),
                                    'response': 'method' not in frame}) + '\n')
        trace.flush()

    def answer(frame: dict) -> bool:
        """Give a recorded response the id of the client request it answers; False if that request has not come."""
        rid = frame.get('id')
        method, k = recorded.get(rid), ordinal.get(rid)
        ids = by_method.get(method, [])
        if k is None or len(ids) <= k:
            return False
        frame['id'] = ids[k]
        return True

    def pump() -> None:
        while frames:
            frame = json.loads(frames[0])
            if 'hoh_sleep' in frame:  # not sent: a pause before what follows
                frames.pop(0)
                time.sleep(float(frame['hoh_sleep']))
                continue
            if 'hoh_close_stdin' in frame:  # not sent: close stdin, so the client's next write fails
                frames.pop(0)
                os.close(0)
                closed.append(True)
                continue
            if 'hoh_wait' in frame:  # not sent: wait until the client has sent this many requests of a method
                want = frame['hoh_wait']
                if received_methods.count(want['method']) < want['count']:
                    return
                frames.pop(0)
                continue
            if 'hoh_batch' in frame:  # these frames in one write; its responses must be answerable now
                batch = frame['hoh_batch']
                if not all(answer(f) for f in batch if 'method' not in f):
                    return
                frames.pop(0)
                emit(*batch)
                continue
            if 'method' not in frame and not answer(frame):  # a response to a recorded request
                return
            frames.pop(0)
            emit(frame)

    pump()
    while True:
        if (exit_when_done and not frames) or closed:
            return 0
        line = sys.stdin.readline()
        if not line:
            return 0
        received.write(line if line.endswith('\n') else line + '\n')
        received.flush()
        try:
            message = json.loads(line)
        except ValueError:
            continue
        trace.write(json.dumps({'dir': 'in', 'method': message.get('method'), 'id': message.get('id'),
                                'response': 'method' not in message}) + '\n')
        trace.flush()
        if 'method' in message and 'id' in message:
            by_method.setdefault(message['method'], []).append(message['id'])
            received_methods.append(message['method'])
        pump()
        if closed:
            return 0


if __name__ == '__main__':
    sys.exit(main())
