#!/usr/bin/env python3
"""Redacted, structure-preserving excerpts of ADR-032 S0 spike runs (record_spike.py), committed as judge fixtures.

The raw recordings stay local (.hoh-dev/evidence/commander-readonly/spike/<run>/): they carry local paths, plugin
lists, rate-limit and plan metadata and account identifiers. An excerpt keeps only the lines the evidence table
reads, and every kept line is the raw line with some values replaced (ADR-032 design correction 4): keys, nesting,
array lengths and the JSON type of every value stay the source's. Replaced values:

- the run's temporary paths become <P>, <W>, <T>, the home directory <HOME>;
- every uuid becomes <uuid-N> (one numbering for all the runs given, so a resumed session keeps its placeholder);
- every string or number under a key named like an account, user, e-mail, plan, organisation, credential, token,
  rate limit, request id, device, socket or provider base URL becomes "<redacted>" or 0;
- the conversation pi's agent_end repeats (it carries the system prompt and the tool descriptions of the user's
  extensions; pi.rs reads nothing of it) has its strings replaced;
- every string under a key that lists the user's plugins, skills, slash commands, agents or memory paths becomes
  "<redacted>";
- a string in which the model listed its tools (it names the user's connectors: `mcp__`) becomes a marker;
- in the pi observer's report, the names of the user's extensions' tools become <extension-tool-N>.

For codex and pi it also excerpts the request log (stdin.txt, every JSON line), with which the adapters correlate the
responses (ADR-032 design correction 7).

The first line of an excerpt is a header: the source file and its sha256, and for every following line its line
index in the source and the sha256 of its key skeleton (excerpt_check.py checks both). Every excerpt is scanned for
e-mail addresses, keys and tokens before anything is written; a hit writes nothing (exit 1).

Usage: python3 tests/fixtures/commander-readonly/excerpt.py <spike run dir>... [--out tests/fixtures/commander-readonly/excerpts]
"""
from __future__ import annotations
import argparse, hashlib, json, os, re, sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
SENSITIVE = ('account', 'userid', 'email', 'plantype', 'orgid', 'organization', 'organisation', 'credential', 'apikey',
             'oauth', 'bearer', 'secret', 'password', 'cookie', 'sessiontoken', 'accesstoken', 'refreshtoken', 'idtoken',
             'ratelimit', 'requestid', 'responseid', 'conversationid', 'installation', 'machineid', 'device', 'socket',
             'creator', 'baseurl')
SENSITIVE_EXACT = ('plan', 'auth', 'authorization')
LISTS = ('plugins', 'skills', 'slashcommands', 'terminalslashcommands', 'agents', 'memorypaths', 'disabledpluginids')
TOOL_LISTING = '<redacted: a listing of the session tools, which names the user connectors>'
UUID = re.compile(r'[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}', re.I)
UUIDS: dict[str, str] = {}
SCAN = [re.compile(p) for p in (r'[\w.+-]+@[\w-]+\.[\w.]+', r'\bsk-[A-Za-z0-9]{8,}', r'\beyJ[A-Za-z0-9_-]{10,}', r'Bearer\s',
                                r'acct_[A-Za-z0-9]', r'user-[A-Za-z0-9]{8,}', r'org-[A-Za-z0-9]{8,}', r'/run/user/\d')]
RAW_TOOL_ITEMS = ('custom_tool_call', 'custom_tool_call_output', 'function_call', 'function_call_output', 'local_shell_call')


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def skeleton(value):
    """Keys, nesting, array lengths and leaf types: what redaction must not change (ADR-032 design correction 4)."""
    if isinstance(value, dict):
        return {k: skeleton(v) for k, v in sorted(value.items())}
    if isinstance(value, list):
        return [skeleton(v) for v in value]
    if isinstance(value, bool):
        return 'b'
    if isinstance(value, (int, float)):
        return 'n'
    return 'z' if value is None else 's'


def skeleton_sha256(value) -> str:
    return sha256(json.dumps(skeleton(value), sort_keys=True, separators=(',', ':')).encode())


def norm(key: str) -> str:
    return re.sub(r'[^a-z0-9]', '', key.lower())


def uuid_placeholder(m: re.Match) -> str:
    return UUIDS.setdefault(m.group(0).lower(), f'<uuid-{len(UUIDS) + 1}>')


def observer_report(text: str) -> str:
    """The pi observer's JSON report with the user's extensions' tool names replaced (builtin names kept)."""
    report = json.loads(text)
    names: dict[str, str] = {}
    for tool, source in report['sources'].items():
        if source != 'builtin':
            names[tool] = f'<extension-tool-{len(names) + 1}>'
    report['active'] = [names.get(t, t) for t in report['active']]
    report['sources'] = {names.get(t, t): ('builtin' if s == 'builtin' else '<extension>') for t, s in report['sources'].items()}
    return json.dumps(report, separators=(',', ':'))


TEXT_KEEP = ('type', 'id', 'role', 'status', 'phase', 'call_id', 'name')


def hide_texts(value, keep: tuple = TEXT_KEEP):
    """Every string under a raw message, reasoning or tool listing item (they may carry instructions and the user's
    connectors) replaced, keys and shapes kept."""
    if isinstance(value, dict):
        return {k: (v if k in keep and not isinstance(v, (dict, list)) else hide_texts(v, ())) for k, v in value.items()}
    if isinstance(value, list):
        return [hide_texts(v, ()) for v in value]
    return '<redacted: model or instruction text>' if isinstance(value, str) else value


def pre_redact(f: dict) -> dict:
    """Value redaction of codex's raw items that are no tool call: messages, reasoning and the tool listing."""
    item = (f.get('params') or {}).get('item')
    if f.get('method') == 'rawResponseItem/completed' and isinstance(item, dict) and item.get('type') not in RAW_TOOL_ITEMS:
        return {**f, 'params': {**f['params'], 'item': hide_texts(item)}}
    if f.get('type') == 'agent_end' and isinstance(f.get('messages'), list):
        return {**f, 'messages': hide_texts(f['messages'], ())}
    if f.get('method') == 'rawResponse/completed' and isinstance((f.get('params') or {}).get('usageMetadata'), dict):
        return {**f, 'params': {**f['params'], 'usageMetadata': hide_texts(f['params']['usageMetadata'], ())}}
    return f


def redact(value, subst: list[tuple[str, str]], hidden: bool = False):
    if isinstance(value, dict):
        return {k: redact(v, subst, hidden or norm(k) in LISTS or norm(k) in SENSITIVE_EXACT
                          or any(s in norm(k) for s in SENSITIVE)) for k, v in value.items()}
    if isinstance(value, list):
        return [redact(v, subst, hidden) for v in value]
    if isinstance(value, bool) or value is None:
        return value
    if isinstance(value, (int, float)):
        return 0 if hidden else value
    if hidden:
        return '<redacted>'
    if 'mcp__' in value:
        return TOOL_LISTING
    if value.startswith('{"hoh_observer"'):
        value = observer_report(value)
    for old, new in subst:
        value = value.replace(old, new)
    return UUID.sub(uuid_placeholder, value)


def raw_lines(path: Path) -> list[tuple[int, dict]]:
    out = []
    for i, line in enumerate(path.read_bytes().splitlines()):
        try:
            f = json.loads(line)
        except ValueError:
            continue
        if isinstance(f, dict):
            out.append((i, f))
    return out


REPO = HERE.parents[2]


def registry(path: str, lists: list[list[str]]) -> set[str]:
    """The keys of a frame registry, read as crates/hoh-agents/src/known.rs `Registry::parse` reads them."""
    doc = json.loads((REPO / path).read_text())
    keys = set()
    for steps in lists:
        node = doc
        for step in steps:
            node = node.get(step, []) if isinstance(node, dict) else []
        keys |= {e['key'] for e in node if isinstance(e, dict) and isinstance(e.get('key'), str)}
    return keys


KNOWN_CLAUDE = registry('tests/fixtures/protocol/claude/2.1.318/known-frames.json', [['frames']])
KNOWN_PI = registry('tests/fixtures/protocol/pi/1.5.0/known-frames.json', [['frames']])
KNOWN_CODEX = registry('tests/fixtures/protocol/codex/0.166.0/known-frames.json',
                       [['app_server', 'notifications'], ['app_server', 'requests']])
SCOPE_OF = ('item/started', 'item/completed', 'thread/tokenUsage/updated', 'turn/started', 'turn/completed')


# The kept lines are every frame that changes the adapter's state (ADR-032 design correction 7, M2T20D-800: derived
# from the adapter's own dispatch, so the product's replay of an excerpt reaches the same state as of the raw), plus
# the frames the evidence table reads. Frames the adapter only reads past (known to its registry, no branch of its
# dispatch) are left out.

def keep_claude(f: dict) -> bool:
    """claude.rs `Driven::on_line`: a frame not in the registry (an unknown event), system/init, an assistant frame
    (authentication_failed), result; and, for the evidence, user frames (tool results) and system/permission_denied."""
    key = f'{f.get("type")}/{f.get("subtype")}' if f.get('type') in ('system', 'result') and f.get('subtype') else f.get('type')
    if key not in KNOWN_CLAUDE:
        return True
    return f.get('type') in ('assistant', 'user', 'result') or key in ('system/init', 'system/permission_denied')


def keep_codex(f: dict) -> bool:
    """codex/app_server.rs `State::on_line`: every response (`on_response`), every server request (`on_request`), and
    of the notifications (`on_notification`) the turn-scoped ones (`scope_of`), `error`, and every one not in the
    registry (an unknown event). For exec (not judged): its item and turn events."""
    if 'method' not in f:
        return 'id' in f or f.get('type') in ('thread.started', 'item.started', 'item.completed', 'turn.completed', 'turn.failed', 'error')
    if 'id' in f:
        return True
    method = f['method']
    return method in SCOPE_OF or method == 'error' or method not in KNOWN_CODEX


def keep_rollout(f: dict) -> bool:
    payload = f.get('payload') or {}
    if f.get('type') == 'event_msg':
        return payload.get('type') in ('task_started', 'task_complete')
    if f.get('type') == 'turn_context':
        return True
    return f.get('type') == 'response_item' and isinstance(payload.get('type'), str) and (
        payload['type'].endswith('_call') or payload['type'].endswith('_call_output'))


def keep_pi(f: dict) -> bool:
    """pi.rs `Rpc::on_record`: a record not in the registry (an unknown event), response, agent_start, agent_end,
    agent_settled, tool_execution_start, message_end, extension_ui_request; and, for the evidence,
    tool_execution_end."""
    if f.get('type') not in KNOWN_PI:
        return True
    if f.get('type') == 'message_end':
        return (f.get('message') or {}).get('role') == 'assistant'
    return f.get('type') in ('response', 'agent_start', 'agent_end', 'agent_settled', 'tool_execution_start',
                             'tool_execution_end', 'extension_ui_request')


KEEP = {'claude': keep_claude, 'codex': keep_codex, 'pi': keep_pi}


def excerpt_file(source: Path, rel: str, keep, subst: list[tuple[str, str]], kept: str) -> str:
    lines, index, skeletons = [], [], []
    for i, f in raw_lines(source):
        if keep(f):
            r = redact(pre_redact(f), subst)
            if skeleton(r) != skeleton(f):
                raise SystemExit(f'redaction changed the structure of {rel} line {i}')
            lines.append(json.dumps(r, ensure_ascii=False))
            index.append(i)
            skeletons.append(skeleton_sha256(r))
    header = {'_excerpt': {'source': rel, 'source_sha256': sha256(source.read_bytes()), 'excerpted': True, 'kept': kept,
                           'redaction': 'values only: keys, nesting, array lengths and value types are the source line\'s',
                           'paths': '<P> <W> <T> <HOME>', 'lines': index, 'skeleton_sha256': skeletons}}
    return '\n'.join([json.dumps(header, ensure_ascii=False)] + lines) + '\n'


def excerpt_case(case_dir: Path, agent: str, run: Path) -> dict[str, str]:
    meta = json.loads((case_dir / 'meta.json').read_text())
    home = os.path.expanduser('~')
    subst = sorted(meta['placeholders'].items(), key=lambda kv: -len(kv[0])) + [(home, '<HOME>')]
    base = f'.hoh-dev/evidence/commander-readonly/spike/{run.name}/{agent}/{case_dir.name}'
    out = {'stdout.excerpt.jsonl': excerpt_file(case_dir / 'stdout.jsonl', f'{base}/stdout.jsonl', KEEP[agent], subst,
                                                'the lines the ADR-032 evidence table reads')}
    if (agent == 'pi' or (agent == 'codex' and not case_dir.name.startswith('exec'))) and (case_dir / 'stdin.txt').is_file():
        # The request log: the adapters correlate the responses with the requests it holds (app_server.rs pending,
        # pi.rs pending); ADR-032 design correction 7.
        out['stdin.excerpt.jsonl'] = excerpt_file(case_dir / 'stdin.txt', f'{base}/stdin.txt', lambda f: True, subst,
                                                  'every request and notification the recorder sent')
    if (case_dir / 'rollout.jsonl').is_file():
        out['rollout.excerpt.jsonl'] = excerpt_file(
            case_dir / 'rollout.jsonl', f'{base}/rollout.jsonl', keep_rollout, subst,
            "the thread's rollout file (thread.path) at the end of the case: turn boundaries, each turn's context, "
            'tool calls and their outputs')
    snap_raw = (case_dir / 'snapshot.json').read_bytes()
    snap = json.loads(snap_raw)
    side = {'agent': agent, 'case': case_dir.name, 'argv': redact(meta['argv'], subst), 'exit_code': meta['exit_code'],
            'stopped': meta['stopped'], 'notes': meta['notes'], 'snapshot_diff': redact(snap['diff'], subst),
            'snapshot_source': f'{base}/snapshot.json', 'snapshot_sha256': sha256(snap_raw),
            'stdout_sha256': meta['stdout_sha256'], 'stderr_sha256': meta['stderr_sha256'],
            'token_sha256': meta['token_sha256'], 'driver': redact((case_dir / 'cells.sh').read_text(), subst)}
    if 'session_dir' in meta:
        side['session_dir'] = redact(meta['session_dir'], subst)
    # The external observations the adapter makes (design correction 7, M2T20D-801): as recorded, else from the
    # recorder's own record of the process's end (the stream read to its end when it exited by itself).
    side['observations'] = redact(meta.get('observations') or {
        'exit': {'returncode': meta['exit_code'], 'how': meta['stopped']}, 'logs_complete': meta['stopped'] == 'exited'}, subst)
    out['meta.json'] = json.dumps(side, ensure_ascii=False, indent=1) + '\n'
    return out


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('runs', type=Path, nargs='+')
    ap.add_argument('--out', type=Path, default=HERE / 'excerpts')
    args = ap.parse_args()
    staged: dict[Path, str] = {}
    cases = [(run, agent, case_dir) for run in args.runs for agent in KEEP
             for case_dir in (sorted((run / agent).glob('*')) if (run / agent).is_dir() else [])]
    for run, agent, case_dir in cases:
        for kind, text in excerpt_case(case_dir, agent, run).items():
            staged[args.out / agent / f'{case_dir.name}.{kind}'] = text
    hits = [(str(p), pat.pattern) for p, text in staged.items() for pat in SCAN if pat.search(text)]
    if hits:
        print('secret scan hits, nothing written: ' + json.dumps(hits), file=sys.stderr)
        return 1
    for path, text in staged.items():
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)
    print(f'wrote {len(staged)} files under {args.out}')
    return 0


if __name__ == '__main__':
    sys.exit(main())
