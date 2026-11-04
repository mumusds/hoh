"""A fake `codex app-server` replaying recorded JSON-RPC frames (M1-T04 tests; test code, not part of the product).

Usage: app_server.py FRAMES REQUESTS LOGDIR [--exit-when-done]

FRAMES is the recorded server output (one JSON frame per line, ids as recorded); REQUESTS maps each recorded request
id to its method ({"0": "initialize", ...}). Frames are sent in order. A response waits until the client has sent a
request of that method that is not answered yet, and goes out with the client's id; notifications and server
requests go out as soon as everything before them has. When every frame is out the fake waits for stdin to close
(or exits at once with --exit-when-done, a stream cut short). A frame {"hoh_sleep": SECONDS} is not sent: the fake
pauses there. In LOGDIR it keeps received.jsonl (every client line),
sent.jsonl (the exact bytes sent) and trace.jsonl (both directions in order).
"""
import json
import sys
import time
from pathlib import Path


def main() -> int:
    frames_path, requests_path, logdir = sys.argv[1], sys.argv[2], Path(sys.argv[3])
    exit_when_done = '--exit-when-done' in sys.argv[4:]
    logdir.mkdir(parents=True, exist_ok=True)
    frames = [line for line in Path(frames_path).read_text(encoding='utf-8').splitlines() if line.strip()]
    recorded = {int(k): v for k, v in json.loads(Path(requests_path).read_text(encoding='utf-8')).items()}
    received = open(logdir / 'received.jsonl', 'a', encoding='utf-8')
    sent = open(logdir / 'sent.jsonl', 'ab')
    trace = open(logdir / 'trace.jsonl', 'a', encoding='utf-8')
    unanswered: list[tuple[object, str]] = []  # client requests (id, method) in arrival order
    out = sys.stdout.buffer

    def emit(frame: dict) -> None:
        data = (json.dumps(frame, ensure_ascii=False, separators=(',', ':')) + '\n').encode('utf-8')
        out.write(data)
        out.flush()
        sent.write(data)
        sent.flush()
        trace.write(json.dumps({'dir': 'out', 'method': frame.get('method'), 'id': frame.get('id'),
                                'response': 'method' not in frame}) + '\n')
        trace.flush()

    def pump() -> None:
        while frames:
            frame = json.loads(frames[0])
            if 'hoh_sleep' in frame:  # not sent: a pause before what follows
                frames.pop(0)
                time.sleep(float(frame['hoh_sleep']))
                continue
            if 'method' not in frame:  # a response to a recorded request
                method = recorded.get(frame.get('id'))
                match = next((i for i, (_, m) in enumerate(unanswered) if m == method), None)
                if match is None:
                    return
                frame['id'] = unanswered.pop(match)[0]
            frames.pop(0)
            emit(frame)

    pump()
    while True:
        if exit_when_done and not frames:
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
            unanswered.append((message['id'], message['method']))
        pump()


if __name__ == '__main__':
    sys.exit(main())
