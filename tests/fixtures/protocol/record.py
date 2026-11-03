#!/usr/bin/env python3
"""Re-record native protocol fixtures (M0-T04). Live: costs model calls for authenticated agents.

Usage: python3 tests/fixtures/protocol/record.py [--agents claude,codex,pi]

Every case runs in a fresh temporary Git repository OUTSIDE this checkout (no project CLAUDE.md or
AGENTS.md leaks in) with the parent agent session variables removed. The operator's user-level agent
configuration (plugins, hooks, auth) still applies and is recorded as-is; it cannot be isolated
without copying credentials. Each case stores meta.json (argv, prompt, exit code, validity), the
exact stdin, raw stdout.jsonl and stderr.log. Cases that do not show what they claim fail the run.
"""
from __future__ import annotations
import argparse, json, os, select, shutil, signal, subprocess, sys, tempfile, time
from datetime import datetime, timezone
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parents[2] / 'tools'))
from agent_task import PARENT_SESSION_VARS  # noqa: E402  (single source of the stripped variable list)

ENV = {k: v for k, v in os.environ.items() if k not in PARENT_SESSION_VARS}


def now() -> str:
    return datetime.now(timezone.utc).isoformat()


def first_line(argv: list[str]) -> str:
    out = subprocess.run(argv, capture_output=True, text=True, env=ENV).stdout.strip().splitlines()
    return out[0] if out else 'unknown'


def workdir() -> Path:
    d = Path(tempfile.mkdtemp(prefix='hoh-protocol-'))
    subprocess.run(['git', 'init', '-q', str(d)], check=True)
    return d


def save(agent: str, version: str, case: str, argv: list[str], stdin: bytes, stdout: bytes, stderr: bytes,
         rc: int | None, notes: str, valid: bool, extra: dict | None = None) -> None:
    d = HERE / agent / version / case
    shutil.rmtree(d, ignore_errors=True)
    d.mkdir(parents=True)
    (d / 'stdin.txt').write_bytes(stdin)
    (d / 'stdout.jsonl').write_bytes(stdout)
    (d / 'stderr.log').write_bytes(stderr)
    meta = {'agent': agent, 'cli_version': version, 'case': case, 'argv': argv, 'cwd': 'fresh temp Git repo outside the checkout',
            'exit_code': rc, 'recorded_at': now(), 'mode': 'live', 'valid': valid, 'notes': notes, **(extra or {})}
    (d / 'meta.json').write_text(json.dumps(meta, ensure_ascii=False, indent=2) + '\n')
    if not valid:
        raise SystemExit(f'{agent}/{case}: recorded output does not show what the case claims; see {d}')


def frames(raw: bytes) -> list[dict]:
    out = []
    for line in raw.splitlines():
        if line.strip():
            try:
                out.append(json.loads(line))
            except ValueError:
                pass
    return out


def run(argv: list[str], stdin: bytes, cwd: Path, timeout: float = 300) -> tuple[bytes, bytes, int]:
    p = subprocess.run(argv, input=stdin, capture_output=True, cwd=cwd, env=ENV, timeout=timeout)
    return p.stdout, p.stderr, p.returncode


def claude() -> None:
    version = first_line(['claude', '--version']).split()[0]
    (HERE / 'claude' / version).mkdir(parents=True, exist_ok=True)
    (HERE / 'claude' / version / 'help.txt').write_text(subprocess.run(['claude', '--help'], capture_output=True, text=True, env=ENV).stdout)
    base = ['claude', '-p', '--output-format', 'stream-json', '--verbose', '--dangerously-skip-permissions']
    cwd = workdir()
    prompt = b'Reply with exactly HOH_SAMPLE_OK and nothing else.'
    out, err, rc = run(base, prompt, cwd)
    fs = frames(out)
    result = [f for f in fs if f.get('type') == 'result']
    save('claude', version, 'success', base, prompt, out, err, rc, 'single turn',
         valid=rc == 0 and len(result) == 1 and result[0].get('subtype') == 'success')
    sid = next(f['session_id'] for f in fs if f.get('type') == 'system' and f.get('subtype') == 'init')

    argv = base + ['--resume', sid]
    prompt = b'Repeat the exact token you replied with in the previous turn, and nothing else.'
    out, err, rc = run(argv, prompt, cwd)
    fs = frames(out)
    save('claude', version, 'resume', base + ['--resume', '<session_id of success>'], prompt, out, err, rc, 'second turn via --resume',
         valid=rc == 0 and any(f.get('type') == 'result' and 'HOH_SAMPLE_OK' in str(f.get('result')) for f in fs)
         and all(f.get('session_id', sid) == sid for f in fs if f.get('type') in ('system', 'result')))

    argv = base + ['--max-turns', '1']
    prompt = b'Use the Bash tool to run `echo hoh` and then reply DONE.'
    out, err, rc = run(argv, prompt, workdir())
    fs = frames(out)
    save('claude', version, 'max-turns-error', argv, prompt, out, err, rc, 'tool use needs a second turn',
         valid=any(f.get('type') == 'result' and f.get('subtype') == 'error_max_turns' for f in fs))

    claude_cancel(version, base)


def claude_cancel(version: str, base: list[str]) -> None:
    """SIGINT while a foreground tool call is still running; valid only if no result frame preceded it."""
    prompt = (b'Run this exact command with the Bash tool in the foreground (not in the background) and wait for it '
              b'to finish: `for i in $(seq 1 20); do echo tick $i; sleep 2; done`. Then reply DONE.')
    p = subprocess.Popen(base, cwd=workdir(), env=ENV, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                         start_new_session=True)
    p.stdin.write(prompt)
    p.stdin.close()
    buf = b''
    tool_id = None
    sigint_after_frame = None
    state = 'waiting for tool_use'
    deadline = time.monotonic() + 240
    while time.monotonic() < deadline:
        ready, _, _ = select.select([p.stdout], [], [], 1.0)
        if ready:
            chunk = os.read(p.stdout.fileno(), 65536)
            if not chunk:
                break
            buf += chunk
        fs = frames(buf)
        if tool_id is None:
            for f in fs:
                if f.get('type') == 'assistant':
                    for c in f.get('message', {}).get('content', []):
                        if c.get('type') == 'tool_use' and c.get('name') == 'Bash' and not c.get('input', {}).get('run_in_background'):
                            tool_id, armed = c['id'], time.monotonic()
            continue
        finished = any(f.get('type') == 'result' for f in fs) or any(
            c.get('type') == 'tool_result' and c.get('tool_use_id') == tool_id
            for f in fs if f.get('type') == 'user' for c in (f.get('message', {}).get('content') or []) if isinstance(c, dict))
        if finished:
            state = 'tool finished before SIGINT'
            break
        if time.monotonic() - armed >= 4:
            sigint_after_frame = len(fs)
            os.killpg(p.pid, signal.SIGINT)
            state = 'SIGINT sent during foreground tool'
            break
    try:
        rest, err = p.communicate(timeout=60)
    except subprocess.TimeoutExpired:
        os.killpg(p.pid, signal.SIGKILL)
        rest, err = p.communicate()
    out = buf + rest
    after = frames(out)[sigint_after_frame or 0:]
    valid = state == 'SIGINT sent during foreground tool'
    save('claude', version, 'sigint-cancel', base, prompt, out, err, p.returncode,
         'SIGINT to the process group 4 s after a foreground Bash tool_use frame, before its tool_result', valid,
         {'sigint_after_frame_index': sigint_after_frame, 'recorder_state': state,
          'frames_after_sigint': [f.get('type') + (':' + f['subtype'] if f.get('subtype') else '') for f in after]})


def codex() -> None:
    version = first_line(['codex', '--version']).split()[-1]
    d = HERE / 'codex' / version
    d.mkdir(parents=True, exist_ok=True)
    subprocess.run(['codex', 'app-server', 'generate-json-schema', '--out', str(d / 'app-server-schema')], check=True, capture_output=True, env=ENV)
    for name, argv in [('help-exec.txt', ['codex', 'exec', '--help']), ('help-app-server.txt', ['codex', 'app-server', '--help'])]:
        (d / name).write_text(subprocess.run(argv, capture_output=True, text=True, env=ENV).stdout)
    status = subprocess.run(['codex', 'login', 'status'], capture_output=True, text=True, env=ENV)
    authed = status.returncode == 0
    (d / 'auth-status.json').write_text(json.dumps({'checked_at': now(), 'command': ['codex', 'login', 'status'], 'exit_code': status.returncode,
                                                    'output': (status.stdout + status.stderr).strip(),
                                                    'state': 'AUTHENTICATED' if authed else 'BLOCKED_AUTH'}, indent=2) + '\n')
    suffix = '' if authed else '-unauthenticated'
    argv = ['codex', 'exec', '--json', '--dangerously-bypass-approvals-and-sandbox', '-']
    prompt = b'Reply with exactly HOH_SAMPLE_OK and nothing else.'
    out, err, rc = run(argv, prompt, workdir())
    save('codex', version, 'exec' + suffix, argv, prompt, out, err, rc,
         'one-shot exec turn' if authed else 'exec without login: records the authentication failure surface', valid=True)
    msgs = [{'method': 'initialize', 'id': 0, 'params': {'clientInfo': {'name': 'hoh-recorder', 'version': '0.0.0'}}},
            {'method': 'initialized'},
            {'method': 'thread/start', 'id': 1, 'params': {'approvalPolicy': 'never', 'sandbox': 'danger-full-access'}}]
    stdin = b''.join(json.dumps(m).encode() + b'\n' for m in msgs)
    p = subprocess.Popen(['codex', 'app-server'], cwd=workdir(), env=ENV, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                         start_new_session=True)
    p.stdin.write(stdin)
    p.stdin.flush()
    time.sleep(10)
    p.stdin.close()
    try:
        out, err = p.communicate(timeout=20)
    except subprocess.TimeoutExpired:
        os.killpg(p.pid, signal.SIGTERM)
        out, err = p.communicate()
    fs = frames(out)
    save('codex', version, 'app-server-handshake' + suffix, ['codex', 'app-server'], stdin, out, err, p.returncode,
         'initialize, initialized, thread/start; stdin closed after 10 s', valid=any(f.get('id') == 0 and 'result' in f for f in fs))


def pi_session(argv: list[str], cwd: Path, msgs: list[dict], until: str, abort_on: str | None = None, timeout: float = 120) -> tuple[bytes, bytes, int | None, bytes]:
    """Drive one `pi --mode rpc` process: send msgs, optionally send abort on the first `abort_on` frame, stop after `until`."""
    p = subprocess.Popen(argv, cwd=cwd, env=ENV, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True)
    sent = b''.join(json.dumps(m).encode() + b'\n' for m in msgs)
    p.stdin.write(sent)
    p.stdin.flush()
    buf = b''
    aborted = False
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        ready, _, _ = select.select([p.stdout], [], [], 1.0)
        if ready:
            chunk = os.read(p.stdout.fileno(), 65536)
            if not chunk:
                break
            buf += chunk
        fs = frames(buf)
        if abort_on and not aborted and any(f.get('type') == abort_on for f in fs):
            extra = json.dumps({'id': 'a1', 'type': 'abort'}).encode() + b'\n'
            p.stdin.write(extra)
            p.stdin.flush()
            sent += extra
            aborted = True
        if any(f.get('type') == until for f in fs):
            break
    p.stdin.close()
    try:
        rest, err = p.communicate(timeout=20)
    except subprocess.TimeoutExpired:
        os.killpg(p.pid, signal.SIGTERM)
        rest, err = p.communicate()
    return buf + rest, err, p.returncode, sent


def pi() -> None:
    version = first_line(['pi', '--version'])
    d = HERE / 'pi' / version
    d.mkdir(parents=True, exist_ok=True)
    (d / 'help.txt').write_text(subprocess.run(['pi', '--help'], capture_output=True, text=True, env=ENV).stdout)
    cwd = workdir()
    argv = ['pi', '--mode', 'rpc', '--session-dir', str(cwd / 'pi-sessions')]
    shown = ['pi', '--mode', 'rpc', '--session-dir', '<temp>/pi-sessions']
    msgs = [{'id': 's1', 'type': 'get_state'}, {'id': 'p1', 'type': 'prompt', 'message': 'Reply with exactly HOH_SAMPLE_OK and nothing else.'}]
    out, err, rc, sent = pi_session(argv, cwd, msgs, until='agent_settled')
    fs = frames(out)
    if any(f.get('id') == 'p1' and f.get('success') is False for f in fs):
        save('pi', version, 'rpc-no-provider', shown, sent, out, err, rc, 'get_state then prompt without a configured provider',
             valid=any(f.get('id') == 's1' and f.get('success') for f in fs))
        return
    save('pi', version, 'rpc-prompt', shown, sent, out, err, rc, 'get_state then one prompt until agent_settled',
         valid=any(f.get('type') == 'agent_settled' for f in fs))
    session_id = next(f['data']['sessionId'] for f in fs if f.get('id') == 's1')
    argv2 = argv + ['--session-id', session_id]
    msgs = [{'id': 's2', 'type': 'get_state'}, {'id': 'p2', 'type': 'prompt', 'message': 'Repeat the exact token you replied with in the previous turn, and nothing else.'}]
    out, err, rc, sent = pi_session(argv2, cwd, msgs, until='agent_settled')
    fs = frames(out)
    same = any(f.get('id') == 's2' and f.get('data', {}).get('sessionId') == session_id for f in fs)
    said = any(f.get('type') == 'message_end' and f.get('message', {}).get('role') == 'assistant' and 'HOH_SAMPLE_OK' in json.dumps(f) for f in fs)
    save('pi', version, 'rpc-resume', shown + ['--session-id', '<session id of rpc-prompt>'], sent, out, err, rc,
         'new process with the same --session-dir and --session-id', valid=same and said)
    msgs = [{'id': 'p3', 'type': 'prompt', 'message': 'Count from 1 to 400, one number per line, with no other text.'}]
    out, err, rc, sent = pi_session(argv2, cwd, msgs, until='agent_settled', abort_on='message_update')
    fs = frames(out)
    stops = [f.get('message', {}).get('stopReason') for f in fs if f.get('type') == 'message_end' and f.get('message', {}).get('role') == 'assistant']
    save('pi', version, 'rpc-abort', shown + ['--session-id', '<session id of rpc-prompt>'], sent, out, err, rc,
         'abort command sent on the first message_update', valid='aborted' in stops and any(f.get('type') == 'agent_settled' for f in fs),
         extra={'assistant_stop_reasons': stops})


def sha256(data: bytes) -> str:
    import hashlib
    return hashlib.sha256(data).hexdigest()


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
    return info


def write_manifest() -> None:
    agents = {}
    for agent_dir in sorted(p for p in HERE.iterdir() if p.is_dir() and not p.name.startswith('_')):
        for version_dir in sorted(p for p in agent_dir.iterdir() if p.is_dir()):
            name = agent_dir.name
            cases = {}
            for meta in sorted(version_dir.glob('*/meta.json')):
                m = json.loads(meta.read_text())
                cases[meta.parent.name] = {'valid': m['valid'], 'files': {f.name: sha256(f.read_bytes()) for f in sorted(meta.parent.iterdir())}}
            auth_file = version_dir / 'auth-status.json'
            if auth_file.exists():
                auth = json.loads(auth_file.read_text())['state']
            elif any(c in cases for c in ('success', 'rpc-prompt')):
                auth = 'AUTHENTICATED (live turn recorded)'
            else:
                auth = 'BLOCKED_AUTH'
            help_argv = [name, 'exec', '--help'] if name == 'codex' else [name, '--help']
            entry = {'cli_version': version_dir.name, 'auth': auth, 'help_argv': help_argv,
                     'help_sha256': sha256(subprocess.run(help_argv, capture_output=True, env=ENV).stdout), **binaries(name), 'cases': cases}
            if (version_dir / 'app-server-schema').is_dir():
                entry['app_server_schema_tree_sha256'] = tree_sha256(version_dir / 'app-server-schema')
            agents[name] = entry
    manifest = {'schema_version': '1.1', 'recorded_at': now(), 'recorder': 'tests/fixtures/protocol/record.py',
                'hash_algorithms': {'tree_sha256': tree_sha256.__doc__, 'files': 'sha256 of the raw file bytes'}, 'agents': agents}
    (HERE / 'compat-manifest.json').write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + '\n')


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('--agents', default='claude,codex,pi')
    selected = ap.parse_args().agents.split(',')
    for name, fn in [('claude', claude), ('codex', codex), ('pi', pi)]:
        if name in selected:
            fn()
            print(f'recorded {name}')
    write_manifest()
    print('wrote compat-manifest.json')
    return 0


if __name__ == '__main__':
    sys.exit(main())
