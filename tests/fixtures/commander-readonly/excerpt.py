#!/usr/bin/env python3
"""Redacted excerpts of an ADR-032 S0 spike run (record_spike.py) for committing as judge fixtures.

The raw recordings stay local (.hoh-dev/evidence/commander-readonly/spike/<run>/): they may carry local paths, plugin
lists, rate-limit metadata and account identifiers. An excerpt keeps only the frames the evidence table reads, with
the run's temporary paths replaced by <P>, <W>, <T>, the home directory by <HOME>, every uuid (thread, turn, item and
session identifiers) by <uuid-N> (one numbering for the whole run, so a resumed thread keeps its new thread's
placeholder), and any field named like an account, user, e-mail, plan, organisation or credential dropped. Its first
line names the source file and its sha256 and says it is an excerpt. Every excerpt is scanned again afterwards;
a hit stops the run (exit 1) and nothing is written.

Usage: python3 tests/fixtures/commander-readonly/excerpt.py <spike run dir>... [--out tests/fixtures/commander-readonly/excerpts]
"""
from __future__ import annotations
import argparse, hashlib, json, os, re, sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
DROP_KEY = re.compile(r'(account|user_?id|email|e_mail|plan|organi[sz]ation|org_?id|credential|api_?key|oauth|auth|bearer|'
                      r'secret|password|cookie|session_?token|access_?token|refresh_?token|rate_?limit|uuid|request_?id|'
                      r'conversation_?id|installation|machine_?id|device)', re.I)
SCAN = [re.compile(p) for p in (r'[\w.+-]+@[\w-]+\.[\w.]+', r'\bsk-[A-Za-z0-9]{8,}', r'\beyJ[A-Za-z0-9_-]{10,}', r'Bearer\s',
                                r'acct_[A-Za-z0-9]', r'user-[A-Za-z0-9]{8,}', r'org-[A-Za-z0-9]{8,}')]


UUID = re.compile(r'[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}', re.I)
UUIDS: dict[str, str] = {}


def uuid_placeholder(m: re.Match) -> str:
    return UUIDS.setdefault(m.group(0).lower(), f'<uuid-{len(UUIDS) + 1}>')


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def redact(value, subst: list[tuple[str, str]]):
    if isinstance(value, dict):
        return {k: redact(v, subst) for k, v in value.items() if not DROP_KEY.search(k)}
    if isinstance(value, list):
        return [redact(v, subst) for v in value]
    if isinstance(value, str):
        for old, new in subst:
            value = value.replace(old, new)
        return UUID.sub(uuid_placeholder, value)
    return value


def frames(path: Path) -> list[dict]:
    out = []
    for line in path.read_bytes().splitlines():
        try:
            f = json.loads(line)
        except ValueError:
            continue
        if isinstance(f, dict):
            out.append(f)
    return out


def keep_claude(f: dict) -> bool:
    if f.get('type') == 'system' and f.get('subtype') in ('init', 'permission_denied'):
        return True
    if f.get('type') in ('assistant', 'user'):
        content = (f.get('message') or {}).get('content')
        return isinstance(content, list) and any(isinstance(c, dict) and c.get('type') in ('tool_use', 'tool_result') for c in content)
    return f.get('type') == 'result'


def slim_claude(f: dict) -> dict:
    if f.get('type') == 'system' and f.get('subtype') == 'init':
        return {k: f.get(k) for k in ('type', 'subtype', 'tools', 'mcp_servers', 'permissionMode')}
    if f.get('type') == 'system':
        return {k: f.get(k) for k in ('type', 'subtype', 'tool_name', 'tool_use_id', 'decision_reason_type', 'message')}
    if f.get('type') == 'result':
        return {k: f.get(k) for k in ('type', 'subtype', 'is_error', 'permission_denials', 'result')}
    content = [c for c in f['message']['content'] if isinstance(c, dict) and c.get('type') in ('tool_use', 'tool_result')]
    return {'type': f['type'], 'message': {'content': content}}


CODEX_KEEP = ('item/started', 'item/completed', 'turn/completed', 'error', 'warning')
RAW_TOOL_ITEMS = ('custom_tool_call', 'custom_tool_call_output', 'function_call', 'function_call_output', 'local_shell_call')


def keep_codex(f: dict) -> bool:
    if 'result' in f and isinstance(f.get('result'), dict) and ('sandbox' in f['result'] or 'thread' in f['result']):
        return True
    if f.get('method') in ('item/started', 'item/completed'):
        return (f.get('params', {}).get('item', {}).get('type')) in ('commandExecution', 'fileChange', 'agentMessage')
    if f.get('method') == 'rawResponseItem/completed':  # only tool calls and their outputs: messages may carry instructions
        return (f.get('params', {}).get('item', {}).get('type')) in RAW_TOOL_ITEMS
    if f.get('method') in CODEX_KEEP:
        return True
    if f.get('type') in ('item.started', 'item.completed', 'turn.completed', 'turn.failed', 'error'):
        return True
    return 'method' in f and 'id' in f  # a server request, if any


def slim_codex(f: dict) -> dict:
    item = f.get('params', {}).get('item', {}) if f.get('method') == 'rawResponseItem/completed' else {}
    if item.get('type') == 'custom_tool_call_output' and 'mcp__' in json.dumps(item):
        # the model listed its tools: the list names the user's connectors and MCP servers
        output = [{**o, 'text': '<redacted: a listing of the session tools, which names the user connectors>'}
                  if isinstance(o, dict) and 'mcp__' in str(o.get('text', '')) else o for o in item.get('output', [])]
        return {**f, 'params': {**f['params'], 'item': {**item, 'output': output}}}
    if 'result' in f and isinstance(f.get('result'), dict):
        r = f['result']
        return {'id': f.get('id'), 'result': {'sandbox': r.get('sandbox'), 'approvalPolicy': r.get('approvalPolicy'),
                                              'thread': {'id': '<thread>'} if 'thread' in r else None}}
    return f


def slim_rollout(f: dict) -> dict | None:
    """A codex rollout line the evidence table reads (turn boundaries, the turn's policy, tool calls and outputs)."""
    payload = f.get('payload') or {}
    if f.get('type') == 'event_msg' and payload.get('type') in ('task_started', 'task_complete'):
        return {'type': f['type'], 'payload': {'type': payload['type'], 'turn_id': payload.get('turn_id')}}
    if f.get('type') == 'turn_context':
        return {'type': f['type'], 'payload': {k: payload.get(k) for k in ('turn_id', 'approval_policy', 'sandbox_policy')}}
    if f.get('type') == 'response_item' and payload.get('type') in RAW_TOOL_ITEMS:
        kept = {k: payload[k] for k in ('type', 'call_id', 'name', 'status', 'input', 'output', 'arguments') if k in payload}
        kept['turn_id'] = (payload.get('internal_chat_message_metadata_passthrough') or {}).get('turn_id')
        line = slim_codex({'method': 'rawResponseItem/completed', 'params': {'item': kept}})
        return {'type': f['type'], 'payload': line['params']['item']}
    return None


def keep_pi(f: dict) -> bool:
    if f.get('type') == 'extension_ui_request' and 'hoh_observer' in str(f.get('message', '')):
        return True
    if f.get('type') in ('tool_execution_start', 'tool_execution_end', 'agent_settled'):
        return True
    if f.get('type') == 'message_end' and (f.get('message') or {}).get('role') == 'assistant':
        return True  # the final message carries the hoh-decisions block
    return f.get('type') == 'response' and f.get('command') in ('prompt', 'get_state')


def slim_pi(f: dict) -> dict:
    if f.get('type') == 'response' and f.get('command') == 'get_state':
        return {'type': 'response', 'command': 'get_state', 'success': f.get('success')}
    if f.get('type') == 'message_end':
        m = f['message']
        return {'type': 'message_end', 'message': {'role': m.get('role'), 'stopReason': m.get('stopReason'),
                                                   'content': [c for c in m.get('content', []) if c.get('type') == 'text']}}
    if f.get('type') == 'extension_ui_request':  # the observer's report: builtin tools kept, the user's extensions not named
        report = json.loads(f['message'])
        names: dict[str, str] = {}
        for tool, source in report['sources'].items():
            if source != 'builtin':
                names[tool] = f'<extension-tool-{len(names) + 1}>'
        report['active'] = [names.get(t, t) for t in report['active']]
        report['sources'] = {names.get(t, t): ('builtin' if s == 'builtin' else '<extension>') for t, s in report['sources'].items()}
        return {**f, 'message': json.dumps(report)}
    return f


AGENT = {'claude': (keep_claude, slim_claude), 'codex': (keep_codex, slim_codex), 'pi': (keep_pi, slim_pi)}


def excerpt_case(case_dir: Path, agent: str, run: Path) -> tuple[list[str], dict, list[str] | None]:
    meta = json.loads((case_dir / 'meta.json').read_text())
    home = os.path.expanduser('~')
    subst = sorted(((k, v) for k, v in meta['placeholders'].items()), key=lambda kv: -len(kv[0])) + [(home, '<HOME>')]
    keep, slim = AGENT[agent]
    stdout = case_dir / 'stdout.jsonl'
    lines = [json.dumps({'_excerpt': {'source': f'.hoh-dev/evidence/commander-readonly/spike/{run.name}/{agent}/{case_dir.name}/stdout.jsonl',
                                      'source_sha256': sha256(stdout.read_bytes()), 'excerpted': True,
                                      'kept': 'the frames the ADR-032 evidence table reads', 'paths': '<P> <W> <T> <HOME>'}},
                         ensure_ascii=False)]
    for f in frames(stdout):
        if keep(f):
            lines.append(json.dumps(redact(slim(f), subst), ensure_ascii=False))
    rollout = None
    if (case_dir / 'rollout.jsonl').is_file():
        raw = (case_dir / 'rollout.jsonl').read_bytes()
        rollout = [json.dumps({'_excerpt': {
            'source': f'.hoh-dev/evidence/commander-readonly/spike/{run.name}/{agent}/{case_dir.name}/rollout.jsonl',
            'source_sha256': sha256(raw), 'excerpted': True, 'paths': '<P> <W> <T> <HOME>',
            'kept': "the thread's rollout file (thread.path) at the end of the case: turn boundaries, each turn's "
                    'approval and sandbox policy, tool calls and their outputs'}}, ensure_ascii=False)]
        rollout += [json.dumps(redact(line, subst), ensure_ascii=False)
                    for line in (slim_rollout(f) for f in frames(case_dir / 'rollout.jsonl')) if line]
    snap = json.loads((case_dir / 'snapshot.json').read_text())
    side = {'agent': agent, 'case': case_dir.name, 'argv': redact(meta['argv'], subst), 'exit_code': meta['exit_code'],
            'stopped': meta['stopped'], 'notes': meta['notes'], 'snapshot_diff': redact(snap['diff'], subst),
            'stdout_sha256': meta['stdout_sha256'], 'stderr_sha256': meta['stderr_sha256'],
            'driver': redact((case_dir / 'cells.sh').read_text(), subst)}
    return lines, side, rollout


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('runs', type=Path, nargs='+')
    ap.add_argument('--out', type=Path, default=HERE / 'excerpts')
    args = ap.parse_args()
    staged: dict[Path, str] = {}
    cases = [(run, agent, case_dir) for run in args.runs for agent in AGENT
             for case_dir in (sorted((run / agent).glob('*')) if (run / agent).is_dir() else [])]
    for run, agent, case_dir in cases:
        lines, side, rollout = excerpt_case(case_dir, agent, run)
        if rollout:
            staged[args.out / agent / f'{case_dir.name}.rollout.excerpt.jsonl'] = '\n'.join(rollout) + '\n'
        staged[args.out / agent / f'{case_dir.name}.stdout.excerpt.jsonl'] = '\n'.join(lines) + '\n'
        staged[args.out / agent / f'{case_dir.name}.meta.json'] = json.dumps(side, ensure_ascii=False, indent=1) + '\n'
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
