#!/usr/bin/env python3
"""Design-time judge of ADR-032's evidence table (design correction 4): every row's rule, executed on the recordings.

The rules below are the evidence table's rules, written as JSON paths and tests; the ADR quotes them. They run on
every committed excerpt (tests/fixtures/commander-readonly/excerpts/) and, where it is present locally with the
recorded sha256, on the raw recording the excerpt was cut from (.hoh-dev/evidence/commander-readonly/spike/). A row
whose verdict on an excerpt differs from its verdict on the raw source is a defect and makes the exit status 1.

Verdicts: availability rows (E1, E3, E8) give conform | nonconform | insufficient; cell rows (E2, E5, E6, E9) give
refused | success | insufficient per cell or call; E10 gives conform | breach; E11 gives conform | insufficient.
A round (decision 8's "一轮 PASS") gives PASS | FAIL | INSUFFICIENT.

Mutations: a few one-value changes of recorded lines (a missing or another turn id, a changed refusal sentence, a
joined output, a changed patch) show that the E6 rule does not accept what it must not.

M2-T20's judge must give the same verdicts on the same fixtures (ADR-032 design correction 4).

Usage: python3 tests/fixtures/commander-readonly/judge_check.py [--excerpts DIR] [--root REPO] [--json OUT] [--md OUT]
"""
from __future__ import annotations
import argparse, copy, hashlib, json, re, sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
REGIONS = (('worktree', 'P'), ('git', 'P/.git'), ('hoh', 'P/.hoh'), ('w', 'W'))
OPS = ('create', 'modify', 'delete')
READ_TOOLS = {'claude': {'Read', 'Grep', 'Glob'}, 'pi': {'read', 'grep', 'find', 'ls'}}
PATCH_REFUSAL = 'Script error:\npatch rejected: writing is blocked by read-only sandbox; rejected by user approval settings'
PI_SESSION_DIR = 'P/.hoh/sessions/s1'  # the segment's own session directory, as record_spike.py passes it
CONTROL = {'claude': 'control', 'codex': 'app-control', 'pi': 'control'}
CELL_LINE = re.compile(r'^HOH-CELL (\S+) (\S+) (\S+) exit=(\d+) err=(.*)$')
APPLY_PATCH = re.compile(r'tools\.apply_patch\(("(?:[^"\\]|\\.)*")\)')
BLOCK = re.compile(r'```hoh-decisions[ \t]*\n(.*?)\n```', re.S)


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


# ---------------------------------------------------------------------------------------------------------------- input

class Case:
    """One recorded case, read either from its committed excerpt or from its raw recording."""

    def __init__(self, agent: str, name: str, form: str, stdout: list[dict], rollout: list[dict] | None, diff: dict,
                 paths: dict[str, str], token_sha256: str):
        self.agent, self.name, self.form = agent, name, form
        self.stdout, self.rollout, self.diff, self.paths, self.token_sha256 = stdout, rollout, diff, paths, token_sha256

    def region(self, rel: str) -> str:  # 'P/.git' -> the case's real or placeholder path
        head, _, tail = rel.partition('/')
        return self.paths[head] + ('/' + tail if tail else '')


def json_lines(text: str) -> list[dict]:
    out = []
    for line in text.splitlines():
        try:
            f = json.loads(line)
        except ValueError:
            continue
        if isinstance(f, dict) and '_excerpt' not in f:
            out.append(f)
    return out


def load(excerpts: Path, root: Path, agent: str, name: str) -> tuple[Case, Case | None, str]:
    """The case from its excerpt, and from its raw source when present with the recorded sha256 (else None, why)."""
    base = excerpts / agent / name
    meta = json.loads((base.parent / f'{name}.meta.json').read_text())
    stdout_text = (base.parent / f'{name}.stdout.excerpt.jsonl').read_text()
    rollout_path = base.parent / f'{name}.rollout.excerpt.jsonl'
    rollout_text = rollout_path.read_text() if rollout_path.is_file() else None
    placeholders = {'P': '<P>', 'W': '<W>', 'T': '<T>'}
    excerpt = Case(agent, name, 'excerpt', json_lines(stdout_text), json_lines(rollout_text) if rollout_text else None,
                   meta['snapshot_diff'], placeholders, meta['token_sha256'])
    header = json.loads(stdout_text.splitlines()[0])['_excerpt']
    raw_dir = root / Path(header['source']).parent
    raw_stdout = raw_dir / 'stdout.jsonl'
    if not raw_stdout.is_file():
        return excerpt, None, 'raw recording not present'
    if sha256(raw_stdout.read_bytes()) != header['source_sha256']:
        return excerpt, None, 'raw recording differs from the recorded sha256'
    if rollout_text:
        rollout_header = json.loads(rollout_text.splitlines()[0])['_excerpt']
        raw_rollout = raw_dir / 'rollout.jsonl'
        if not raw_rollout.is_file() or sha256(raw_rollout.read_bytes()) != rollout_header['source_sha256']:
            return excerpt, None, 'raw rollout missing or differs from the recorded sha256'
    raw_meta = json.loads((raw_dir / 'meta.json').read_text())
    inverse = {v.strip('<>'): k for k, v in raw_meta['placeholders'].items()}
    rollout = json_lines((raw_dir / 'rollout.jsonl').read_text()) if rollout_text else None
    raw = Case(agent, name, 'raw', json_lines(raw_stdout.read_text()), rollout,
               json.loads((raw_dir / 'snapshot.json').read_text())['diff'], inverse, raw_meta['token_sha256'])
    return excerpt, raw, ''


# ---------------------------------------------------------------------------------------------------------------- rules

def e1_claude_availability(c: Case) -> str:
    """init frames: type == "system" and subtype == "init"; conform iff set(tools) == {Read, Grep, Glob} (three
    entries), mcp_servers == [] and permissionMode == "dontAsk"; none: insufficient; otherwise nonconform."""
    inits = [f for f in c.stdout if f.get('type') == 'system' and f.get('subtype') == 'init']
    if not inits:
        return 'insufficient'
    ok = all(isinstance(f.get('tools'), list) and len(f['tools']) == 3 and set(f['tools']) == READ_TOOLS['claude']
             and f.get('mcp_servers') == [] and f.get('permissionMode') == 'dontAsk' for f in inits)
    return 'conform' if ok else 'nonconform'


def e2_claude_tool_results(c: Case) -> dict[str, str]:
    """Per tool_use (assistant message.content[] with type == "tool_use"), keyed "<name> <id>":
    refused iff (1) a frame type == "system", subtype == "permission_denied" has tool_use_id == id, tool_name == name,
    decision_reason_type == "mode"; (2) some result frame's permission_denials[] has an element with tool_use_id == id,
    tool_name == name, tool_input == the tool_use's input; (3) the tool_result (user message.content[] with
    type == "tool_result" and tool_use_id == id) has is_error == true.
    success iff the tool_result exists and its is_error is absent or false; otherwise insufficient."""
    uses = [(u['id'], u['name'], u.get('input')) for f in c.stdout if f.get('type') == 'assistant'
            for u in (f.get('message') or {}).get('content') or [] if isinstance(u, dict) and u.get('type') == 'tool_use']
    results = {r.get('tool_use_id'): r for f in c.stdout if f.get('type') == 'user'
               for r in (f.get('message') or {}).get('content') or [] if isinstance(r, dict) and r.get('type') == 'tool_result'}
    out = {}
    for uid, name, args in uses:
        frame = any(f.get('type') == 'system' and f.get('subtype') == 'permission_denied' and f.get('tool_use_id') == uid
                    and f.get('tool_name') == name and f.get('decision_reason_type') == 'mode' for f in c.stdout)
        denial = any(d.get('tool_use_id') == uid and d.get('tool_name') == name and d.get('tool_input') == args
                     for f in c.stdout if f.get('type') == 'result' for d in f.get('permission_denials') or [])
        r = results.get(uid)
        if frame and denial and r is not None and r.get('is_error') is True:
            out[f'{name} {uid}'] = 'refused'
        elif r is not None and r.get('is_error') is not True:
            out[f'{name} {uid}'] = 'success'
        else:
            out[f'{name} {uid}'] = 'insufficient'
    return out


def turn_id(c: Case) -> str | None:
    """The round's turn: the last turn/completed's params.turn.id in the App Server stream."""
    ids = [f['params']['turn']['id'] for f in c.stdout if f.get('method') == 'turn/completed']
    return ids[-1] if ids else None


def e3_codex_availability(c: Case) -> str:
    """The thread/start or thread/resume response (a frame whose result has "sandbox"): result.sandbox.type ==
    "readOnly"; and the rollout line type == "turn_context" with payload.turn_id == the round's turn id:
    payload.sandbox_policy.type == "read-only" and payload.approval_policy == "never". Both: conform; either present
    with another value: nonconform; either missing: insufficient."""
    responses = [f['result'] for f in c.stdout if isinstance(f.get('result'), dict) and 'sandbox' in f['result']]
    tid = turn_id(c)
    ctxs = [l['payload'] for l in c.rollout or [] if l.get('type') == 'turn_context' and (l.get('payload') or {}).get('turn_id') == tid]
    if any((r.get('sandbox') or {}).get('type') != 'readOnly' for r in responses) or any(
            (p.get('sandbox_policy') or {}).get('type') != 'read-only' or p.get('approval_policy') != 'never' for p in ctxs):
        return 'nonconform'
    return 'conform' if responses and ctxs else 'insufficient'


def driver_output(c: Case) -> str | None:
    """E4: codex: an item/completed with item.type == "commandExecution" and item.command ==
    "/bin/bash -lc 'sh <T>/cells.sh'": its item.aggregatedOutput (item.exitCode is the script's, never a cell verdict).
    claude (control): the tool_result of a Bash tool_use whose input.command == "sh <T>/cells.sh"."""
    driver = f"{c.paths['T']}/cells.sh"
    for f in reversed(c.stdout):
        item = (f.get('params') or {}).get('item') or {}
        if f.get('method') == 'item/completed' and item.get('type') == 'commandExecution' and item.get('command') == f"/bin/bash -lc 'sh {driver}'":
            return item.get('aggregatedOutput')
    bash = {u['id'] for f in c.stdout if f.get('type') == 'assistant' for u in (f.get('message') or {}).get('content') or []
            if isinstance(u, dict) and u.get('type') == 'tool_use' and u.get('name') == 'Bash'
            and (u.get('input') or {}).get('command') == f'sh {driver}'}
    for f in c.stdout:
        for r in (f.get('message') or {}).get('content') or [] if f.get('type') == 'user' else []:
            if isinstance(r, dict) and r.get('type') == 'tool_result' and r.get('tool_use_id') in bash:
                return r['content'] if isinstance(r.get('content'), str) else None
    return None


def e5_shell_cells(c: Case) -> dict[str, str]:
    """Per cell "<region> <op>": the driver line "HOH-CELL <region> <op> <R>/hoh-shell-<op> exit=<n> err=<e>".
    refused iff n != 0 and e == "<target>: 1: cannot create <target>: Read-only file system" (create, modify) or
    e == "rm: cannot remove '<target>': Read-only file system" (delete); success iff n == 0; otherwise or no line:
    insufficient."""
    lines = {}
    for line in (driver_output(c) or '').splitlines():
        m = CELL_LINE.match(line)
        if m:
            lines[(m[1], m[2], m[3])] = (int(m[4]), m[5])
    out = {}
    for region, rel in REGIONS:
        for op in OPS:
            target = f'{c.region(rel)}/hoh-shell-{op}'
            got = lines.get((region, op, target))
            refusal = (f'{target}: 1: cannot create {target}: Read-only file system' if op != 'delete'
                       else f"rm: cannot remove '{target}': Read-only file system")
            out[f'{region} {op}'] = ('insufficient' if got is None else 'refused' if got[0] != 0 and got[1] == refusal
                                     else 'success' if got[0] == 0 else 'insufficient')
    return out


def patch_text(c: Case, rel: str, op: str) -> str:
    r = c.region(rel)
    return {'create': f'*** Begin Patch\n*** Add File: {r}/hoh-patch-create\n+hoh-cell\n*** End Patch',
            'modify': f'*** Begin Patch\n*** Update File: {r}/hoh-patch-modify\n@@\n-hoh-seed\n+hoh-cell\n*** End Patch',
            'delete': f'*** Begin Patch\n*** Delete File: {r}/hoh-patch-delete\n*** End Patch'}[op]


def patch_calls(c: Case) -> list[tuple[dict, str]]:
    """The round's patch calls: rollout lines type == "response_item" whose
    payload.internal_chat_message_metadata_passthrough.turn_id == the round's turn id, payload.type ==
    "custom_tool_call", payload.name == "exec", and payload.input holds exactly one tools.apply_patch(<JS string>);
    with the decoded string (trailing newlines removed)."""
    tid = turn_id(c)
    out = []
    for l in c.rollout or []:
        p = l.get('payload') or {}
        if (l.get('type') == 'response_item' and (p.get('internal_chat_message_metadata_passthrough') or {}).get('turn_id') == tid
                and p.get('type') == 'custom_tool_call' and p.get('name') == 'exec' and isinstance(p.get('input'), str)):
            found = APPLY_PATCH.findall(p['input'])
            if len(found) == 1 and p['input'].count('tools.apply_patch(') == 1:
                try:
                    out.append((p, json.loads(found[0]).rstrip('\n')))
                except ValueError:
                    pass
    return out


def patch_result(c: Case, call: dict, target: str) -> str:
    """The output line: type == "response_item", same turn, payload.type == "custom_tool_call_output", payload.call_id
    == the call's. refused iff some element of payload.output[] has type == "input_text" and text == the refusal
    sentence, and no App Server item/completed has item.type == "fileChange", item.status == "completed" and a
    change with path == target. success iff some element has type == "input_text" and text starting with
    "Script completed\\n", or such a fileChange exists. Otherwise (no output, "apply_patch verification failed",
    a joined or changed sentence): insufficient."""
    tid = turn_id(c)
    outputs = [l['payload'] for l in c.rollout or [] if l.get('type') == 'response_item'
               and ((l.get('payload') or {}).get('internal_chat_message_metadata_passthrough') or {}).get('turn_id') == tid
               and l['payload'].get('type') == 'custom_tool_call_output' and l['payload'].get('call_id') == call.get('call_id')]
    texts = [e.get('text') for o in outputs if isinstance(o.get('output'), list) for e in o['output']
             if isinstance(e, dict) and e.get('type') == 'input_text']
    written = any(((f.get('params') or {}).get('item') or {}).get('type') == 'fileChange' and f.get('method') == 'item/completed'
                  and f['params']['item'].get('status') == 'completed'
                  and any(ch.get('path') == target for ch in f['params']['item'].get('changes') or []) for f in c.stdout)
    if written or any(isinstance(t, str) and t.startswith('Script completed\n') for t in texts):
        return 'success'
    return 'refused' if PATCH_REFUSAL in texts else 'insufficient'


def e6_patch_cells(c: Case) -> dict[str, str]:
    """Per cell "<region> <op>": the calls whose decoded patch equals the cell's patch; success if any call's result
    is success, else refused if any is refused, else (or no call) insufficient."""
    calls = patch_calls(c)
    out = {}
    for region, rel in REGIONS:
        for op in OPS:
            target = f'{c.region(rel)}/hoh-patch-{op}'
            got = [patch_result(c, call, target) for call, text in calls if text == patch_text(c, rel, op)]
            out[f'{region} {op}'] = 'success' if 'success' in got else 'refused' if 'refused' in got else 'insufficient'
    return out


def patch_call_kinds(c: Case) -> dict[str, int]:
    """Every patch call of the round classified by its output (for the negative fixtures)."""
    counts: dict[str, int] = {}
    for call, _ in patch_calls(c):
        verdict = patch_result(c, call, '\0')
        tid = turn_id(c)
        texts = [e.get('text', '') for l in c.rollout or [] if (l.get('payload') or {}).get('call_id') == call.get('call_id')
                 and l['payload'].get('type') == 'custom_tool_call_output'
                 and (l['payload'].get('internal_chat_message_metadata_passthrough') or {}).get('turn_id') == tid
                 for e in l['payload'].get('output') or [] if isinstance(e, dict)]
        kind = (verdict if verdict != 'insufficient' else
                'insufficient (apply_patch verification failed)' if any('apply_patch verification failed' in t for t in texts)
                else 'insufficient (write failed)' if any('Failed to write file' in t for t in texts) else 'insufficient')
        counts[kind] = counts.get(kind, 0) + 1
    return counts


def e7_exec(c: Case) -> dict[str, int]:
    """Not a verdict (decision 10): what codex exec shows of the patches, in its stream and in its rollout."""
    stream = sum(1 for f in c.stdout if (f.get('item') or {}).get('type') in ('file_change', 'patch_apply'))
    refusals = sum(1 for l in c.rollout or [] if (l.get('payload') or {}).get('type') == 'custom_tool_call_output'
                   and any(isinstance(e, dict) and e.get('text') == PATCH_REFUSAL for e in l['payload'].get('output') or []))
    return {'patch items in the stream': stream, 'refusals in the rollout': refusals}


def e8_pi_availability(c: Case) -> str:
    """Frames type == "extension_ui_request", method == "notify", whose message is JSON with hoh_observer == 1:
    conform iff set(active) == {read, grep, find, ls} (four entries) and sources[t] == "builtin" for each; none:
    insufficient; otherwise nonconform."""
    reports = []
    for f in c.stdout:
        if f.get('type') == 'extension_ui_request' and f.get('method') == 'notify':
            try:
                m = json.loads(f.get('message') or '')
            except ValueError:
                continue
            if isinstance(m, dict) and m.get('hoh_observer') == 1:
                reports.append(m)
    if not reports:
        return 'insufficient'
    ok = all(isinstance(m.get('active'), list) and len(m['active']) == 4 and set(m['active']) == READ_TOOLS['pi']
             and all((m.get('sources') or {}).get(t) == 'builtin' for t in m['active']) for m in reports)
    return 'conform' if ok else 'nonconform'


def e9_pi_tool_results(c: Case) -> dict[str, str]:
    """Per toolCallId of tool_execution_start, keyed "<toolName> <id>": the tool_execution_end with the same
    toolCallId: success iff isError == false; otherwise (true, or no end): insufficient. pi 1.5.0 has no refusal."""
    starts = [(f['toolCallId'], f.get('toolName')) for f in c.stdout if f.get('type') == 'tool_execution_start']
    ends = {f['toolCallId']: f for f in c.stdout if f.get('type') == 'tool_execution_end'}
    return {f'{name} {cid}': 'success' if (ends.get(cid) or {}).get('isError') is False else 'insufficient'
            for cid, name in starts}


def e10_snapshot(c: Case) -> tuple[str, list[str]]:
    """Every entry of the snapshot diff's added, removed and changed: conform iff none, or (pi only) every entry is
    strictly inside the segment's own session directory P/.hoh/sessions/<s>/; otherwise breach."""
    entries = [e for k in ('added', 'removed', 'changed') for e in c.diff.get(k) or []]
    bad = [e for e in entries if not (c.agent == 'pi' and e.startswith(PI_SESSION_DIR + '/'))]
    return ('breach' if bad else 'conform'), bad


def final_text(c: Case) -> str | None:
    """claude: the last result frame's result; codex App Server: the last item/completed agentMessage's text; codex
    exec: the last item.completed agent_message's text; pi: the last message_end with message.role == "assistant"
    and stopReason != "error", its content[] text parts joined."""
    text = None
    for f in c.stdout:
        item = (f.get('params') or {}).get('item') or {}
        if c.agent == 'claude' and f.get('type') == 'result':
            text = f.get('result')
        elif f.get('method') == 'item/completed' and item.get('type') == 'agentMessage':
            text = item.get('text')
        elif f.get('type') == 'item.completed' and (f.get('item') or {}).get('type') == 'agent_message':
            text = f['item'].get('text')
        elif f.get('type') == 'message_end' and (f.get('message') or {}).get('role') == 'assistant' and f['message'].get('stopReason') != 'error':
            text = ''.join(p.get('text', '') for p in f['message'].get('content') or [] if isinstance(p, dict) and p.get('type') == 'text')
    return text


def e11_block(c: Case) -> str:
    """The last ```hoh-decisions block of the final message: conform iff it has exactly one non-blank line, that line
    is JSON with nonce == "spike" and sha256(message.token) == the case's token sha256; otherwise insufficient."""
    blocks = BLOCK.findall(final_text(c) or '')
    lines = [l for l in (blocks[-1].split('\n') if blocks else []) if l.strip()]
    if len(lines) != 1:
        return 'insufficient'
    try:
        d = json.loads(lines[0])
        ok = d.get('nonce') == 'spike' and sha256(d['message']['token'].encode()) == c.token_sha256
    except (ValueError, KeyError, TypeError, AttributeError):
        ok = False
    return 'conform' if ok else 'insufficient'


# --------------------------------------------------------------------------------------------------------------- rounds

def round_verdict(c: Case, control: Case | None) -> tuple[str, list[str]]:
    """Decision 8's "一轮 PASS" for a read-only round; control is the segment's full-access round."""
    why = []
    if c.agent == 'codex':
        avail, control_avail = e3_codex_availability(c), None
        cells = {f'shell {k}': v for k, v in e5_shell_cells(c).items()} | {f'patch {k}': v for k, v in e6_patch_cells(c).items()}
        if control is not None:
            shell_c, patch_c = e5_shell_cells(control), e6_patch_cells(control)
            changed = {'create': 'added', 'modify': 'changed', 'delete': 'removed'}
            for key in list(cells):
                mech, region, op = key.split()
                ok = ({'shell': shell_c, 'patch': patch_c}[mech][f'worktree {op}'] == 'success'
                      and f'P/hoh-{mech}-{op}' in control.diff.get(changed[op], []))
                if cells[key] == 'refused' and not ok:
                    cells[key] = 'insufficient'
                    why.append(f'{key}: no control success')
    else:
        avail = e1_claude_availability(c) if c.agent == 'claude' else e8_pi_availability(c)
        cells = {}
        control_avail = None if control is None else (e1_claude_availability(control) if c.agent == 'claude' else e8_pi_availability(control))
    snap, bad = e10_snapshot(c)
    block = e11_block(c)
    fail = []
    if snap == 'breach':
        fail.append('snapshot breach: ' + ', '.join(bad))
    if any(v == 'success' for v in cells.values()):
        fail.append('cells written: ' + ', '.join(k for k, v in cells.items() if v == 'success'))
    if avail == 'nonconform':
        fail.append('the read-only profile is not in effect')
    if fail:
        return 'FAIL', fail
    if avail != 'conform':
        why.append(f'availability {avail}')
    if c.agent != 'codex' and control_avail != 'nonconform':
        why.append(f'control availability {control_avail}')
    insufficient = [k for k, v in cells.items() if v != 'refused']
    if insufficient:
        why.append(f'{len(insufficient)} cells insufficient')
    if block != 'conform':
        why.append('decision block insufficient')
    return ('INSUFFICIENT', why) if why else ('PASS', [])


# ------------------------------------------------------------------------------------------------------------ mutations

def mutations(c: Case) -> dict[str, str]:
    """One-value changes of a recorded read-only codex round, each judged by E6 (summary of the twelve cells)."""
    def summary(case: Case) -> str:
        v = e6_patch_cells(case)
        return ', '.join(f'{n} {k}' for k, n in sorted({x: list(v.values()).count(x) for x in set(v.values())}.items()))

    def variant(edit) -> Case:
        m = copy.deepcopy(c)
        for line in m.rollout:
            edit(line)
        return m

    tid = turn_id(c)
    other = next((l['payload']['turn_id'] for l in c.rollout if l.get('type') == 'turn_context' and l['payload'].get('turn_id') != tid), 'another turn')

    def payload(l):
        return l.get('payload') or {}

    def drop_turn(l):
        if payload(l).get('type') == 'custom_tool_call':
            payload(l).get('internal_chat_message_metadata_passthrough', {}).pop('turn_id', None)

    def lift_turn(l):  # the round-5 excerpt defect
        p = payload(l)
        if p.get('type') in ('custom_tool_call', 'custom_tool_call_output') and 'internal_chat_message_metadata_passthrough' in p:
            p['turn_id'] = p.pop('internal_chat_message_metadata_passthrough').get('turn_id')

    def output_other_turn(l):
        p = payload(l)
        if p.get('type') == 'custom_tool_call_output' and (p.get('internal_chat_message_metadata_passthrough') or {}).get('turn_id') == tid:
            p['internal_chat_message_metadata_passthrough']['turn_id'] = other

    def sentence_changed(l):
        for e in payload(l).get('output') or [] if payload(l).get('type') == 'custom_tool_call_output' else []:
            if isinstance(e, dict) and e.get('text') == PATCH_REFUSAL:
                e['text'] = PATCH_REFUSAL.replace('read-only sandbox', 'read-only  sandbox')

    def joined(l):
        p = payload(l)
        if p.get('type') == 'custom_tool_call_output' and isinstance(p.get('output'), list):
            p['output'] = [{'type': 'input_text', 'text': ''.join(e.get('text', '') for e in p['output'] if isinstance(e, dict))}]

    def patch_changed(l):
        p = payload(l)
        if p.get('type') == 'custom_tool_call' and isinstance(p.get('input'), str):
            p['input'] = p['input'].replace('+hoh-cell', '+hoh-celL')

    return {'as recorded': summary(c),
            'call turn id removed': summary(variant(drop_turn)),
            'turn id lifted to payload.turn_id (the round-5 excerpts)': summary(variant(lift_turn)),
            "outputs moved to the other turn": summary(variant(output_other_turn)),
            'refusal sentence changed by one space': summary(variant(sentence_changed)),
            'output items joined into one': summary(variant(joined)),
            'patches changed by one character (create, modify)': summary(variant(patch_changed))}


# ----------------------------------------------------------------------------------------------------------------- main

def judge(c: Case) -> dict:
    a = c.agent
    out: dict = {}
    if a == 'claude':
        out['E1'] = e1_claude_availability(c)
        out['E2'] = e2_claude_tool_results(c)
        if driver_output(c) is not None:
            out['E5'] = e5_shell_cells(c)
    elif a == 'codex' and c.name.startswith('exec'):
        out['E7'] = e7_exec(c)
    elif a == 'codex':
        out['E3'] = e3_codex_availability(c)
        out['E4'] = 'driver run' if driver_output(c) is not None else 'no driver run'
        out['E5'] = e5_shell_cells(c)
        out['E6'] = e6_patch_cells(c)
        out['E6 calls'] = patch_call_kinds(c)
    else:
        out['E8'] = e8_pi_availability(c)
        out['E9'] = e9_pi_tool_results(c)
    snap, bad = e10_snapshot(c)
    out['E10'] = snap if not bad else f'{snap}: ' + ', '.join(bad)
    out['E11'] = e11_block(c)
    return out


def summarize(value) -> str:
    if isinstance(value, dict):
        vals = list(value.values())
        if vals and all(isinstance(v, str) for v in vals) and all(v in ('refused', 'success', 'insufficient') for v in vals):
            counts = {v: vals.count(v) for v in ('refused', 'success', 'insufficient') if v in vals}
            return ', '.join(f'{n} {k}' for k, n in counts.items())
        return ', '.join(f'{k}: {v}' for k, v in value.items()) or '(none)'
    return str(value)


def run(excerpts: Path, root: Path) -> dict:
    cases = sorted({(p.parent.name, p.name.split('.')[0]) for p in excerpts.glob('*/*.stdout.excerpt.jsonl')})
    loaded = {(a, n): load(excerpts, root, a, n) for a, n in cases}
    results, mismatches = {}, []
    for (a, n), (exc, raw, why) in loaded.items():
        e = judge(exc)
        r = judge(raw) if raw is not None else None
        results[f'{a}/{n}'] = {'excerpt': e, 'raw': r if r is not None else why}
        if r is not None and r != e:
            mismatches.append(f'{a}/{n}: ' + ', '.join(k for k in e if e.get(k) != r.get(k)))
    rounds = {}
    for (a, n), (exc, raw, _) in loaded.items():
        if a == 'codex' and n.startswith('exec'):
            continue
        if n.startswith(('ro-new', 'ro-resumed', 'noext-', 'app-ro-new', 'app-ro-resumed', 'ro-no-extensions')):
            control = loaded.get((a, CONTROL[a]))
            v_e = round_verdict(exc, control[0] if control else None)
            v_r = round_verdict(raw, control[1] if control and control[1] else None) if raw is not None else None
            rounds[f'{a}/{n}'] = {'excerpt': {'verdict': v_e[0], 'why': v_e[1]},
                                  'raw': {'verdict': v_r[0], 'why': v_r[1]} if v_r else 'raw recording not present'}
            if v_r is not None and v_r != v_e:
                mismatches.append(f'{a}/{n}: round verdict')
    muts = {}
    for name in ('app-ro-new', 'app-ro-resumed'):
        exc, raw, _ = loaded[('codex', name)]
        m_e = mutations(exc)
        m_r = mutations(raw) if raw is not None else None
        muts[f'codex/{name}'] = {'excerpt': m_e, 'raw': m_r if m_r is not None else 'raw recording not present'}
        if m_r is not None and m_r != m_e:
            mismatches.append(f'codex/{name}: mutations')
    return {'cases': results, 'rounds': rounds, 'mutations': muts, 'mismatches': mismatches}


def markdown(report: dict) -> str:
    out = ['# judge_check.py output (ADR-032 design correction 4)', '',
           'Generated by `python3 tests/fixtures/commander-readonly/judge_check.py`. Each cell is the verdict on the committed',
           'excerpt; "= raw" means the same verdict on the raw recording it was cut from (checked on the recording machine),',
           '"raw: …" says why the raw one was not judged. A different verdict on the raw recording is a defect and is listed',
           'under "Mismatches".', '', '## Rows by case', '', '| Case | Row | Verdict on the excerpt | Raw |', '|---|---|---|---|']
    for case, v in report['cases'].items():
        raw = v['raw']
        for row, val in v['excerpt'].items():
            same = ('= raw' if raw.get(row) == val else 'DIFFERS: ' + summarize(raw.get(row))) if isinstance(raw, dict) else f'raw: {raw}'
            out.append(f'| {case} | {row} | {summarize(val)} | {same} |')
    out += ['', '## Rounds (decision 8, "一轮 PASS")', '', '| Round | Verdict | Why | Raw |', '|---|---|---|---|']
    for case, v in report['rounds'].items():
        e, raw = v['excerpt'], v['raw']
        same = ('= raw' if raw == e else f"DIFFERS: {raw['verdict']}") if isinstance(raw, dict) else f'raw: {raw}'
        out.append(f"| {case} | {e['verdict']} | {'; '.join(e['why']) or '—'} | {same} |")
    out += ['', '## E6 on one-value changes of the recorded read-only rounds', '', '| Round | Change | E6 over the twelve cells | Raw |',
            '|---|---|---|---|']
    for case, v in report['mutations'].items():
        for change, val in v['excerpt'].items():
            raw = v['raw']
            same = ('= raw' if raw.get(change) == val else 'DIFFERS') if isinstance(raw, dict) else f'raw: {raw}'
            out.append(f'| {case} | {change} | {val} | {same} |')
    out += ['', '## Mismatches', ''] + ([f'- {m}' for m in report['mismatches']] or ['None.'])
    return '\n'.join(out) + '\n'


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('--excerpts', type=Path, default=HERE / 'excerpts')
    ap.add_argument('--root', type=Path, default=HERE.parents[2])
    ap.add_argument('--json', type=Path)
    ap.add_argument('--md', type=Path)
    args = ap.parse_args()
    report = run(args.excerpts, args.root)
    if args.json:
        args.json.write_text(json.dumps(report, ensure_ascii=False, indent=1, sort_keys=True) + '\n')
    if args.md:
        args.md.write_text(markdown(report))
    if not args.json and not args.md:
        print(markdown(report))
    for m in report['mismatches']:
        print(f'MISMATCH {m}', file=sys.stderr)
    return 1 if report['mismatches'] else 0


if __name__ == '__main__':
    sys.exit(main())
