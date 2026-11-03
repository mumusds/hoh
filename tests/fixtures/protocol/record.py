#!/usr/bin/env python3
"""Re-record native protocol fixtures (M0-T04, hardened in M0-T06). Live: costs model calls for authenticated agents.

Usage: python3 tests/fixtures/protocol/record.py [--agents claude,codex,pi]

Every case runs in a fresh temporary Git repository OUTSIDE this checkout (no project CLAUDE.md or
AGENTS.md leaks in) with the parent agent session variables removed. The operator's user-level agent
configuration (plugins, hooks, auth) still applies and is recorded as-is; it cannot be isolated
without copying credentials. Each case stores meta.json (argv, exit code, validity and the problems
found), the exact stdin, raw stdout.jsonl and stderr.log.

Each agent version is recorded into a staging directory next to the published one. It replaces the
published directory in one rename only when every case shows what it specifically claims (VALIDATORS)
and the binary did not change during the run. Otherwise the published fixtures and their provenance
stay as they were, the staging directory moves to .hoh-dev/protocol-failed/ and the run exits 1, so a
failed re-recording never leaves older successes looking current (M0R3-01, M0R3-02). Provenance
(binary and help hashes, auth state) is measured while recording and stored with the fixtures.
Child processes are stopped with bounded escalation: close stdin, SIGTERM, SIGKILL, reap (M0R3-03).
"""
from __future__ import annotations
import argparse, ctypes, errno, hashlib, json, os, selectors, shutil, signal, subprocess, sys, tempfile, time
from datetime import datetime, timezone
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[2]
sys.path.insert(0, str(REPO / 'tools'))
from agent_task import PARENT_SESSION_VARS  # noqa: E402  (single source of the stripped variable list)
from common import Terminated, install_termination_handlers, restore_handlers  # noqa: E402

ENV = {k: v for k, v in os.environ.items() if k not in PARENT_SESSION_VARS}
TOKEN = 'HOH_SAMPLE_OK'
TEMP_DIRS: list[Path] = []  # temporary repositories and homes of the current recording, removed by record()


def now() -> str:
    return datetime.now(timezone.utc).isoformat()


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def first_line(argv: list[str]) -> str:
    out = subprocess.run(argv, capture_output=True, text=True, env=ENV, timeout=60).stdout.strip().splitlines()
    return out[0] if out else 'unknown'


def tempdir(prefix: str) -> Path:
    d = Path(tempfile.mkdtemp(prefix=prefix))
    TEMP_DIRS.append(d)
    return d


def workdir() -> Path:
    d = tempdir('hoh-protocol-')
    subprocess.run(['git', 'init', '-q', str(d)], check=True)
    return d


def frames(raw: bytes) -> list[dict]:
    out = []
    for line in raw.splitlines():
        if line.strip():
            try:
                f = json.loads(line)
            except ValueError:
                continue
            if isinstance(f, dict):
                out.append(f)
    return out


# --- processes ------------------------------------------------------------------------------------------

LIBC = ctypes.CDLL(None, use_errno=True)
LIVE: set['Proc'] = set()  # children not yet stopped, so an interrupted recording can stop them (M0R4-01)
TOKEN_VAR = 'HOH_RECORDER_PROC'
SIGNALS = {signal.SIGINT, signal.SIGTERM, signal.SIGHUP}


def proc_state(pid: int) -> str | None:
    """Process state letter from /proc (Z for a zombie), None when the process is gone."""
    try:
        return Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()[0]
    except (OSError, IndexError):
        return None


def alive(pid: int) -> bool:
    return proc_state(pid) not in (None, 'Z')


def start_time(pid: int) -> int | None:
    """Start time in clock ticks since boot (/proc/<pid>/stat field 22), None when the process is gone."""
    try:
        return int(Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()[19])
    except (OSError, IndexError, ValueError):
        return None


def carrying(token: str) -> list[int]:
    """Live processes whose environment holds this Proc's token: the child and every descendant that kept the
    inherited environment, wherever it was re-parented and whatever session or group it moved to (M0R5-02/06).
    A descendant that exec'd with a cleared environment cannot be found this way."""
    needle = f'{TOKEN_VAR}={token}'.encode()
    found = []
    for environ in Path('/proc').glob('[0-9]*/environ'):
        try:
            if needle in environ.read_bytes().split(b'\0') and alive(int(environ.parent.name)):
                found.append(int(environ.parent.name))
        except (OSError, ValueError):
            continue
    return found


def unverifiable(since: int) -> list[int]:
    """Our own live processes started at or after `since` whose environment we may not read (e.g. made non-dumpable):
    they could be descendants, so a case cannot be shown clean while they run (M0R6-02). They are not killed,
    because they could equally be unrelated."""
    found = []
    for environ in Path('/proc').glob('[0-9]*/environ'):
        try:
            pid = int(environ.parent.name)
            if environ.parent.stat().st_uid != os.getuid():
                continue
            environ.read_bytes()
        except PermissionError:
            started = start_time(pid)
            if started is not None and started >= since and alive(pid):
                found.append(pid)
        except (OSError, ValueError):
            continue
    return found


class Stopped(str):
    """How a child ended (exited, SIGTERM, ...), with the command lines of descendants that outlived it: `lingered`
    ended by themselves within the grace period, `descendants` had to be killed, `unverifiable` might be ours but
    their environment cannot be read."""
    descendants: list[str] = []
    lingered: list[str] = []
    unverifiable: list[str] = []


def child_setup() -> None:
    """Runs in the child before exec: default dispositions and an empty mask for the signals the recorder uses, also
    when the recorder itself was started with SIGINT ignored (a background job) — Claude's cancel needs SIGINT."""
    for sig in SIGNALS:
        signal.signal(sig, signal.SIG_DFL)
    signal.pthread_sigmask(signal.SIG_UNBLOCK, SIGNALS)


def cmdline(pid: int) -> str:
    try:
        return Path(f'/proc/{pid}/cmdline').read_bytes().replace(b'\0', b' ').decode('utf-8', 'replace').strip()[:300]
    except OSError:
        return f'pid {pid}'


class Proc:
    """A child in its own process group. stdin is written without blocking and stdout/stderr are drained in the same
    loop, so no pipe can stall the recorder and every wait is bounded (M0R3-03, M0R4-06). Its environment carries a
    unique token so its descendants can be found and killed when it stops."""

    def __init__(self, argv: list[str], cwd: Path, env: dict | None = None):
        self.token = os.urandom(8).hex()
        self.out = b''
        self.err = b''
        self.sent = b''
        self.pending = bytearray()
        self.close_requested = False
        self.descendants: list[str] = []  # command lines of descendants killed by stop()
        self.lingered: list[str] = []  # descendants that outlived the child but ended on their own
        self.hidden: list[str] = []  # processes that may be descendants but whose environment is unreadable
        self.sel = selectors.DefaultSelector()
        # Signals stay blocked until the child is fully registered, so an interrupt is handled only once stop_all()
        # can find it (M0R5-01); the child unblocks them before exec.
        blocked = signal.pthread_sigmask(signal.SIG_BLOCK, SIGNALS)
        try:
            self.p = subprocess.Popen(argv, cwd=cwd, env=(env or ENV) | {TOKEN_VAR: self.token}, stdin=subprocess.PIPE,
                                      stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True,
                                      preexec_fn=child_setup)
            LIVE.add(self)
            self.started = start_time(self.p.pid) or 0
            os.set_blocking(self.p.stdin.fileno(), False)
            self.sel.register(self.p.stdout, selectors.EVENT_READ)
            self.sel.register(self.p.stderr, selectors.EVENT_READ)
        finally:
            signal.pthread_sigmask(signal.SIG_SETMASK, blocked)

    def registered(self, f) -> bool:
        return f in [k.fileobj for k in self.sel.get_map().values()]

    def reading(self) -> bool:
        return self.registered(self.p.stdout) or self.registered(self.p.stderr)

    def stdin_busy(self) -> bool:
        return not self.p.stdin.closed and bool(self.pending)

    def close_stdin(self) -> None:
        self.pending.clear()
        if not self.p.stdin.closed:
            if self.registered(self.p.stdin):
                self.sel.unregister(self.p.stdin)
            try:
                self.p.stdin.close()
            except OSError:
                pass

    def flush(self) -> None:
        """Write as much pending input as the pipe takes now; close stdin once everything requested is written."""
        while self.pending and not self.p.stdin.closed:
            try:
                n = os.write(self.p.stdin.fileno(), self.pending[:65536])
            except BlockingIOError:
                break
            except OSError:  # the CLI stopped reading (EPIPE); what it printed is still collected
                self.close_stdin()
                return
            del self.pending[:n]
        if self.p.stdin.closed:
            return
        if self.pending and not self.registered(self.p.stdin):
            self.sel.register(self.p.stdin, selectors.EVENT_WRITE)
        elif not self.pending and self.registered(self.p.stdin):
            self.sel.unregister(self.p.stdin)
        if not self.pending and self.close_requested:
            self.close_stdin()

    def write(self, data: bytes) -> None:
        self.sent += data
        if not self.p.stdin.closed and not self.close_requested:
            self.pending += data
            self.flush()

    def pump(self, timeout: float) -> bool:
        """Write pending input and read what arrives within `timeout`. Returns whether stdout or stderr is still
        open; pending input keeps being written after both closed (M0R5-04)."""
        if not self.sel.get_map():
            return False
        for key, _ in self.sel.select(timeout):
            if key.fileobj is self.p.stdin:
                self.flush()
                continue
            chunk = os.read(key.fd, 65536)
            if not chunk:
                self.sel.unregister(key.fileobj)
                key.fileobj.close()
            elif key.fileobj is self.p.stdout:
                self.out += chunk
            else:
                self.err += chunk
        return self.reading()

    def frames(self) -> list[dict]:
        return frames(self.out)

    def wait_for(self, predicate, timeout: float) -> dict | None:
        deadline = time.monotonic() + timeout
        while True:
            for f in self.frames():
                if predicate(f):
                    return f
            if time.monotonic() >= deadline or not self.pump(min(1.0, max(0.0, deadline - time.monotonic()))):
                return next((f for f in self.frames() if predicate(f)), None)

    def signal(self, sig: int) -> None:
        try:
            os.killpg(self.p.pid, sig)
        except ProcessLookupError:
            pass

    def drain(self, wait: float) -> bool:
        """Write and read until stdin is done, stdout/stderr close and the child exits, at most `wait` s."""
        deadline = time.monotonic() + wait
        while time.monotonic() < deadline:
            remaining = max(0.0, deadline - time.monotonic())
            if self.pump(min(0.5, remaining)) or self.stdin_busy():
                continue
            try:
                self.p.wait(timeout=remaining)
                return True
            except subprocess.TimeoutExpired:
                return False
        return False

    def kill_descendants(self, wait: float) -> int:
        """SIGKILL every process still carrying our token and wait until they are gone; returns how many there were."""
        pids = carrying(self.token)
        for pid in pids:
            self.descendants.append(cmdline(pid))
            try:
                os.kill(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        deadline = time.monotonic() + wait
        while carrying(self.token) and time.monotonic() < deadline:
            self.p.poll()  # reaps the child itself if it was among them
            time.sleep(0.05)
        return len(pids)

    def outlived(self, grace: float) -> None:
        """Give descendants that outlived the child up to `grace` s to end by themselves (Claude can leave the last
        `sleep` of a cancelled tool for a moment); they are recorded, and only those still running get killed."""
        deadline = time.monotonic() + grace
        running: dict[int, str] = {}
        while True:  # rescan each round: descendants born during the grace period count too (M0R6-03)
            now_running = {pid for pid in carrying(self.token) if pid != self.p.pid}
            for pid in now_running:
                running.setdefault(pid, cmdline(pid))
            for pid in [pid for pid in running if pid not in now_running]:
                self.lingered.append(running.pop(pid))
            if not running or time.monotonic() >= deadline:
                return
            time.sleep(0.05)

    def stop(self, grace: float = 20, term_grace: float = 10, kill_grace: float = 5, descendant_grace: float = 5) -> Stopped:
        """Finish writing stdin, close it and wait `grace` s for exit; then SIGTERM the group, then SIGKILL it, each
        bounded. Afterwards, on every path, processes still carrying the token are given `descendant_grace` s to end
        by themselves (`lingered`) and then killed (`+descendants-killed`); possible descendants whose environment is
        unreadable make it `+descendants-unverifiable`. Pipes still open even then are closed (`+abandoned`).
        If an exception (SIGINT, SIGTERM) interrupts this, the child is terminated before it propagates and stays
        registered until then (M0R6-01)."""
        self.close_requested = True
        try:
            result = self.stop_gracefully(grace, term_grace, kill_grace, descendant_grace)
        except BaseException:
            self.terminate()
            raise
        LIVE.discard(self)
        return result

    def stop_gracefully(self, grace: float, term_grace: float, kill_grace: float, descendant_grace: float) -> Stopped:
        how = 'exited'
        self.flush()
        finished = False
        for sig, wait in ((None, grace), (signal.SIGTERM, term_grace), (signal.SIGKILL, kill_grace)):
            if sig is not None:
                how = sig.name
                self.close_stdin()
                self.signal(sig)
            if self.drain(wait):
                finished = True
                break
        if finished:
            self.outlived(descendant_grace)
        if self.kill_descendants(kill_grace):
            how += '+descendants-killed'
        if not finished and not self.drain(kill_grace):
            self.close_pipes()
            how += '+abandoned'
            try:
                self.p.wait(timeout=kill_grace)
            except subprocess.TimeoutExpired:
                pass
        self.close_stdin()
        self.hidden = [f'{pid} {cmdline(pid)}' for pid in unverifiable(self.started)]
        if self.hidden:
            how += '+descendants-unverifiable'
            print('warning: processes that may be descendants could not be checked (environment unreadable): ' + '; '.join(self.hidden), file=sys.stderr)
        result = Stopped(how)
        result.descendants, result.lingered, result.unverifiable = list(self.descendants), list(self.lingered), list(self.hidden)
        return result

    def close_pipes(self) -> None:
        for key in list(self.sel.get_map().values()):
            self.sel.unregister(key.fileobj)
            key.fileobj.close()

    def terminate(self) -> None:
        """Kill the child, its group and its descendants now, with SIGINT/SIGTERM/SIGHUP blocked so a second signal
        cannot interrupt the cleanup; bounded (about 10 s). Unregisters the Proc when done."""
        blocked = signal.pthread_sigmask(signal.SIG_BLOCK, SIGNALS)
        try:
            self.close_stdin()
            self.signal(signal.SIGTERM)
            if not self.drain(2):
                self.signal(signal.SIGKILL)
                self.drain(3)
            self.kill_descendants(3)
            self.close_pipes()
            try:
                self.p.wait(timeout=2)
            except subprocess.TimeoutExpired:
                pass
        finally:
            LIVE.discard(self)
            signal.pthread_sigmask(signal.SIG_SETMASK, blocked)

    @property
    def returncode(self) -> int | None:
        return self.p.returncode


def stop_all() -> None:
    """Terminate every child still registered, e.g. after SIGINT/SIGTERM interrupted a case (M0R4-01). A failure to
    terminate one is reported and does not prevent terminating the others (M0R5-01)."""
    for p in list(LIVE):
        try:
            p.terminate()
        except Exception as exc:  # keep going: the remaining children and the caller's cleanup still matter
            LIVE.discard(p)
            print(f'warning: could not stop child {getattr(getattr(p, "p", None), "pid", "?")}: {type(exc).__name__}: {exc}', file=sys.stderr)


def run(argv: list[str], stdin: bytes, cwd: Path, timeout: float = 300, env: dict | None = None) -> tuple[bytes, bytes, int | None, str]:
    p = Proc(argv, cwd, env)
    p.write(stdin)
    how = p.stop(grace=timeout)
    return p.out, p.err, p.returncode, how


# --- case validators (M0R3-01) ----------------------------------------------------------------------------
# Each returns the problems found; a case is valid only with none. They see the raw frames, the exit code,
# the case's meta.json, the requests sent (JSON-RPC/RPC stdin) and identifiers from earlier cases (ctx).

def expect(out: list[str], cond: bool, msg: str) -> None:
    if not cond:
        out.append(msg)


def typed(fs: list[dict], t: str) -> list[dict]:
    return [f for f in fs if f.get('type') == t]


def claude_result(fs: list[dict], rc, out: list[str]) -> dict:
    results = typed(fs, 'result')
    expect(out, len(results) == 1, f'{len(results)} result frames, expected 1')
    return results[-1] if results else {}


def v_claude_success(fs, rc, meta, sent, ctx):
    out: list[str] = []
    r = claude_result(fs, rc, out)
    expect(out, rc == 0, f'exit code {rc}, expected 0')
    expect(out, r.get('subtype') == 'success' and r.get('is_error') is False, f'result {r.get("subtype")!r} is_error={r.get("is_error")!r}')
    expect(out, TOKEN in str(r.get('result', '')), f'assistant result lacks {TOKEN}')
    expect(out, any(f.get('type') == 'system' and f.get('subtype') == 'init' and f.get('session_id') for f in fs), 'no system:init with session_id')
    return out


def v_claude_resume(fs, rc, meta, sent, ctx):
    out = v_claude_success(fs, rc, meta, sent, ctx)
    ids = {f.get('session_id') for f in fs if f.get('type') in ('system', 'result')}
    expect(out, ids == {ctx.get('session_id')}, f'session ids {sorted(map(str, ids))}, expected only {ctx.get("session_id")}')
    return out


def v_claude_max_turns(fs, rc, meta, sent, ctx):
    out: list[str] = []
    r = claude_result(fs, rc, out)
    expect(out, r.get('subtype') == 'error_max_turns' and r.get('is_error') is True, f'result {r.get("subtype")!r} is_error={r.get("is_error")!r}')
    expect(out, rc not in (0, None), f'exit code {rc}, expected non-zero')
    return out


def v_claude_sigint(fs, rc, meta, sent, ctx):
    out: list[str] = []
    expect(out, meta.get('recorder_state') == 'SIGINT sent during foreground tool', f'recorder state {meta.get("recorder_state")!r}')
    expect(out, not meta.get('killed_by_recorder'), 'the recorder had to kill the CLI after SIGINT')
    cut = meta.get('sigint_after_frame_index') or 0
    expect(out, not typed(fs[:cut], 'result'), 'a result frame arrived before SIGINT')
    after = typed(fs[cut:], 'result')
    expect(out, bool(after), 'no result frame after SIGINT')
    expect(out, all(r.get('is_error') is True and r.get('subtype') != 'success' for r in after),
           'a result after SIGINT reports success: ' + ', '.join(str(r.get('subtype')) for r in after))
    return out


def codex_agent_text(fs: list[dict]) -> str:
    return '\n'.join(str(f['item'].get('text', '')) for f in typed(fs, 'item.completed')
                     if isinstance(f.get('item'), dict) and f['item'].get('type') == 'agent_message')


def failed_message(fs: list[dict]) -> str:
    return ' '.join(str((f.get('error') or {}).get('message', '')) for f in typed(fs, 'turn.failed'))


def v_codex_exec(fs, rc, meta, sent, ctx):
    out: list[str] = []
    expect(out, rc == 0, f'exit code {rc}, expected 0')
    expect(out, bool(typed(fs, 'turn.completed')) and not typed(fs, 'turn.failed'), 'no turn.completed, or a turn.failed')
    expect(out, TOKEN in codex_agent_text(fs), f'no agent_message containing {TOKEN}')
    return out


def v_codex_exec_failed(needle):
    """turn.failed whose message names the claimed cause: a fixed text, or a value taken from meta (M0R4-04)."""
    def check(fs, rc, meta, sent, ctx):
        out: list[str] = []
        want = needle(meta) if callable(needle) else needle
        expect(out, rc not in (0, None), f'exit code {rc}, expected non-zero')
        expect(out, bool(typed(fs, 'turn.failed')) and not typed(fs, 'turn.completed'), 'no turn.failed, or a turn.completed')
        expect(out, bool(want) and want.lower() in failed_message(fs).lower(), f'turn.failed message lacks {want!r}: {failed_message(fs)[:200]!r}')
        return out
    return check


def requested_model(meta: dict) -> str | None:
    argv = meta.get('argv', [])
    return next((argv[i + 1] for i, a in enumerate(argv[:-1]) if a in ('--model', '-m')), None)


def requests(sent: list[dict], method: str) -> list[dict]:
    return [m for m in sent if m.get('method') == method and 'id' in m]


def reply(fs: list[dict], sent: list[dict], method: str, nth: int = 0) -> dict:
    """The JSON-RPC response to the nth request of `method`, or {} when it never came."""
    reqs = requests(sent, method)
    if len(reqs) <= nth:
        return {}
    rid = reqs[nth]['id']
    return next((f for f in fs if f.get('id') == rid and ('result' in f or 'error' in f) and 'method' not in f), {})


def turns_completed(fs: list[dict], thread: str | None = None) -> list[dict]:
    return [f['params']['turn'] for f in fs if f.get('method') == 'turn/completed'
            and (thread is None or f.get('params', {}).get('threadId') == thread)]


def agent_messages(fs: list[dict]) -> str:
    # Only items the agent wrote: a userMessage item echoes the prompt, which may contain the token (M0R3-01).
    return '\n'.join(str(f['params']['item'].get('text', '')) for f in fs if f.get('method') == 'item/completed'
                     and f.get('params', {}).get('item', {}).get('type') == 'agentMessage')


def v_app_handshake(fs, rc, meta, sent, ctx):
    out: list[str] = []
    for method in ('initialize', 'thread/start'):
        expect(out, 'result' in reply(fs, sent, method), f'no result for {method}')
    expect(out, any(f.get('method') == 'thread/started' for f in fs), 'no thread/started notification')
    return out


def one_turn(fs, sent, out, status: str, thread: str | None = None) -> None:
    done = turns_completed(fs, thread)
    expect(out, len(done) == 1, f'{len(done)} turn/completed, expected 1')
    expect(out, bool(done) and done[-1].get('status') == status, f'turn status {done[-1].get("status") if done else None!r}, expected {status!r}')


def v_app_turn(fs, rc, meta, sent, ctx):
    out: list[str] = []
    expect(out, 'result' in reply(fs, sent, 'thread/start'), 'no thread/start result')
    expect(out, 'result' in reply(fs, sent, 'turn/start'), 'no turn/start result')
    one_turn(fs, sent, out, 'completed')
    expect(out, TOKEN in agent_messages(fs), f'no agentMessage containing {TOKEN}')
    return out


def v_app_resume(fs, rc, meta, sent, ctx):
    out: list[str] = []
    expect(out, 'result' in reply(fs, sent, 'thread/resume'), 'no thread/resume result')
    one_turn(fs, sent, out, 'completed', ctx.get('thread_id'))
    expect(out, TOKEN in agent_messages(fs), f'no agentMessage containing {TOKEN}')
    return out


def v_app_interrupt(fs, rc, meta, sent, ctx):
    out: list[str] = []
    expect(out, 'result' in reply(fs, sent, 'turn/interrupt'), 'no turn/interrupt result')
    one_turn(fs, sent, out, 'interrupted')
    return out


def v_app_steer(fs, rc, meta, sent, ctx):
    out: list[str] = []
    stale = reply(fs, sent, 'turn/steer', 0).get('error') or {}
    expect(out, stale.get('code') == -32600 and 'expected active turn id' in str(stale.get('message')),
           f'stale expectedTurnId was not rejected as a stale turn: {stale}')
    expect(out, 'result' in reply(fs, sent, 'turn/steer', 1), 'steer with the active turn id was not accepted')
    one_turn(fs, sent, out, 'completed')
    expect(out, 'STEERED' in agent_messages(fs), 'the agent did not follow the steer (no agentMessage containing STEERED)')
    return out


def pi_response(fs: list[dict], rid: str) -> dict:
    return next((f for f in typed(fs, 'response') if f.get('id') == rid), {})


def pi_assistant(fs: list[dict]) -> list[dict]:
    return [f['message'] for f in typed(fs, 'message_end') if isinstance(f.get('message'), dict) and f['message'].get('role') == 'assistant']


def pi_text(message: dict) -> str:
    content = message.get('content')
    return ''.join(b.get('text', '') for b in content if isinstance(b, dict)) if isinstance(content, list) else str(content or '')


def pi_turn_ok(fs, out: list[str], prompt_id: str) -> None:
    """Judged by the final answer: Pi retries failed requests itself (auto_retry_*), so an earlier error message is
    fine when a retry succeeded afterwards (M0R4-05); an exhausted retry or a final error/abort is not."""
    expect(out, pi_response(fs, prompt_id).get('success') is True, f'prompt {prompt_id} was not accepted')
    msgs = pi_assistant(fs)
    final = msgs[-1] if msgs else {}
    expect(out, final.get('stopReason') not in (None, 'error', 'aborted'), 'assistant stop reasons ' + ', '.join(str(m.get('stopReason')) for m in msgs))
    expect(out, TOKEN in pi_text(final), f'final assistant message lacks {TOKEN}')
    retries = typed(fs, 'auto_retry_end')
    expect(out, all(r.get('success') is not False for r in retries), 'a native retry was exhausted')
    if any(m.get('stopReason') == 'error' for m in msgs[:-1]):
        expect(out, any(r.get('success') is True for r in retries), 'an assistant error was not followed by a successful retry')
    settled = typed(fs, 'agent_settled')
    expect(out, len(settled) == 1 and settled[0].get('aborted') is False, f'agent_settled frames {settled}')
    expect(out, not typed(fs, 'error') and not typed(fs, 'agent_error'), 'error frames present')


def v_pi_prompt(fs, rc, meta, sent, ctx):
    out: list[str] = []
    expect(out, pi_response(fs, 's1').get('success') is True, 'get_state failed')
    pi_turn_ok(fs, out, 'p1')
    return out


def v_pi_resume(fs, rc, meta, sent, ctx):
    out: list[str] = []
    sid = pi_response(fs, 's2').get('data', {}).get('sessionId')
    expect(out, sid == ctx.get('session_id'), f'session id {sid!r}, expected {ctx.get("session_id")!r}')
    pi_turn_ok(fs, out, 'p2')
    return out


def v_pi_abort(fs, rc, meta, sent, ctx):
    out: list[str] = []
    expect(out, pi_response(fs, 'p3').get('success') is True, 'prompt p3 was not accepted')
    expect(out, pi_response(fs, 'a1').get('success') is True, 'abort was not acknowledged')
    expect(out, 'aborted' in [m.get('stopReason') for m in pi_assistant(fs)], 'no assistant message ended with stopReason aborted')
    expect(out, any(f.get('aborted') is True for f in typed(fs, 'agent_settled')), 'no agent_settled with aborted=true')
    return out


def v_pi_no_provider(fs, rc, meta, sent, ctx):
    out: list[str] = []
    expect(out, pi_response(fs, 's1').get('success') is True, 'get_state failed')
    p1 = pi_response(fs, 'p1')
    expect(out, p1.get('success') is False and 'api key' in str(p1.get('error', '')).lower(), f'prompt without a provider did not fail for a missing API key: {p1}')
    return out


def v_pi_invalid_model(fs, rc, meta, sent, ctx):
    out: list[str] = []
    m1 = pi_response(fs, 'm1')
    expect(out, m1.get('success') is False and 'model not found' in str(m1.get('error', '')).lower(), f'set_model with an unknown model did not fail as not found: {m1}')
    return out


VALIDATORS = {
    ('claude', 'success'): v_claude_success, ('claude', 'resume'): v_claude_resume,
    ('claude', 'max-turns-error'): v_claude_max_turns, ('claude', 'sigint-cancel'): v_claude_sigint,
    ('codex', 'exec'): v_codex_exec, ('codex', 'exec-unauthenticated'): v_codex_exec_failed('401'),
    ('codex', 'exec-turn-failed'): v_codex_exec_failed(requested_model),
    ('codex', 'app-server-handshake'): v_app_handshake, ('codex', 'app-server-handshake-unauthenticated'): v_app_handshake,
    ('codex', 'app-server-turn'): v_app_turn, ('codex', 'app-server-resume'): v_app_resume,
    ('codex', 'app-server-interrupt'): v_app_interrupt, ('codex', 'app-server-steer'): v_app_steer,
    ('pi', 'rpc-no-provider'): v_pi_no_provider, ('pi', 'rpc-invalid-model'): v_pi_invalid_model,
    ('pi', 'rpc-prompt'): v_pi_prompt, ('pi', 'rpc-resume'): v_pi_resume, ('pi', 'rpc-abort'): v_pi_abort,
}


def sent_messages(stdin: bytes) -> list[dict]:
    return frames(stdin)


# --- staging and atomic publication (M0R3-02) ----------------------------------------------------------

class RecordingFailed(Exception):
    pass


def exchange(a: Path, b: Path) -> None:
    """Atomically swap two directories (Linux renameat2 RENAME_EXCHANGE), so the published path never vanishes."""
    if LIBC.renameat2(-100, os.fsencode(a), -100, os.fsencode(b), 2) != 0:  # AT_FDCWD, RENAME_EXCHANGE
        err = ctypes.get_errno()
        raise OSError(err, os.strerror(err), str(a))


def recover(agent_dir: Path) -> None:
    """Undo what a run killed mid-publish or mid-recording left: restore a displaced version directory whose
    replacement never landed, drop displaced ones that were replaced, and drop abandoned staging directories."""
    for old in sorted(agent_dir.glob('.replaced-*')):
        version = old.name[len('.replaced-'):].rsplit('-', 1)[0]
        if (agent_dir / version).exists():
            shutil.rmtree(old)
        else:
            old.rename(agent_dir / version)
    for stale in agent_dir.glob('.recording-*'):
        shutil.rmtree(stale)


class Recording:
    def __init__(self, agent: str, version: str, root: Path = HERE, failed_root: Path | None = None):
        self.agent, self.version = agent, version
        self.parent = root / agent
        self.final = self.parent / version
        self.failed_root = failed_root or REPO / '.hoh-dev' / 'protocol-failed'
        self.parent.mkdir(parents=True, exist_ok=True)
        recover(self.parent)
        self.dir = Path(tempfile.mkdtemp(prefix=f'.recording-{version}-', dir=self.parent))
        self.problems: dict[str, list[str]] = {}
        self.ctx: dict = {}
        self.auth = 'UNKNOWN'
        self.published = False

    def save(self, case: str, argv: list[str], stdin: bytes, stdout: bytes, stderr: bytes, rc: int | None, notes: str,
             extra: dict | None = None, stop: str | None = None) -> list[dict]:
        d = self.dir / case
        d.mkdir()
        (d / 'stdin.txt').write_bytes(stdin)
        (d / 'stdout.jsonl').write_bytes(stdout)
        (d / 'stderr.log').write_bytes(stderr)
        meta = {'agent': self.agent, 'cli_version': self.version, 'case': case, 'argv': argv, 'cwd': 'fresh temp Git repo outside the checkout',
                'exit_code': rc, 'recorded_at': now(), 'mode': 'live', 'notes': notes, **(extra or {})}
        if stop is not None:
            meta['stopped'] = stop
        if getattr(stop, 'descendants', None):
            meta['descendants_killed'] = stop.descendants
        if getattr(stop, 'lingered', None):
            meta['descendants_lingered'] = stop.lingered  # outlived the CLI briefly, ended on their own
        if getattr(stop, 'unverifiable', None):
            meta['descendants_unverifiable'] = stop.unverifiable
        fs = frames(stdout)
        problems = VALIDATORS[(self.agent, case)](fs, rc, meta, sent_messages(stdin), self.ctx)
        if stop is not None and any(word in stop for word in ('SIGKILL', 'descendants-killed', 'descendants-unverifiable', 'abandoned')):
            problems.append(f'process cleanup: {stop}')  # a child that had to be killed or left descendants (M0R4-07, M0R5-02)
        meta |= {'valid': not problems, 'problems': problems}
        (d / 'meta.json').write_text(json.dumps(meta, ensure_ascii=False, indent=2) + '\n')
        if problems:
            self.problems[case] = problems
            print(f'{self.agent}/{case}: INVALID: ' + '; '.join(problems), file=sys.stderr)
        return fs

    def fail(self, what: str, message: str) -> None:
        self.problems.setdefault(what, []).append(message)

    def publish(self, provenance: dict) -> None:
        if self.problems:
            raise RecordingFailed('; '.join(f'{k}: {", ".join(v)}' for k, v in self.problems.items()))
        (self.dir / 'provenance.json').write_text(json.dumps(provenance, ensure_ascii=False, indent=2) + '\n')
        if not self.final.exists():
            self.dir.rename(self.final)
            self.published = True
            return
        try:
            exchange(self.dir, self.final)  # staging now holds the old version (M0R4-02)
        except OSError as exc:
            if exc.errno not in (errno.EINVAL, errno.ENOSYS, errno.EXDEV):
                raise
            # No RENAME_EXCHANGE here: two renames; recover() restores the old version if we die in between.
            old = self.parent / f'.replaced-{self.version}-{os.getpid()}'
            self.final.rename(old)
            try:
                self.dir.rename(self.final)
            except BaseException:
                old.rename(self.final)
                raise
            self.published = True
            shutil.rmtree(old)
            return
        self.published = True
        shutil.rmtree(self.dir)

    def abandon(self) -> Path:
        """Keep an unpublished recording as failure evidence; after publication only the displaced old copy remains
        here, which is removed (M0R5-05: decided by the publication itself, not by which files exist)."""
        if self.published or not self.dir.exists():
            shutil.rmtree(self.dir, ignore_errors=True)
            return self.final
        self.failed_root.mkdir(parents=True, exist_ok=True)
        dest = self.failed_root / f'{self.agent}-{self.version}-{datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S%fZ")}'
        shutil.move(str(self.dir), dest)
        return dest


def help_file(rec: Recording, name: str, argv: list[str]) -> bytes:
    data = subprocess.run(argv, capture_output=True, env=ENV, timeout=60).stdout
    (rec.dir / name).write_bytes(data)
    return data


# --- agents ---------------------------------------------------------------------------------------------

def claude(rec: Recording) -> None:
    help_file(rec, 'help.txt', ['claude', '--help'])
    base = ['claude', '-p', '--output-format', 'stream-json', '--verbose', '--dangerously-skip-permissions']
    cwd = workdir()
    prompt = f'Reply with exactly {TOKEN} and nothing else.'.encode()
    out, err, rc, how = run(base, prompt, cwd)
    fs = rec.save('success', base, prompt, out, err, rc, 'single turn', stop=how)
    sid = next((f.get('session_id') for f in fs if f.get('type') == 'system' and f.get('subtype') == 'init'), None)
    if not sid:
        rec.fail('success', 'no session id; the remaining cases need it')
        return
    rec.ctx['session_id'] = sid
    rec.auth = 'AUTHENTICATED (live turn recorded)'

    prompt = b'Repeat the exact token you replied with in the previous turn, and nothing else.'
    out, err, rc, how = run(base + ['--resume', sid], prompt, cwd)
    rec.save('resume', base + ['--resume', '<session_id of success>'], prompt, out, err, rc, 'second turn via --resume',
             {'resumed_session_id': sid}, stop=how)

    argv = base + ['--max-turns', '1']
    prompt = b'Use the Bash tool to run `echo hoh` and then reply DONE.'
    out, err, rc, how = run(argv, prompt, workdir())
    rec.save('max-turns-error', argv, prompt, out, err, rc, 'tool use needs a second turn', stop=how)
    claude_cancel(rec, base)


def claude_cancel(rec: Recording, base: list[str]) -> None:
    """SIGINT while a foreground tool call is still running; valid only if no result frame preceded it."""
    prompt = (b'Run this exact command with the Bash tool in the foreground (not in the background) and wait for it '
              b'to finish: `for i in $(seq 1 20); do echo tick $i; sleep 2; done`. Then reply DONE.')
    p = Proc(base, workdir())
    p.write(prompt)
    p.p.stdin.close()
    tool_id = None
    armed = 0.0
    sigint_after_frame = None
    state = 'waiting for tool_use'
    deadline = time.monotonic() + 240
    while time.monotonic() < deadline and p.pump(1.0):
        fs = p.frames()
        if tool_id is None:
            for f in typed(fs, 'assistant'):
                for c in f.get('message', {}).get('content', []):
                    if c.get('type') == 'tool_use' and c.get('name') == 'Bash' and not c.get('input', {}).get('run_in_background'):
                        tool_id, armed = c['id'], time.monotonic()
            continue
        finished = bool(typed(fs, 'result')) or any(
            c.get('type') == 'tool_result' and c.get('tool_use_id') == tool_id
            for f in typed(fs, 'user') for c in (f.get('message', {}).get('content') or []) if isinstance(c, dict))
        if finished:
            state = 'tool finished before SIGINT'
            break
        if time.monotonic() - armed >= 4:
            sigint_after_frame = len(fs)
            p.signal(signal.SIGINT)
            state = 'SIGINT sent during foreground tool'
            break
    how = p.stop(grace=60)
    after = p.frames()[sigint_after_frame or 0:]
    rec.save('sigint-cancel', base, prompt, p.out, p.err, p.returncode,
             'SIGINT to the process group 4 s after a foreground Bash tool_use frame, before its tool_result',
             {'sigint_after_frame_index': sigint_after_frame, 'recorder_state': state, 'killed_by_recorder': how != 'exited',
              'frames_after_sigint': [f.get('type', '') + (':' + f['subtype'] if f.get('subtype') else '') for f in after]}, stop=how)


def codex(rec: Recording) -> None:
    subprocess.run(['codex', 'app-server', 'generate-json-schema', '--out', str(rec.dir / 'app-server-schema')], check=True,
                   capture_output=True, env=ENV, timeout=120)
    help_file(rec, 'help-exec.txt', ['codex', 'exec', '--help'])
    help_file(rec, 'help-app-server.txt', ['codex', 'app-server', '--help'])
    status = subprocess.run(['codex', 'login', 'status'], capture_output=True, text=True, env=ENV, timeout=60)
    authed = status.returncode == 0
    rec.auth = 'AUTHENTICATED' if authed else 'BLOCKED_AUTH'
    (rec.dir / 'auth-status.json').write_text(json.dumps({'checked_at': now(), 'command': ['codex', 'login', 'status'], 'exit_code': status.returncode,
                                                          'output': (status.stdout + status.stderr).strip(), 'state': rec.auth}, indent=2) + '\n')
    argv = ['codex', 'exec', '--json', '--dangerously-bypass-approvals-and-sandbox', '-']
    prompt = f'Reply with exactly {TOKEN} and nothing else.'.encode()
    # The unauthenticated surfaces are reproducible with an empty CODEX_HOME, whatever the operator's login.
    unauth = ENV | {'CODEX_HOME': str(tempdir('hoh-codex-home-'))}
    out, err, rc, how = run(argv, prompt, workdir(), env=unauth)
    rec.save('exec-unauthenticated', argv, prompt, out, err, rc, 'exec with an empty CODEX_HOME: the authentication failure surface',
             {'env_overrides': ['CODEX_HOME=<empty temp dir>']}, stop=how)
    codex_handshake(rec, 'app-server-handshake-unauthenticated', unauth, 'empty CODEX_HOME: ')
    if not authed:
        rec.fail('auth', 'BLOCKED_AUTH: codex login status failed; the authenticated cases cannot be recorded')
        return
    bad = argv[:-1] + ['--model', 'hoh-invalid-model', '-']
    out, err, rc, how = run(bad, prompt, workdir())
    rec.save('exec-turn-failed', bad, prompt, out, err, rc, 'logged in, model unavailable to the account: the MODEL_UNAVAILABLE surface', stop=how)
    out, err, rc, how = run(argv, prompt, workdir())
    rec.save('exec', argv, prompt, out, err, rc, 'one-shot exec turn', stop=how)
    codex_handshake(rec, 'app-server-handshake', ENV, '')
    codex_app_server(rec)


def codex_handshake(rec: Recording, case: str, env: dict, note: str) -> None:
    msgs = [{'method': 'initialize', 'id': 0, 'params': {'clientInfo': {'name': 'hoh-recorder', 'version': '0.0.0'}}},
            {'method': 'initialized'},
            {'method': 'thread/start', 'id': 1, 'params': {'approvalPolicy': 'never', 'sandbox': 'danger-full-access'}}]
    p = Proc(['codex', 'app-server'], workdir(), env)
    p.write(b''.join(json.dumps(m).encode() + b'\n' for m in msgs))
    end = time.monotonic() + 10
    while time.monotonic() < end and p.pump(end - time.monotonic()):
        pass
    how = p.stop(grace=20)
    rec.save(case, ['codex', 'app-server'], p.sent, p.out, p.err, p.returncode, note + 'initialize, initialized, thread/start; stdin closed after 10 s',
             {'env_overrides': ['CODEX_HOME=<empty temp dir>']} if env is not ENV else None, stop=how)


class AppServer:
    """Minimal JSON-RPC driver for `codex app-server` that keeps every byte sent and received."""

    def __init__(self, cwd: Path):
        self.p = Proc(['codex', 'app-server'], cwd)
        self.next_id = 0

    def send(self, method: str, params: dict | None = None, notify: bool = False) -> int | None:
        msg: dict = {'method': method}
        rid = None
        if not notify:
            rid = self.next_id
            self.next_id += 1
            msg['id'] = rid
        if params is not None:
            msg['params'] = params
        self.p.write(json.dumps(msg).encode() + b'\n')
        return rid

    def wait(self, predicate, timeout: float = 180) -> dict | None:
        return self.p.wait_for(predicate, timeout)

    def response(self, rid: int | None, timeout: float = 60) -> dict:
        return self.wait(lambda f: f.get('id') == rid and ('result' in f or 'error' in f), timeout) or {}

    def start(self) -> None:
        self.response(self.send('initialize', {'clientInfo': {'name': 'hoh-recorder', 'version': '0.0.0'}}))
        self.send('initialized', notify=True)

    def thread(self, params: dict) -> str | None:
        return self.response(self.send('thread/start', params)).get('result', {}).get('thread', {}).get('id')

    def save(self, rec: Recording, case: str, notes: str, extra: dict | None = None) -> list[dict]:
        how = self.p.stop(grace=20)
        return rec.save(case, ['codex', 'app-server'], self.p.sent, self.p.out, self.p.err, self.p.returncode, notes, extra, stop=how)


def text(t: str) -> list[dict]:
    return [{'type': 'text', 'text': t}]


def turn_done(thread_id: str | None):
    return lambda f: f.get('method') == 'turn/completed' and f.get('params', {}).get('threadId') == thread_id


def codex_app_server(rec: Recording) -> None:
    """Authenticated App Server cases: turn, resume, interrupt, steer (stale + valid expectedTurnId)."""
    YOLO = {'approvalPolicy': 'never', 'sandbox': 'danger-full-access'}
    cwd = workdir()
    s = AppServer(cwd)
    s.start()
    thread = s.thread(YOLO)
    s.send('turn/start', {'threadId': thread, 'input': text(f'Reply with exactly {TOKEN} and nothing else.')})
    s.wait(turn_done(thread))
    s.save(rec, 'app-server-turn', 'thread/start then one turn/start until turn/completed', {'thread_id': thread})
    rec.ctx['thread_id'] = thread

    s = AppServer(cwd)
    s.start()
    s.response(s.send('thread/resume', {'threadId': thread, **YOLO}))
    s.send('turn/start', {'threadId': thread, 'input': text('Repeat the exact token you replied with in the previous turn, and nothing else.')})
    s.wait(turn_done(thread))
    s.save(rec, 'app-server-resume', 'new app-server process: thread/resume of app-server-turn, then a turn that must recall the token',
           {'resumed_thread_id': thread})

    long_task = text('Run this exact shell command and wait for it to finish: for i in $(seq 1 30); do echo tick $i; sleep 2; done. Then reply DONE.')
    s = AppServer(workdir())
    s.start()
    thread = s.thread(YOLO)
    s.send('turn/start', {'threadId': thread, 'input': long_task})
    started = s.wait(lambda f: f.get('method') == 'turn/started') or {}
    s.wait(lambda f: f.get('method') == 'item/started' and f.get('params', {}).get('item', {}).get('type') == 'commandExecution', timeout=120)
    turn_id = started.get('params', {}).get('turn', {}).get('id', 'missing')
    s.response(s.send('turn/interrupt', {'threadId': thread, 'turnId': turn_id}))
    s.wait(turn_done(thread))
    s.save(rec, 'app-server-interrupt', 'turn/interrupt while a command execution item runs')

    s = AppServer(workdir())
    s.start()
    thread = s.thread(YOLO)
    s.send('turn/start', {'threadId': thread, 'input': long_task})
    started = s.wait(lambda f: f.get('method') == 'turn/started') or {}
    turn_id = started.get('params', {}).get('turn', {}).get('id', 'missing')
    s.response(s.send('turn/steer', {'threadId': thread, 'expectedTurnId': 'hoh-stale-turn-id', 'input': text('Stop and reply STEERED.')}))
    s.response(s.send('turn/steer', {'threadId': thread, 'expectedTurnId': turn_id, 'input': text('Stop running the command now and reply exactly STEERED.')}))
    s.wait(turn_done(thread))
    s.save(rec, 'app-server-steer', 'turn/steer with a stale expectedTurnId (must be rejected) and with the active turn id')


def pi_session(argv: list[str], cwd: Path, msgs: list[dict], until: str, abort_on: str | None = None, timeout: float = 120,
               env: dict | None = None, settle: float = 0) -> tuple[Proc, str]:
    """Drive one `pi --mode rpc` process: send msgs, optionally abort on the first `abort_on` frame, stop once
    `until` frames have arrived for every request (responses) or once (events), then wait `settle` seconds."""
    p = Proc(argv, cwd, env)
    want = len(msgs) if until == 'response' else 1
    p.write(b''.join(json.dumps(m).encode() + b'\n' for m in msgs))
    aborted = False
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline and p.pump(1.0):
        fs = p.frames()
        if abort_on and not aborted and typed(fs, abort_on):
            p.write(json.dumps({'id': 'a1', 'type': 'abort'}).encode() + b'\n')
            aborted = True
        if len(typed(fs, until)) >= want:
            break
    end = time.monotonic() + settle
    while time.monotonic() < end and p.pump(end - time.monotonic()):
        pass
    return p, p.stop(grace=20)


def pi(rec: Recording) -> None:
    help_file(rec, 'help.txt', ['pi', '--help'])
    cwd = workdir()
    argv = ['pi', '--mode', 'rpc', '--session-dir', str(cwd / 'pi-sessions')]
    shown = ['pi', '--mode', 'rpc', '--session-dir', '<temp>/pi-sessions']
    prompt = f'Reply with exactly {TOKEN} and nothing else.'
    # Failure without any provider, reproducible regardless of the operator's setup: empty agent dir, no key variables.
    bare = {k: v for k, v in ENV.items() if not k.endswith(('_API_KEY', '_AUTH_TOKEN', '_OAUTH_TOKEN'))}
    bare |= {'PI_CODING_AGENT_DIR': str(cwd / 'empty-agent-dir'), 'PI_OFFLINE': '1'}
    p, how = pi_session(argv, cwd, [{'id': 's1', 'type': 'get_state'}, {'id': 'p1', 'type': 'prompt', 'message': prompt}],
                        until='response', env=bare, settle=8)
    rec.save('rpc-no-provider', shown, p.sent, p.out, p.err, p.returncode,
             'empty PI_CODING_AGENT_DIR, no provider key variables, PI_OFFLINE=1: get_state then prompt',
             {'env_overrides': ['PI_CODING_AGENT_DIR=<temp>/empty-agent-dir', 'PI_OFFLINE=1', 'provider key variables removed']}, stop=how)
    p, how = pi_session(argv, cwd, [{'id': 'm1', 'type': 'set_model', 'provider': 'hoh-invalid-provider', 'modelId': 'hoh-invalid-model'}],
                        until='response', settle=5)
    rec.save('rpc-invalid-model', shown, p.sent, p.out, p.err, p.returncode, 'set_model with an unknown provider/model', stop=how)
    p, how = pi_session(argv, cwd, [{'id': 's1', 'type': 'get_state'}, {'id': 'p1', 'type': 'prompt', 'message': prompt}], until='agent_settled')
    fs = p.frames()
    if pi_response(fs, 'p1').get('success') is False:
        rec.auth = 'BLOCKED_AUTH'
        rec.fail('auth', 'BLOCKED_AUTH: no provider configured; the live cases cannot be recorded')
        return
    rec.save('rpc-prompt', shown, p.sent, p.out, p.err, p.returncode, 'get_state then one prompt until agent_settled', stop=how)
    session_id = pi_response(fs, 's1').get('data', {}).get('sessionId')
    if not session_id:
        rec.fail('rpc-prompt', 'get_state returned no sessionId; the remaining cases need it')
        return
    rec.ctx['session_id'] = session_id
    rec.auth = 'AUTHENTICATED (live turn recorded)'
    argv2 = argv + ['--session-id', session_id]
    shown2 = shown + ['--session-id', '<session id of rpc-prompt>']
    p, how = pi_session(argv2, cwd, [{'id': 's2', 'type': 'get_state'},
                                     {'id': 'p2', 'type': 'prompt', 'message': 'Repeat the exact token you replied with in the previous turn, and nothing else.'}],
                        until='agent_settled')
    rec.save('rpc-resume', shown2, p.sent, p.out, p.err, p.returncode, 'new process with the same --session-dir and --session-id',
             {'resumed_session_id': session_id}, stop=how)
    p, how = pi_session(argv2, cwd, [{'id': 'p3', 'type': 'prompt', 'message': 'Count from 1 to 400, one number per line, with no other text.'}],
                        until='agent_settled', abort_on='message_update')
    rec.save('rpc-abort', shown2, p.sent, p.out, p.err, p.returncode, 'abort command sent on the first message_update',
             {'assistant_stop_reasons': [m.get('stopReason') for m in pi_assistant(p.frames())]}, stop=how)


# --- provenance and manifest ------------------------------------------------------------------------------

def tree_sha256(root: Path) -> str:
    """sha256 over the sorted lines '<relative path> <sha256 of file>\\n' of every file under root."""
    lines = sorted(f'{f.relative_to(root).as_posix()} {sha256(f.read_bytes())}\n' for f in root.rglob('*') if f.is_file())
    return sha256(''.join(lines).encode())


def binaries(name: str) -> dict:
    wrapper = Path(shutil.which(name) or '').resolve()
    info = {'entry_path': str(wrapper), 'entry_sha256': sha256(wrapper.read_bytes()) if wrapper.is_file() else None}
    if name == 'codex':  # the npm entry is a JS wrapper; the protocol is implemented by the vendored native binary
        native = sorted(wrapper.parent.parent.glob('node_modules/@openai/codex-*/vendor/*/bin/codex'))
        if native:
            info |= {'native_path': str(native[0]), 'native_sha256': sha256(native[0].read_bytes())}
    if name == 'pi':  # the entry is a shell launcher; hash the release package that implements Pi
        pkg = sorted(wrapper.parent.parent.glob('install/releases/*/node_modules/@earendil-works/pi-coding-agent'))
        version = first_line(['pi', '--version'])
        pkg = [d for d in pkg if d.parts[-4] == version] or pkg
        if pkg:
            files = [f for f in pkg[0].rglob('*') if f.is_file() and 'node_modules' not in f.relative_to(pkg[0]).parts]
            lines = sorted(f'{f.relative_to(pkg[0]).as_posix()} {sha256(f.read_bytes())}\n' for f in files)
            info |= {'package_path': str(pkg[0]), 'package_tree_sha256': sha256(''.join(lines).encode())}
    return info


INSTALLED_VERSION = {'claude': lambda: first_line(['claude', '--version']).split()[0],
                     'codex': lambda: first_line(['codex', '--version']).split()[-1],
                     'pi': lambda: first_line(['pi', '--version'])}
HELP = {'claude': ('help.txt', ['claude', '--help']), 'codex': ('help-exec.txt', ['codex', 'exec', '--help']), 'pi': ('help.txt', ['pi', '--help'])}
AGENTS = {'claude': claude, 'codex': codex, 'pi': pi}


def record(agent: str, root: Path = HERE) -> Path:
    """Record one agent into staging and publish it; raises RecordingFailed and leaves the published fixtures alone otherwise."""
    previous = install_termination_handlers()  # SIGTERM/SIGHUP raise Terminated so children are stopped (M0R4-01)
    try:
        version = INSTALLED_VERSION[agent]()
        before = binaries(agent)
        started = now()
        rec = Recording(agent, version, root)
    except BaseException:
        restore_handlers(previous)
        raise
    try:
        AGENTS[agent](rec)
        if INSTALLED_VERSION[agent]() != version or binaries(agent) != before:
            rec.fail('provenance', 'the installed binary changed during the recording')
        name, argv = HELP[agent]
        help_bytes = (rec.dir / name).read_bytes()
        rec.publish({'cli_version': version, 'auth': rec.auth, 'recording_started_at': started, 'recorded_at': now(),
                     'help_argv': argv, 'help_sha256': sha256(help_bytes), **before})
        return rec.final
    except BaseException as exc:
        stop_all()
        where = rec.abandon()
        if isinstance(exc, (KeyboardInterrupt, SystemExit, Terminated)):
            raise
        detail = str(exc) if isinstance(exc, RecordingFailed) else f'{type(exc).__name__}: {exc}'
        raise RecordingFailed(f'{agent} {version}: {detail}; published fixtures unchanged; staging kept at {where}') from exc
    finally:
        try:
            stop_all()  # children first, then their directories
        finally:
            restore_handlers(previous)
            while TEMP_DIRS:
                shutil.rmtree(TEMP_DIRS.pop(), ignore_errors=True)


def write_manifest(root: Path = HERE) -> None:
    """One entry per published (agent, version), built from what is on disk: provenance.json written when that
    directory was recorded, or for older directories without one, the provenance the previous manifest recorded.
    Nothing is re-measured here, so an entry never pairs old fixtures with a newer binary (M0R3-02)."""
    path = root / 'compat-manifest.json'
    try:
        previous = json.loads(path.read_text()).get('agents', {}) if path.exists() else {}
    except ValueError:  # truncated by an interrupted write before writes became atomic (M0R4-03)
        print(f'warning: {path} is not valid JSON; rebuilding it from provenance.json files only', file=sys.stderr)
        previous = {}
    for agent_dir in (p for p in root.iterdir() if p.is_dir() and not p.name.startswith(('_', '.'))):
        recover(agent_dir)  # an interrupted publish must not hide its version (M0R4-02)
    agents: dict[str, dict] = {}
    for agent_dir in sorted(p for p in root.iterdir() if p.is_dir() and not p.name.startswith(('_', '.'))):
        name = agent_dir.name
        for version_dir in sorted(p for p in agent_dir.iterdir() if p.is_dir() and not p.name.startswith('.')):
            cases = {}
            for meta in sorted(version_dir.glob('*/meta.json')):
                m = json.loads(meta.read_text())
                cases[meta.parent.name] = {'valid': m['valid'], 'files': {f.name: sha256(f.read_bytes()) for f in sorted(meta.parent.iterdir())}}
            prov_file = version_dir / 'provenance.json'
            if prov_file.exists():
                prov = json.loads(prov_file.read_text())
            else:
                prov = {k: v for k, v in previous.get(name, {}).get(version_dir.name, {}).items() if k not in ('cases', 'app_server_schema_tree_sha256')}
                prov = prov or {'cli_version': version_dir.name, 'auth': 'UNKNOWN', 'binary': 'not recorded'}
            entry = {**prov, 'cli_version': version_dir.name, 'cases': cases}
            if (version_dir / 'app-server-schema').is_dir():
                entry['app_server_schema_tree_sha256'] = tree_sha256(version_dir / 'app-server-schema')
            agents.setdefault(name, {})[version_dir.name] = entry
    manifest = {'schema_version': '1.3', 'recorded_at': now(), 'recorder': 'tests/fixtures/protocol/record.py',
                'hash_algorithms': {'tree_sha256': tree_sha256.__doc__, 'files': 'sha256 of the raw file bytes'}, 'agents': agents}
    tmp = path.with_name(f'.{path.name}.{os.getpid()}.tmp')
    tmp.write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + '\n')
    os.replace(tmp, path)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('--agents', default='claude,codex,pi')
    selected = ap.parse_args().agents.split(',')
    unknown = set(selected) - set(AGENTS)
    if unknown:
        ap.error('unknown agents: ' + ', '.join(sorted(unknown)))
    failures = []
    previous = install_termination_handlers()  # the whole run, including between agents (M0R6-04)
    try:
        for name in AGENTS:
            if name in selected:
                try:
                    print(f'recorded {name}: {record(name, HERE)}')
                except (KeyboardInterrupt, Terminated) as exc:
                    print(f'interrupted while recording {name} ({type(exc).__name__} {exc}); its published fixtures are unchanged', file=sys.stderr)
                    return 128 + signal.Signals[str(exc)] if isinstance(exc, Terminated) else 130
                except RecordingFailed as exc:
                    failures.append(str(exc))
                    print(f'NOT recorded: {exc}', file=sys.stderr)
    finally:
        try:
            write_manifest(HERE)  # also after an interrupt: agents published earlier in this run must be listed (M0R5-03)
            print('wrote compat-manifest.json')
        finally:
            restore_handlers(previous)
    return 1 if failures else 0


if __name__ == '__main__':
    sys.exit(main())
