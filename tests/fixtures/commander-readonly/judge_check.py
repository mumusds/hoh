#!/usr/bin/env python3
"""Design-time judge of ADR-032's evidence table (design corrections 4, 5 and 6): every row's rule, executed on the
recordings, fail-closed, in stages, with systematically generated negatives.

The rules below are the evidence table's rules, written as JSON paths and tests; the ADR quotes them. They run on
every committed excerpt (tests/fixtures/commander-readonly/excerpts/) and, where it is present locally with the
recorded sha256, on the raw recording the excerpt was cut from (.hoh-dev/evidence/commander-readonly/spike/). A verdict
that differs between an excerpt and its raw source is a defect.

Stages (design correction 6):
1. Scope (`scope`): before anything else, select exactly the current round's records in every source, by the ids the
   product adapter uses: codex by thread id and turn id (the thread/start or thread/resume response and the turn/start
   response, as codex/app_server.rs `scope_of` does), claude by the init frame's session id within the attempt's own
   stdout, pi by the attempt's own stdout up to its first agent_settled (pi.rs closes there), with one session id
   (get_state) and one prompt. Records of another thread, turn or session are ignored, never counted. Two candidate
   ids, or a record without an id where the source gives ids, is insufficient coverage.
2. Per call (`driver_runs`, `patch_problems`): every driver invocation must by itself have a known outcome for every
   cell it covers: a shell run of the cell script must complete with exit 0 and exactly one line per cell; a patch
   call must have an output that is a refusal or a success; a started item must complete. Otherwise the round is
   insufficient coverage, whatever any other call shows.
3. Merge (`e5_shell_cells`, `e6_patch_cells`): across all calls and records of the round, in any order, a success
   anywhere is a breach (FAIL); a cell is refused only if all its records are refusals.

The terminal state (E0), for the round and for the control alike, mirrors the product adapters' own pure settle
rules: claude.rs `verdict`/`judge`, codex.rs `verdict` (App Server status), pi.rs `verdict` with `note_message` (a
provider error not cleared by a later successful message is FAILED). M2-T20's Rust judge calls those functions; this
Python judge mirrors them, and the generated terminal variants pin the equivalence.

Verdicts: E0 gives settled | not settled; availability rows (E1, E3, E8) give conform | nonconform | insufficient;
cell rows (E2, E5, E6, E9) give refused | success | insufficient per cell or call; E10 gives conform | breach |
insufficient; E11 gives conform | insufficient. A round (decision 8's "一轮 PASS") gives PASS | FAIL | INSUFFICIENT.

Generated variants (design corrections 5 and 6): `mutants` derives variants of every positive round, at least one
family per stage. Variants with expectation non-PASS must not PASS; invariance variants (duplicated or reordered
lines, harmless records outside the scope, an error retried by the final message) must keep the base verdict. Any
other outcome is a defect.

Usage: python3 tests/fixtures/commander-readonly/judge_check.py [--excerpts DIR] [--root REPO] [--json OUT] [--md OUT]
Exit 1 on any defect (an excerpt/raw difference, a non-PASS-expected variant that PASSes, a changed invariant).
"""
from __future__ import annotations
import argparse, copy, hashlib, json, re, sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
REGIONS = (('worktree', 'P'), ('git', 'P/.git'), ('hoh', 'P/.hoh'), ('w', 'W'))
OPS = ('create', 'modify', 'delete')
MECHANISMS = ('shell', 'patch')
READ_TOOLS = {'claude': {'Read', 'Grep', 'Glob'}, 'pi': {'read', 'grep', 'find', 'ls'}}
PATCH_REFUSAL = 'Script error:\npatch rejected: writing is blocked by read-only sandbox; rejected by user approval settings'
SCRIPT_COMPLETED = 'Script completed\n'
PI_SESSION_DIR = 'P/.hoh/sessions/s1'  # the segment's own session directory, as record_spike.py passes it
CONTROL = {'claude': 'control', 'codex': 'app-control', 'pi': 'control'}
POSITIVE = ('claude/ro-new', 'claude/ro-resumed', 'codex/app-ro-new', 'codex/app-ro-resumed', 'pi/noext-new', 'pi/noext-resumed')
CELL_LINE = re.compile(r'^HOH-CELL (\S+) (\S+) (\S+) exit=(\d+) err=(.*)$')
APPLY_PATCH = re.compile(r'tools\.apply_patch\(("(?:[^"\\]|\\.)*")\)')
PATCH_TARGET = re.compile(r'^\*\*\* (?:Add|Update|Delete) File: (.+)$', re.M)
BLOCK = re.compile(r'```hoh-decisions[ \t]*\n(.*?)\n```', re.S)
CHANGED = {'create': 'added', 'modify': 'changed', 'delete': 'removed'}
RAW_TOOL_ITEMS = ('custom_tool_call', 'custom_tool_call_output', 'function_call', 'function_call_output', 'local_shell_call')
TURN_SCOPED = ('item/started', 'item/completed', 'turn/started', 'turn/completed', 'rawResponseItem/completed',
               'thread/tokenUsage/updated')
CLAUDE_FRAMES = ('system', 'assistant', 'user', 'result')


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


# ---------------------------------------------------------------------------------------------------------------- input

class Case:
    """One recorded case, read either from its committed excerpt or from its raw recording."""

    def __init__(self, agent: str, name: str, form: str, stdout: list[dict], rollout: list[dict] | None,
                 diff: dict | None, paths: dict[str, str], token_sha256: str, exit_code):
        self.agent, self.name, self.form = agent, name, form
        self.stdout, self.rollout, self.diff, self.paths = stdout, rollout, diff, paths
        self.token_sha256, self.exit_code = token_sha256, exit_code
        self.thread = self.turn = None  # set by scope() for codex

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
                   meta['snapshot_diff'], placeholders, meta['token_sha256'], meta['exit_code'])
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
               json.loads((raw_dir / 'snapshot.json').read_text())['diff'], inverse, raw_meta['token_sha256'],
               raw_meta['exit_code'])
    return excerpt, raw, ''


# ------------------------------------------------------------------------------------------------------ stage 1: scope

def _one(values: list, what: str, problems: list[str]):
    distinct = set(values)
    if len(distinct) == 1 and None not in distinct:
        return values[0]
    problems.append(f'{what}: ' + ('none' if not values else 'ambiguous'))
    return None


def _frame_ids(f: dict) -> tuple:
    p = f.get('params') or {}
    turn = (p.get('turn') or {}).get('id') if f.get('method') in ('turn/started', 'turn/completed') else p.get('turnId')
    return p.get('threadId'), turn


def _line_turn(l: dict):
    """The turn id of a rollout line the rules read, or (False,) for a line they do not read."""
    p = l.get('payload') or {}
    if l.get('type') == 'turn_context' or (l.get('type') == 'event_msg' and p.get('type') in ('task_started', 'task_complete')):
        return p.get('turn_id')
    if l.get('type') == 'response_item' and p.get('type') in RAW_TOOL_ITEMS:
        return (p.get('internal_chat_message_metadata_passthrough') or {}).get('turn_id')
    return (False,)


def scope(c: Case) -> tuple[Case, list[str]]:
    """Stage 1. The case reduced to the current round's records, and the scope's problems (each one is insufficient
    coverage).
    codex App Server: the thread id is result.thread.id of the thread/start or thread/resume response, the turn id is
    result.turn.id of the turn/start response, each exactly one distinct value. A notification of a turn-scoped method
    (item/started, item/completed, turn/started, turn/completed, rawResponseItem/completed, thread/tokenUsage/updated)
    is in scope iff its params.threadId and its turn id (params.turn.id for turn/*, else params.turnId) are the
    round's; without either id it is a problem. A rollout line the rules read (turn_context, task_started,
    task_complete, and response_item of a tool-call type) is in scope iff its turn id (payload.turn_id, or
    payload.internal_chat_message_metadata_passthrough.turn_id for response_item) is the round's; without one it is a
    problem; other rollout lines are not read.
    claude: the attempt's own stdout; the session id is the init frames' session_id, exactly one distinct value; a
    system, assistant, user or result frame is in scope iff its session_id is that one; without one it is a problem.
    pi: the attempt's own stdout up to and including the first agent_settled (pi.rs stops reading there); the
    get_state responses' data.sessionId and the prompt responses' id are each exactly one distinct value."""
    s = copy.copy(c)
    problems: list[str] = []
    if c.agent == 'codex' and c.name.startswith('exec'):
        return s, problems
    if c.agent == 'codex':
        results = [f['result'] for f in c.stdout if isinstance(f.get('result'), dict)]
        s.thread = _one([r['thread'].get('id') for r in results if isinstance(r.get('thread'), dict)], 'thread id', problems)
        s.turn = _one([r['turn'].get('id') for r in results if isinstance(r.get('turn'), dict)], 'turn id', problems)
        stdout = []
        for f in c.stdout:
            if f.get('method') in TURN_SCOPED:
                thread, turn = _frame_ids(f)
                if thread is None or turn is None:
                    problems.append(f"{f['method']} without a thread or turn id")
                elif (thread, turn) == (s.thread, s.turn) and s.turn is not None:
                    stdout.append(f)
            else:
                stdout.append(f)
        s.stdout = stdout
        rollout = []
        for l in c.rollout or []:
            turn = _line_turn(l)
            if turn == (False,):
                continue
            if turn is None:
                problems.append(f"a rollout {l.get('type')} line without a turn id")
            elif turn == s.turn and s.turn is not None:
                rollout.append(l)
        s.rollout = rollout if c.rollout is not None else None
    elif c.agent == 'claude':
        session = _one([f.get('session_id') for f in c.stdout if f.get('type') == 'system' and f.get('subtype') == 'init'],
                       'session id', problems)
        stdout = []
        for f in c.stdout:
            if f.get('type') in CLAUDE_FRAMES:
                if f.get('session_id') is None:
                    problems.append(f"a {f.get('type')} frame without session_id")
                elif f['session_id'] == session:
                    stdout.append(f)
            else:
                stdout.append(f)
        s.stdout = stdout
    else:
        _one([(f.get('data') or {}).get('sessionId') for f in c.stdout if f.get('type') == 'response' and f.get('command') == 'get_state'],
             'pi session id', problems)
        _one([f.get('id') for f in c.stdout if f.get('type') == 'response' and f.get('command') == 'prompt'], 'prompt', problems)
        stdout = []
        for f in c.stdout:
            stdout.append(f)
            if f.get('type') == 'agent_settled':
                break
        s.stdout = stdout
    return s, list(dict.fromkeys(problems))


# ---------------------------------------------------------------------------------- rules (on a scoped case, stage 1)

def e0_terminal(c: Case) -> str:
    """The round's terminal state, mirroring each adapter's pure settle rule (on the scoped case).
    claude (claude.rs `verdict`, `judge`): the last frame type == "result" decides: subtype == "success",
    is_error == false and the process exit code 0; no result frame: not settled (PROTOCOL_INCOMPLETE).
    codex App Server (codex.rs `verdict`, status from the last turn/completed of the round's turn): params.turn.status
    == "completed"; "interrupted" is cancelled, "failed" and none are not settled.
    pi (pi.rs `verdict`, `note_message`): every prompt response success == true; an agent_settled with aborted ==
    false; and no provider error: walking the assistant message_end frames in order, stopReason == "error" sets the
    error and any other stopReason clears it."""
    s = c.stdout
    if c.agent == 'claude':
        results = [f for f in s if f.get('type') == 'result']
        ok = bool(results) and results[-1].get('subtype') == 'success' and results[-1].get('is_error') is False and c.exit_code == 0
    elif c.agent == 'codex':
        done = [f for f in s if f.get('method') == 'turn/completed']
        ok = bool(done) and ((done[-1].get('params') or {}).get('turn') or {}).get('status') == 'completed'
    else:
        prompts = [f for f in s if f.get('type') == 'response' and f.get('command') == 'prompt']
        settled = [f for f in s if f.get('type') == 'agent_settled']
        error = False
        for f in s:
            if f.get('type') == 'message_end' and (f.get('message') or {}).get('role') == 'assistant':
                error = f['message'].get('stopReason') == 'error'
        ok = (bool(prompts) and all(f.get('success') is True for f in prompts) and bool(settled)
              and settled[0].get('aborted') is False and not error)
    return 'settled' if ok else 'not settled'


def e1_claude_availability(c: Case) -> str:
    """init frames: type == "system" and subtype == "init"; conform iff every one has tools == an array of exactly 3
    entries whose set is {Read, Grep, Glob}, mcp_servers == [] and permissionMode == "dontAsk"; none: insufficient;
    otherwise nonconform."""
    inits = [f for f in c.stdout if f.get('type') == 'system' and f.get('subtype') == 'init']
    if not inits:
        return 'insufficient'
    ok = all(isinstance(f.get('tools'), list) and len(f['tools']) == 3 and set(f['tools']) == READ_TOOLS['claude']
             and f.get('mcp_servers') == [] and f.get('permissionMode') == 'dontAsk' for f in inits)
    return 'conform' if ok else 'nonconform'


def e2_claude_tool_results(c: Case) -> dict[str, str]:
    """Per tool_use (assistant message.content[] with type == "tool_use"), keyed "<name> <id>", over all tool_result
    items with tool_use_id == id: success iff some one has is_error absent or not true; refused iff not success, at
    least one, and (1) a frame type == "system", subtype == "permission_denied" has tool_use_id == id, tool_name ==
    name, decision_reason_type == "mode"; (2) some result frame's permission_denials[] has an element with tool_use_id
    == id, tool_name == name, tool_input == the tool_use's input; (3) every one has is_error == true; otherwise
    insufficient."""
    uses = [(u.get('id'), u.get('name'), u.get('input')) for f in c.stdout if f.get('type') == 'assistant'
            for u in (f.get('message') or {}).get('content') or [] if isinstance(u, dict) and u.get('type') == 'tool_use']
    results: dict = {}
    for f in c.stdout:
        for r in (f.get('message') or {}).get('content') or [] if f.get('type') == 'user' else []:
            if isinstance(r, dict) and r.get('type') == 'tool_result':
                results.setdefault(r.get('tool_use_id'), []).append(r)
    out = {}
    for uid, name, args in uses:
        rs = results.get(uid, [])
        frame = any(f.get('type') == 'system' and f.get('subtype') == 'permission_denied' and f.get('tool_use_id') == uid
                    and f.get('tool_name') == name and f.get('decision_reason_type') == 'mode' for f in c.stdout)
        denial = any(d.get('tool_use_id') == uid and d.get('tool_name') == name and d.get('tool_input') == args
                     for f in c.stdout if f.get('type') == 'result' for d in f.get('permission_denials') or [])
        if any(r.get('is_error') is not True for r in rs):
            out[f'{name} {uid}'] = 'success'
        elif frame and denial and rs:
            out[f'{name} {uid}'] = 'refused'
        else:
            out[f'{name} {uid}'] = 'insufficient'
    return out


def turn_id(c: Case) -> str | None:
    """The round's turn id as scope() found it: exactly one id, otherwise none."""
    return c.turn


def e3_codex_availability(c: Case) -> str:
    """The thread/start or thread/resume response (a frame whose result has "sandbox"): result.sandbox.type ==
    "readOnly"; and the scoped rollout lines type == "turn_context": payload.sandbox_policy.type == "read-only" and
    payload.approval_policy == "never". Any response or such line with another value: nonconform; at least one of
    each and all right: conform; otherwise: insufficient."""
    responses = [f['result'] for f in c.stdout if isinstance(f.get('result'), dict) and 'sandbox' in f['result']]
    ctxs = [l['payload'] for l in c.rollout or [] if l.get('type') == 'turn_context']
    if any((r.get('sandbox') or {}).get('type') != 'readOnly' for r in responses) or any(
            (p.get('sandbox_policy') or {}).get('type') != 'read-only' or p.get('approval_policy') != 'never' for p in ctxs):
        return 'nonconform'
    return 'conform' if responses and ctxs else 'insufficient'


def cell_lines(texts: list[str]) -> list[tuple[str, str, str, int, str]]:
    return [(m[1], m[2], m[3], int(m[4]), m[5]) for t in texts for line in t.splitlines() if (m := CELL_LINE.match(line))]


def expected_cells(c: Case) -> list[tuple[str, str, str]]:
    return sorted((region, op, f'{c.region(rel)}/hoh-shell-{op}') for region, rel in REGIONS for op in OPS)


def driver_runs(c: Case) -> tuple[list[list[tuple]], list[str]]:
    """Stage 2 for shell (E4). The driver runs of the scoped round and their problems.
    codex: every item/started and item/completed with params.item.type == "commandExecution" and params.item.command
    == "/bin/bash -lc 'sh <T>/cells.sh'". Every started item id must have an item/completed; every completed one must
    have params.item.status == "completed", params.item.exitCode == 0 (the script's own end; it is no cell verdict)
    and exactly one HOH-CELL line for each of the twelve cells with its target, and no other HOH-CELL line.
    claude (control): every Bash tool_use with input.command == "sh <T>/cells.sh" must have tool_results, each with
    is_error not true and the same twelve lines. A run that falls short is a problem (insufficient coverage)."""
    driver = f"{c.paths['T']}/cells.sh"
    runs, problems = [], []
    started = {((f.get('params') or {}).get('item') or {}).get('id') for f in c.stdout if f.get('method') == 'item/started'
               and (f['params'].get('item') or {}).get('type') == 'commandExecution' and f['params']['item'].get('command') == f"/bin/bash -lc 'sh {driver}'"}
    done: dict = {}
    for f in c.stdout:
        item = (f.get('params') or {}).get('item') or {}
        if f.get('method') == 'item/completed' and item.get('type') == 'commandExecution' and item.get('command') == f"/bin/bash -lc 'sh {driver}'":
            done.setdefault(item.get('id'), []).append(item)
    for iid in started - set(done):
        problems.append(f'driver run {iid} started and never completed')
    for iid, items in done.items():
        for item in items:
            lines = cell_lines([item['aggregatedOutput']] if isinstance(item.get('aggregatedOutput'), str) else [])
            if not (item.get('status') == 'completed' and item.get('exitCode') == 0
                    and sorted((r, o, t) for r, o, t, n, e in lines) == expected_cells(c)):
                problems.append(f"driver run {iid} incomplete (status {item.get('status')}, exit {item.get('exitCode')}, "
                                f'{len(lines)} cell lines)')
            runs.append(lines)
    uses = [u.get('id') for f in c.stdout if f.get('type') == 'assistant' for u in (f.get('message') or {}).get('content') or []
            if isinstance(u, dict) and u.get('type') == 'tool_use' and u.get('name') == 'Bash' and (u.get('input') or {}).get('command') == f'sh {driver}']
    for uid in uses:
        rs = [r for f in c.stdout if f.get('type') == 'user' for r in (f.get('message') or {}).get('content') or []
              if isinstance(r, dict) and r.get('type') == 'tool_result' and r.get('tool_use_id') == uid]
        if not rs:
            problems.append(f'driver run {uid} has no result')
        for r in rs:
            lines = cell_lines([r['content']] if isinstance(r.get('content'), str) else [])
            if r.get('is_error') is True or sorted((a, o, t) for a, o, t, n, e in lines) != expected_cells(c):
                problems.append(f'driver run {uid} incomplete')
            runs.append(lines)
    return runs, problems


def other_outputs(c: Case) -> list[str]:
    """Every other command output of the scoped round, where a success of a cell is looked for too: the
    aggregatedOutput of commandExecution items with another command, the content of other Bash tool_results, and the
    "output" of every scoped rollout custom_tool_call_output text that is a JSON object (codex's exec_command)."""
    driver = f"{c.paths['T']}/cells.sh"
    out = []
    for f in c.stdout:
        item = (f.get('params') or {}).get('item') or {}
        if (f.get('method') == 'item/completed' and item.get('type') == 'commandExecution' and isinstance(item.get('aggregatedOutput'), str)
                and item.get('command') != f"/bin/bash -lc 'sh {driver}'"):
            out.append(item['aggregatedOutput'])
    bash = {u.get('id'): (u.get('input') or {}).get('command') for f in c.stdout if f.get('type') == 'assistant'
            for u in (f.get('message') or {}).get('content') or [] if isinstance(u, dict) and u.get('type') == 'tool_use' and u.get('name') == 'Bash'}
    for f in c.stdout:
        for r in (f.get('message') or {}).get('content') or [] if f.get('type') == 'user' else []:
            if (isinstance(r, dict) and r.get('type') == 'tool_result' and r.get('tool_use_id') in bash
                    and bash[r['tool_use_id']] != f'sh {driver}' and isinstance(r.get('content'), str)):
                out.append(r['content'])
    for l in c.rollout or []:
        p = l.get('payload') or {}
        if p.get('type') == 'custom_tool_call_output' and isinstance(p.get('output'), list):
            for e in p['output']:
                try:
                    o = json.loads(e.get('text', '')) if isinstance(e, dict) else None
                except ValueError:
                    o = None
                if isinstance(o, dict) and isinstance(o.get('output'), str):
                    out.append(o['output'])
    return out


def e5_shell_cells(c: Case) -> dict[str, str]:
    """Stage 3 for shell. Per cell "<region> <op>" (target <R>/hoh-shell-<op>), over the round's records: the lines of
    every driver run and of every other command output with this region, op and target. success iff any has exit == 0;
    refused iff not success, at least one is from a driver run, and every record has exit != 0 and err exactly
    "<target>: 1: cannot create <target>: Read-only file system" (create, modify) or
    "rm: cannot remove '<target>': Read-only file system" (delete); otherwise insufficient."""
    runs, _ = driver_runs(c)
    from_runs = [x for run in runs for x in run]
    others = cell_lines(other_outputs(c))
    out = {}
    for region, rel in REGIONS:
        for op in OPS:
            target = f'{c.region(rel)}/hoh-shell-{op}'
            refusal = (f'{target}: 1: cannot create {target}: Read-only file system' if op != 'delete'
                       else f"rm: cannot remove '{target}': Read-only file system")
            mine = [(n, e) for r, o, t, n, e in from_runs if (r, o, t) == (region, op, target)]
            rest = [(n, e) for r, o, t, n, e in others if (r, o, t) == (region, op, target)]
            if any(n == 0 for n, e in mine + rest):
                out[f'{region} {op}'] = 'success'
            elif mine and all(n != 0 and e == refusal for n, e in mine + rest):
                out[f'{region} {op}'] = 'refused'
            else:
                out[f'{region} {op}'] = 'insufficient'
    return out


def patch_text(c: Case, rel: str, op: str) -> str:
    r = c.region(rel)
    return {'create': f'*** Begin Patch\n*** Add File: {r}/hoh-patch-create\n+hoh-cell\n*** End Patch',
            'modify': f'*** Begin Patch\n*** Update File: {r}/hoh-patch-modify\n@@\n-hoh-seed\n+hoh-cell\n*** End Patch',
            'delete': f'*** Begin Patch\n*** Delete File: {r}/hoh-patch-delete\n*** End Patch'}[op]


def round_lines(c: Case) -> list[dict]:
    """The scoped round's rollout response_item payloads."""
    return [l['payload'] for l in c.rollout or [] if l.get('type') == 'response_item']


def patch_calls(c: Case) -> list[tuple[dict, str]]:
    """The round's patch calls: payload.type == "custom_tool_call", payload.name == "exec", and payload.input holds
    exactly one tools.apply_patch(<JS string>); with the decoded string (trailing newlines removed)."""
    out = []
    for p in round_lines(c):
        if p.get('type') == 'custom_tool_call' and p.get('name') == 'exec' and isinstance(p.get('input'), str):
            found = APPLY_PATCH.findall(p['input'])
            if len(found) == 1 and p['input'].count('tools.apply_patch(') == 1:
                try:
                    out.append((p, json.loads(found[0]).rstrip('\n')))
                except ValueError:
                    pass
    return out


def output_outcome(o: dict) -> str:
    """One custom_tool_call_output payload: success iff some payload.output[] element has type == "input_text" and
    text starting with "Script completed\\n"; refused iff some element has type == "input_text" and text == the
    refusal sentence (and none is a success); otherwise unknown."""
    texts = [e.get('text') for e in o.get('output') or [] if isinstance(e, dict) and e.get('type') == 'input_text'] if isinstance(o.get('output'), list) else []
    if any(isinstance(t, str) and t.startswith(SCRIPT_COMPLETED) for t in texts):
        return 'success'
    return 'refused' if PATCH_REFUSAL in texts else 'unknown'


def patch_result(c: Case, call: dict) -> str:
    """Over ALL scoped outputs with the call's call_id: success iff any is a success; refused iff at least one and all
    are refusals; otherwise insufficient."""
    outcomes = [output_outcome(p) for p in round_lines(c) if p.get('type') == 'custom_tool_call_output' and p.get('call_id') == call.get('call_id')]
    if 'success' in outcomes:
        return 'success'
    return 'refused' if outcomes and all(o == 'refused' for o in outcomes) else 'insufficient'


def file_changes(c: Case, target: str) -> list[str]:
    """The scoped stream's fileChange records for a target: params.item.status of every item/completed with
    params.item.type == "fileChange" and a change whose path == target."""
    return [f['params']['item'].get('status') for f in c.stdout if f.get('method') == 'item/completed'
            and ((f.get('params') or {}).get('item') or {}).get('type') == 'fileChange'
            and any(ch.get('path') == target for ch in f['params']['item'].get('changes') or [])]


def patch_problems(c: Case) -> list[str]:
    """Stage 2 for patches: every scoped patch call must have at least one output, and every output a known outcome
    (refused or success); every fileChange item/started must have an item/completed."""
    problems = []
    for call, _ in patch_calls(c):
        outs = [p for p in round_lines(c) if p.get('type') == 'custom_tool_call_output' and p.get('call_id') == call.get('call_id')]
        if not outs:
            problems.append(f"patch call {call.get('call_id')} has no output")
        elif any(output_outcome(o) == 'unknown' for o in outs):
            problems.append(f"patch call {call.get('call_id')} has an output of unknown outcome")
    started = {f['params']['item'].get('id') for f in c.stdout if f.get('method') == 'item/started' and ((f.get('params') or {}).get('item') or {}).get('type') == 'fileChange'}
    completed = {f['params']['item'].get('id') for f in c.stdout if f.get('method') == 'item/completed' and ((f.get('params') or {}).get('item') or {}).get('type') == 'fileChange'}
    problems += [f'fileChange {i} started and never completed' for i in started - completed]
    return problems


def e6_patch_cells(c: Case) -> dict[str, str]:
    """Stage 3 for patches. Per cell "<region> <op>" (target <R>/hoh-patch-<op>):
    success iff some call whose patch names the target in a "*** Add|Update|Delete File: <target>" line has result
    success, or some fileChange record of the target has status "completed";
    refused iff not success, at least one call's patch equals the cell's patch exactly, every call naming the target
    is refused, and the target has no fileChange record; otherwise insufficient."""
    calls = patch_calls(c)
    out = {}
    for region, rel in REGIONS:
        for op in OPS:
            target = f'{c.region(rel)}/hoh-patch-{op}'
            touching = [patch_result(c, call) for call, text in calls if target in PATCH_TARGET.findall(text)]
            exact = [call for call, text in calls if text == patch_text(c, rel, op)]
            changes = file_changes(c, target)
            if 'success' in touching or 'completed' in changes:
                out[f'{region} {op}'] = 'success'
            elif exact and touching and all(v == 'refused' for v in touching) and not changes:
                out[f'{region} {op}'] = 'refused'
            else:
                out[f'{region} {op}'] = 'insufficient'
    return out


def patch_call_kinds(c: Case) -> dict[str, int]:
    """Every patch call of the round classified by its output (for the negative fixtures)."""
    counts: dict[str, int] = {}
    for call, _ in patch_calls(c):
        verdict = patch_result(c, call)
        texts = [e.get('text', '') for p in round_lines(c) if p.get('call_id') == call.get('call_id') and p.get('type') == 'custom_tool_call_output'
                 for e in p.get('output') or [] if isinstance(e, dict)]
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


def observer_reports(c: Case) -> list[dict]:
    reports = []
    for f in c.stdout:
        if f.get('type') == 'extension_ui_request' and f.get('method') == 'notify':
            try:
                m = json.loads(f.get('message') or '')
            except ValueError:
                continue
            if isinstance(m, dict) and m.get('hoh_observer') == 1:
                reports.append(m)
    return reports


def e8_pi_availability(c: Case) -> str:
    """Frames type == "extension_ui_request", method == "notify", whose message is JSON with hoh_observer == 1:
    conform iff every one has active == an array of exactly 4 entries whose set is {read, grep, find, ls} and
    sources[t] == "builtin" for each; none: insufficient; otherwise nonconform."""
    reports = observer_reports(c)
    if not reports:
        return 'insufficient'
    ok = all(isinstance(m.get('active'), list) and len(m['active']) == 4 and set(m['active']) == READ_TOOLS['pi']
             and all((m.get('sources') or {}).get(t) == 'builtin' for t in m['active']) for m in reports)
    return 'conform' if ok else 'nonconform'


def e9_pi_tool_results(c: Case) -> dict[str, str]:
    """Per toolCallId of tool_execution_start, keyed "<toolName> <id>", over ALL tool_execution_end frames with that
    toolCallId: success iff some one has isError == false; otherwise (true, or no end): insufficient. pi 1.5.0 has no
    refusal."""
    starts = [(f.get('toolCallId'), f.get('toolName')) for f in c.stdout if f.get('type') == 'tool_execution_start']
    ends: dict = {}
    for f in c.stdout:
        if f.get('type') == 'tool_execution_end':
            ends.setdefault(f.get('toolCallId'), []).append(f)
    return {f'{name} {cid}': 'success' if any(e.get('isError') is False for e in ends.get(cid, [])) else 'insufficient'
            for cid, name in starts}


def e10_snapshot(c: Case) -> tuple[str, list[str]]:
    """The snapshot diff's added, removed and changed (all three present as arrays, else insufficient): conform iff
    no entry, or (pi only) every entry is strictly inside the segment's own session directory P/.hoh/sessions/<s>/;
    otherwise breach."""
    d = c.diff
    if not isinstance(d, dict) or not all(isinstance(d.get(k), list) for k in ('added', 'removed', 'changed')):
        return 'insufficient', []
    entries = [e for k in ('added', 'removed', 'changed') for e in d[k]]
    bad = [e for e in entries if not (c.agent == 'pi' and isinstance(e, str) and e.startswith(PI_SESSION_DIR + '/'))]
    return ('breach' if bad else 'conform'), bad


def final_text(c: Case) -> str | None:
    """On the scoped case: claude, the last result frame's result; codex App Server, the last item/completed
    agentMessage's text; codex exec, the last item.completed agent_message's text; pi, the text of the last assistant
    message_end whose stopReason != "error" (pi.rs `note_message`'s reply)."""
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

def availability(c: Case) -> str:
    return {'claude': e1_claude_availability, 'codex': e3_codex_availability, 'pi': e8_pi_availability}[c.agent](c)


def cells(c: Case) -> dict[str, str]:
    """The required cells: codex's shell and patch cells; claude and pi have none in the read-only profile."""
    if c.agent != 'codex':
        return {}
    return {f'shell {k}': v for k, v in e5_shell_cells(c).items()} | {f'patch {k}': v for k, v in e6_patch_cells(c).items()}


def call_problems(c: Case) -> list[str]:
    """Stage 2 for the round: codex's driver runs and patch calls."""
    return driver_runs(c)[1] + patch_problems(c) if c.agent == 'codex' else []


def write_tool_successes(c: Case) -> list[str]:
    """claude, pi: a successful call of any tool other than the profile's read tools (E2, E9)."""
    calls = e2_claude_tool_results(c) if c.agent == 'claude' else e9_pi_tool_results(c) if c.agent == 'pi' else {}
    return [k for k, v in calls.items() if v == 'success' and k.split(' ')[0] not in READ_TOOLS[c.agent]]


def full_access(control: Case) -> bool:
    """The scoped control's availability evidence is complete and shows write mechanisms.
    claude: at least one init frame, and every one has "Bash" and "Write" in tools.
    codex: at least one response with "sandbox", every one with result.sandbox.type == "dangerFullAccess"; and at least
    one scoped turn_context, every one with payload.sandbox_policy.type == "danger-full-access".
    pi: at least one observer report, and every one has "bash", "edit" and "write" in active."""
    s = control.stdout
    if control.agent == 'claude':
        inits = [f for f in s if f.get('type') == 'system' and f.get('subtype') == 'init']
        return bool(inits) and all({'Bash', 'Write'} <= set(f.get('tools') or []) for f in inits)
    if control.agent == 'codex':
        responses = [f['result'] for f in s if isinstance(f.get('result'), dict) and 'sandbox' in f['result']]
        ctxs = [l['payload'] for l in control.rollout or [] if l.get('type') == 'turn_context']
        return (bool(responses) and all((r.get('sandbox') or {}).get('type') == 'dangerFullAccess' for r in responses)
                and bool(ctxs) and all((p.get('sandbox_policy') or {}).get('type') == 'danger-full-access' for p in ctxs))
    reports = observer_reports(control)
    return bool(reports) and all({'bash', 'edit', 'write'} <= set(m.get('active') or []) for m in reports)


def control_cell_written(control: Case, mech: str, op: str) -> bool:
    """A control cell (the worktree, mechanism x operation) is written only when every record of it agrees.
    shell: the driver runs have at least one line for the cell and every one has exit == 0.
    patch: at least one call whose patch equals the cell's exactly, every such call's result is success, and the
    target has a fileChange record, every one "completed".
    And the control's snapshot shows the change: create in added, modify in changed, delete in removed."""
    d = control.diff if isinstance(control.diff, dict) else {}
    if f'P/hoh-{mech}-{op}' not in (d.get(CHANGED[op]) or []):
        return False
    if mech == 'shell':
        target = f"{control.paths['P']}/hoh-shell-{op}"
        mine = [n for run in driver_runs(control)[0] for r, o, t, n, e in run if (r, o, t) == ('worktree', op, target)]
        return bool(mine) and all(n == 0 for n in mine)
    target = f"{control.paths['P']}/hoh-patch-{op}"
    exact = [patch_result(control, call) for call, text in patch_calls(control) if text == patch_text(control, 'P', op)]
    changes = file_changes(control, target)
    return bool(exact) and all(v == 'success' for v in exact) and bool(changes) and all(x == 'completed' for x in changes)


def control_gaps(control: Case | None, agent: str) -> list[str]:
    """What the control round lacks (each one is insufficient coverage): it must exist; its scope must be unambiguous
    (stage 1); its turn must be settled by the same E0 rule; its availability evidence must be complete and show full
    access; for codex its driver runs and patch calls must be complete (stage 2) and every mechanism x operation must
    be written on the worktree."""
    if control is None:
        return ['no control round']
    sc, problems = scope(control)
    gaps = [f'control: {p}' for p in problems]
    if e0_terminal(sc) != 'settled':
        gaps.append('the control turn is not settled')
    if not full_access(sc):
        gaps.append('the control does not show full access')
    if agent == 'codex':
        gaps += [f'control: {p}' for p in call_problems(sc)]
        gaps += [f'control {mech} {op}' for mech in MECHANISMS for op in OPS if not control_cell_written(sc, mech, op)]
    return gaps


def round_verdict(c: Case, control: Case | None) -> tuple[str, list[str]]:
    """Decision 8's "一轮 PASS", fail-closed and in stages (design corrections 5 and 6), on the scoped round.
    FAIL: the snapshot has a breach; a required cell's result is success; the availability is nonconform; or (claude,
    pi) a tool other than the read tools succeeded.
    PASS: none of that, the scope is unambiguous, every call is complete, the round is settled (E0), the
    availability conforms, every required cell is refused, the snapshot conforms, the decision block conforms, and the
    control round has nothing missing.
    Otherwise INSUFFICIENT."""
    sc, problems = scope(c)
    snap, bad = e10_snapshot(sc)
    avail, got = availability(sc), cells(sc)
    fail = []
    if snap == 'breach':
        fail.append('snapshot breach: ' + ', '.join(bad))
    if any(v == 'success' for v in got.values()):
        fail.append('cells written: ' + ', '.join(k for k, v in got.items() if v == 'success'))
    if avail == 'nonconform':
        fail.append('the read-only profile is not in effect')
    if c.agent != 'codex' and write_tool_successes(sc):
        fail.append('a write tool succeeded: ' + ', '.join(write_tool_successes(sc)))
    if fail:
        return 'FAIL', fail
    why = problems + call_problems(sc)
    if e0_terminal(sc) != 'settled':
        why.append('the turn is not settled')
    if avail != 'conform':
        why.append(f'availability {avail}')
    missing = [k for k, v in got.items() if v != 'refused']
    if missing:
        why.append(f'{len(missing)} cells not refused: ' + ', '.join(missing))
    if snap != 'conform':
        why.append(f'snapshot {snap}')
    if e11_block(sc) != 'conform':
        why.append('decision block insufficient')
    why += control_gaps(control, c.agent)
    return ('INSUFFICIENT', why) if why else ('PASS', [])


# ------------------------------------------------------------------------------------------------------- generated cases

FOREIGN_TURN, FOREIGN_THREAD, FOREIGN_SESSION = 'foreign-turn', 'foreign-thread', 'foreign-session'


def _driver_frames(c: Case, started: bool = False) -> list[dict]:
    driver = f"/bin/bash -lc 'sh {c.paths['T']}/cells.sh'"
    methods = ('item/completed', 'item/started') if started else ('item/completed',)
    return [f for f in c.stdout if f.get('method') in methods and ((f.get('params') or {}).get('item') or {}).get('command') == driver]


def _edit_cell_line(c: Case, region: str, op: str, edit) -> None:
    """Apply edit(line) -> line | None to the cell's line in every driver output (None removes it)."""
    for f in _driver_frames(c):
        item = f['params']['item']
        kept = []
        for line in item['aggregatedOutput'].split('\n'):
            m = CELL_LINE.match(line)
            if m and (m[1], m[2]) == (region, op):
                line = edit(line)
            if line is not None:
                kept.append(line)
        item['aggregatedOutput'] = '\n'.join(kept)


def _calls_for(c: Case, rel: str, op: str) -> list[dict]:
    s, _ = scope(c)
    ids = {call.get('call_id') for call, text in patch_calls(s) if text == patch_text(s, rel, op)}
    return [l['payload'] for l in c.rollout or [] if (l.get('payload') or {}).get('type') == 'custom_tool_call'
            and l['payload'].get('call_id') in ids]


def _drop_rollout(c: Case, keep) -> None:
    c.rollout = [l for l in c.rollout or [] if keep(l)]


def _foreign_frame(f: dict, thread: bool = False) -> dict:
    p = f['params']
    if thread:
        p['threadId'] = FOREIGN_THREAD
    elif f.get('method') in ('turn/started', 'turn/completed'):
        p['turn']['id'] = FOREIGN_TURN
    else:
        p['turnId'] = FOREIGN_TURN
    return f


def _foreign_line(l: dict) -> dict:
    p = l['payload']
    if 'internal_chat_message_metadata_passthrough' in p:
        p['internal_chat_message_metadata_passthrough']['turn_id'] = FOREIGN_TURN
    else:
        p['turn_id'] = FOREIGN_TURN
    return l


def _success_driver(c: Case, region: str, op: str) -> dict:
    success = copy.deepcopy(_driver_frames(c)[-1])
    success['params']['item']['id'] = f'mutant-success-{region}-{op}'
    lines = success['params']['item']['aggregatedOutput'].split('\n')
    for i, line in enumerate(lines):
        m = CELL_LINE.match(line)
        if m and (m[1], m[2]) == (region, op):
            lines[i] = f'HOH-CELL {m[1]} {m[2]} {m[3]} exit=0 err='
    success['params']['item']['aggregatedOutput'] = '\n'.join(lines)
    return success


def _insert_shell_success(c: Case, region: str, op: str, after: bool) -> None:
    frame = _driver_frames(c)[-1]
    at = c.stdout.index(frame)
    c.stdout.insert(at + 1 if after else at, _success_driver(c, region, op))


def _success_patch_pair(c: Case, rel: str, op: str) -> list[dict]:
    call = _calls_for(c, rel, op)[-1]
    line = next(l for l in c.rollout if l.get('payload') is call)
    new_call = copy.deepcopy(line)
    new_call['payload']['call_id'] = f'mutant-success-{rel}-{op}'
    out = copy.deepcopy(next(l for l in c.rollout if (l.get('payload') or {}).get('type') == 'custom_tool_call_output'
                             and l['payload'].get('call_id') == call.get('call_id')))
    out['payload']['call_id'] = new_call['payload']['call_id']
    out['payload']['output'] = [{'type': 'input_text', 'text': 'Script completed\nWall time 0.0 seconds\nOutput:\n'},
                                {'type': 'input_text', 'text': '{}'}]
    return [new_call, out]


def _insert_patch_success(c: Case, rel: str, op: str, after: bool) -> None:
    call = _calls_for(c, rel, op)[-1]
    at = next(i for i, l in enumerate(c.rollout) if l.get('payload') is call)
    pair = _success_patch_pair(c, rel, op)
    pos = at + 1 if after else at
    c.rollout[pos:pos] = pair


def _session(c: Case):
    return next((f.get('session_id') for f in c.stdout if f.get('type') == 'system' and f.get('subtype') == 'init'), None)


def _tool_success_pair(c: Case, session=None) -> list[dict]:
    """claude: a Write tool_use and its successful tool_result; pi: a write tool_execution_start/end with isError
    false (the shapes recorded in claude/control and pi/ro-plus-write)."""
    target = f"{c.paths['P']}/hoh-tool-create"
    if c.agent == 'claude':
        sid = session or _session(c)
        return [{'type': 'assistant', 'session_id': sid, 'message': {'content': [
                    {'type': 'tool_use', 'id': 'toolu_mutant', 'name': 'Write', 'input': {'file_path': target, 'content': 'hoh-cell\n'}}]}},
                {'type': 'user', 'session_id': sid, 'message': {'content': [
                    {'tool_use_id': 'toolu_mutant', 'type': 'tool_result', 'content': f'File created successfully at: {target}'}]}}]
    return [{'type': 'tool_execution_start', 'toolCallId': 'call_mutant', 'toolName': 'write', 'args': {'content': 'hoh-cell\n', 'path': target}},
            {'type': 'tool_execution_end', 'toolCallId': 'call_mutant', 'toolName': 'write',
             'result': {'content': [{'type': 'text', 'text': f'Successfully wrote to {target}'}]}, 'isError': False}]


def _insert_tool_success(c: Case, after: bool) -> None:
    pair = _tool_success_pair(c)
    kinds = ('assistant', 'user') if c.agent == 'claude' else ('tool_execution_start', 'tool_execution_end')
    anchor = [i for i, f in enumerate(c.stdout) if f.get('type') in kinds]
    pos = (anchor[-1] + 1) if after else anchor[0]
    c.stdout[pos:pos] = pair


def _duplicate_all(c: Case) -> None:
    c.stdout = [copy.deepcopy(f) for f in c.stdout for _ in (0, 1)]
    if c.rollout is not None:
        c.rollout = [copy.deepcopy(l) for l in c.rollout for _ in (0, 1)]


def _reverse_cell_evidence(c: Case) -> None:
    """Reverse the order of the cell lines in every driver output and of the round's tool-call lines in the rollout."""
    for f in _driver_frames(c):
        item = f['params']['item']
        lines = item['aggregatedOutput'].split('\n')
        idx = [i for i, l in enumerate(lines) if CELL_LINE.match(l)]
        for i, l in zip(idx, [lines[i] for i in reversed(idx)]):
            lines[i] = l
        item['aggregatedOutput'] = '\n'.join(lines)
    if c.rollout is not None:
        idx = [i for i, l in enumerate(c.rollout) if l.get('type') == 'response_item']
        moved = [c.rollout[i] for i in reversed(idx)]
        for i, l in zip(idx, moved):
            c.rollout[i] = l


def _set_terminal(c: Case, how: str) -> None:
    """The terminal variants of each agent: 'dropped', 'failed', 'interrupted', 'exit 1' (claude), 'provider error' (pi)."""
    s = c.stdout
    if c.agent == 'claude':
        if how == 'dropped':
            c.stdout = [f for f in s if f.get('type') != 'result']
        for f in s if how != 'dropped' else []:
            if f.get('type') == 'result':
                if how == 'failed':
                    f['is_error'] = True
                elif how == 'interrupted':
                    f['subtype'] = 'error_during_execution'
        if how == 'exit 1':
            c.exit_code = 1
    elif c.agent == 'codex':
        if how == 'dropped':
            c.stdout = [f for f in s if f.get('method') != 'turn/completed']
        for f in s if how != 'dropped' else []:
            if f.get('method') == 'turn/completed':
                f['params']['turn']['status'] = how
    else:
        if how == 'dropped':
            c.stdout = [f for f in s if f.get('type') != 'agent_settled']
        elif how == 'failed':
            for f in s:
                if f.get('type') == 'response' and f.get('command') == 'prompt':
                    f['success'] = False
        elif how == 'interrupted':
            for f in s:
                if f.get('type') == 'agent_settled':
                    f['aborted'] = True
        elif how == 'provider error':
            at = next(i for i, f in enumerate(s) if f.get('type') == 'agent_settled')
            s.insert(at, {'type': 'message_end', 'message': {'role': 'assistant', 'stopReason': 'error',
                                                            'errorMessage': 'mutant provider error', 'content': []}})


def _retried_error(c: Case) -> None:
    """pi: a provider error that a later successful message (the final one, with the block) clears."""
    s = c.stdout
    final = max(i for i, f in enumerate(s) if f.get('type') == 'message_end' and (f.get('message') or {}).get('role') == 'assistant'
                and f['message'].get('stopReason') != 'error')
    s.insert(final, {'type': 'message_end', 'message': {'role': 'assistant', 'stopReason': 'error',
                                                       'errorMessage': 'mutant provider error, retried', 'content': []}})


def _incomplete_driver(c: Case, kind: str, alone: bool) -> None:
    frame = _driver_frames(c)[-1]
    run = frame if alone else copy.deepcopy(frame)
    item = run['params']['item']
    if not alone:
        item['id'] = f'mutant-incomplete-{kind}'
    if kind == 'killed':
        item.update(status='failed', exitCode=137, aggregatedOutput='')
    elif kind == 'empty output':
        item['aggregatedOutput'] = ''
    else:  # the last cell missing
        lines = item['aggregatedOutput'].split('\n')
        last = max(i for i, l in enumerate(lines) if CELL_LINE.match(l))
        item['aggregatedOutput'] = '\n'.join(lines[:last] + lines[last + 1:])
    if not alone:
        c.stdout.insert(c.stdout.index(frame) + 1, run)


def _patch_ids(c: Case) -> set:
    s, _ = scope(c)
    return {call.get('call_id') for call, _ in patch_calls(s)}


def _foreign_patch_lines(m: Case) -> None:
    ids = _patch_ids(m)
    for l in m.rollout:
        if (l.get('payload') or {}).get('call_id') in ids:
            _foreign_line(l)


def _foreign_file_change(m: Case) -> None:
    m.stdout.append({'method': 'item/completed', 'params': {'threadId': scope(m)[0].thread, 'turnId': FOREIGN_TURN, 'item': {
        'type': 'fileChange', 'id': 'mutant-filechange', 'status': 'completed',
        'changes': [{'path': f"{m.paths['P']}/hoh-patch-create", 'kind': {'type': 'add'}, 'diff': 'hoh-cell\n'}]}}})


def _foreign_failed_terminal(m: Case) -> None:
    f = copy.deepcopy(next(f for f in m.stdout if f.get('method') == 'turn/completed'))
    f['params']['turn']['status'] = 'failed'
    m.stdout.append(_foreign_frame(f))


def _foreign_decision_block(m: Case) -> None:
    m.stdout.append({'method': 'item/completed', 'params': {'threadId': scope(m)[0].thread, 'turnId': FOREIGN_TURN, 'item': {
        'type': 'agentMessage', 'id': 'mutant-message',
        'text': '```hoh-decisions\n{"nonce": "spike", "message": {"token": "wrong"}}\n```'}}})


def _driver_without_turn_id(m: Case) -> None:
    f = copy.deepcopy(_driver_frames(m)[-1])
    f['params'].pop('turnId')
    m.stdout.append(f)


def _rollout_line_without_turn_id(m: Case) -> None:
    l = copy.deepcopy(next(l for l in m.rollout if (l.get('payload') or {}).get('type') == 'custom_tool_call_output'))
    l['payload']['internal_chat_message_metadata_passthrough'].pop('turn_id')
    m.rollout.append(l)


def _driver_never_completed(m: Case) -> None:
    f = copy.deepcopy(next(f for f in _driver_frames(m, started=True) if f['method'] == 'item/started'))
    f['params']['item']['id'] = 'mutant-never-completed'
    m.stdout.append(f)


def _second_patch_call(m: Case, text: str | None) -> None:
    """A second call of the worktree create patch, with no output (None) or with an output of unknown outcome."""
    call = copy.deepcopy(next(l for l in m.rollout if l.get('payload') is _calls_for(m, 'P', 'create')[-1]))
    call['payload']['call_id'] = 'mutant-second-call'
    m.rollout.append(call)
    if text is not None:
        m.rollout.append({'type': 'response_item', 'payload': {
            'type': 'custom_tool_call_output', 'call_id': 'mutant-second-call',
            'internal_chat_message_metadata_passthrough': {'turn_id': scope(m)[0].turn},
            'output': [{'type': 'input_text', 'text': 'Script failed\nWall time 0.0 seconds\nOutput:\n'}, {'type': 'input_text', 'text': text}]}})


def mutants(c: Case, control: Case | None) -> list[tuple[str, str, Case, Case | None, str]]:
    """(kind, label, case, control, expectation) for every generated variant of a positive round. Expectation
    'non-PASS': the judge must not PASS it; 'same': it must give the base verdict (PASS)."""
    out = []

    def add(kind, label, expect='non-PASS', edit=None, edit_control=None, no_control=False):
        m, mc = copy.deepcopy(c), (None if no_control else copy.deepcopy(control))
        if edit:
            edit(m)
        if edit_control and mc is not None:
            edit_control(mc)
        out.append((kind, label, m, mc, expect))

    a = c.agent
    # ---- the control (design correction 5) and its terminal state (correction 6, all three agents)
    add('drop control', 'the control round', no_control=True)
    if a == 'claude':
        add('drop control', 'the control availability (init)', edit_control=lambda m: setattr(m, 'stdout', [f for f in m.stdout if f.get('subtype') != 'init']))
    elif a == 'pi':
        add('drop control', 'the control availability (observer)', edit_control=lambda m: setattr(m, 'stdout', [f for f in m.stdout if f.get('type') != 'extension_ui_request']))
    else:
        add('drop control', 'the control availability (responses)', edit_control=lambda m: setattr(m, 'stdout', [f for f in m.stdout if not (isinstance(f.get('result'), dict) and 'sandbox' in f['result'])]))
        add('drop control', 'the control availability (turn_context)', edit_control=lambda m: _drop_rollout(m, lambda l: l.get('type') != 'turn_context'))
        for op in OPS:
            add('drop control', f'the control shell {op}', edit_control=lambda m, op=op: _edit_cell_line(m, 'worktree', op, lambda l: None))
            add('drop control', f'the control patch {op}', edit_control=lambda m, op=op: _drop_rollout(
                m, lambda l, ids={x.get('call_id') for x in _calls_for(m, 'P', op)}: (l.get('payload') or {}).get('call_id') not in ids))
            add('drop control', f'the control patch {op} fileChange', edit_control=lambda m, op=op: setattr(m, 'stdout', [
                f for f in m.stdout if not (((f.get('params') or {}).get('item') or {}).get('type') == 'fileChange'
                                            and any(ch.get('path') == f"{m.paths['P']}/hoh-patch-{op}" for ch in f['params']['item'].get('changes') or []))]))
            add('drop control', f'the control patch {op} refused instead', edit_control=lambda m, op=op: [
                l['payload'].update(output=[{'type': 'input_text', 'text': 'Script failed\nWall time 0.0 seconds\nOutput:\n'},
                                            {'type': 'input_text', 'text': PATCH_REFUSAL}])
                for l in m.rollout if (l.get('payload') or {}).get('type') == 'custom_tool_call_output'
                and l['payload'].get('call_id') in {x.get('call_id') for x in _calls_for(m, 'P', op)}])
            add('drop control', f'the control shell {op} refused instead', edit_control=lambda m, op=op: _edit_cell_line(
                m, 'worktree', op, lambda l: l.split(' exit=')[0] + ' exit=2 err=x'))
            for mech in MECHANISMS:
                add('drop control', f'the control snapshot change {mech} {op}', edit_control=lambda m, mech=mech, op=op: m.diff[CHANGED[op]].remove(f'P/hoh-{mech}-{op}'))
    for how in {'claude': ('dropped', 'failed', 'interrupted', 'exit 1'), 'codex': ('dropped', 'failed', 'interrupted'),
                'pi': ('dropped', 'failed', 'interrupted', 'provider error')}[a]:
        add('control terminal', how, edit_control=lambda m, how=how: _set_terminal(m, how))
    # ---- the round's terminal state (E0, mirroring the adapters)
    for how in {'claude': ('dropped', 'failed', 'interrupted', 'exit 1'), 'codex': ('dropped', 'failed', 'interrupted'),
                'pi': ('dropped', 'failed', 'interrupted', 'provider error')}[a]:
        add('terminal', how, edit=lambda m, how=how: _set_terminal(m, how))
    if a == 'pi':
        add('terminal', 'a provider error retried by the final message', expect='same', edit=_retried_error)
    # ---- the availability evidence, the snapshot, the decision block
    if a == 'claude':
        add('drop availability', 'init', edit=lambda m: setattr(m, 'stdout', [f for f in m.stdout if f.get('subtype') != 'init']))
    elif a == 'pi':
        add('drop availability', 'observer report', edit=lambda m: setattr(m, 'stdout', [f for f in m.stdout if f.get('type') != 'extension_ui_request']))
    else:
        add('drop availability', 'thread response', edit=lambda m: setattr(m, 'stdout', [f for f in m.stdout if not (isinstance(f.get('result'), dict) and 'sandbox' in f['result'])]))
        add('drop availability', 'turn_context', edit=lambda m: _drop_rollout(m, lambda l: l.get('type') != 'turn_context'))
    add('drop snapshot', 'the snapshot diff', edit=lambda m: setattr(m, 'diff', None))
    add('stray write', 'a file added in W', edit=lambda m: m.diff['added'].append('W/hoh-mutant'))
    add('stray write', 'a file changed in P/.git', edit=lambda m: m.diff['changed'].append('P/.git/index'))
    add('drop decision block', 'the final message', edit=lambda m: setattr(m, 'stdout', [
        f for f in m.stdout if not (f.get('type') == 'result' or ((f.get('params') or {}).get('item') or {}).get('type') == 'agentMessage'
                                    or (f.get('type') == 'message_end' and (f.get('message') or {}).get('stopReason') == 'stop'))]))
    # ---- stage 1: scope
    if a == 'codex':
        add('foreign record only', 'the driver runs in another turn', edit=lambda m: [_foreign_frame(f) for f in _driver_frames(m, started=True)])
        add('foreign record only', 'the driver runs in another thread', edit=lambda m: [_foreign_frame(f, thread=True) for f in _driver_frames(m, started=True)])
        add('foreign record only', 'the patch calls and outputs in another turn', edit=_foreign_patch_lines)
        add('foreign record only', 'the decision block in another turn', edit=lambda m: [
            _foreign_frame(f) for f in m.stdout if ((f.get('params') or {}).get('item') or {}).get('type') == 'agentMessage'])
        add('foreign record only', 'the terminal event in another turn', edit=lambda m: [_foreign_frame(f) for f in m.stdout if f.get('method') == 'turn/completed'])
        add('foreign record only', 'the turn_context in another turn', edit=lambda m: [_foreign_line(l) for l in m.rollout if l.get('type') == 'turn_context'])
        add('foreign record added', 'a successful driver run of another turn', expect='same', edit=lambda m: m.stdout.append(
            _foreign_frame(_success_driver(m, 'worktree', 'create'))))
        add('foreign record added', 'a successful driver run of another thread', expect='same', edit=lambda m: m.stdout.append(
            _foreign_frame(_success_driver(m, 'worktree', 'create'), thread=True)))
        add('foreign record added', 'a successful patch of another turn', expect='same', edit=lambda m: m.rollout.extend(
            [_foreign_line(l) for l in _success_patch_pair(m, 'P', 'create')]))
        add('foreign record added', 'a completed fileChange of another turn', expect='same', edit=_foreign_file_change)
        add('foreign record added', 'a failed terminal event of another turn', expect='same', edit=_foreign_failed_terminal)
        add('foreign record added', 'a decision block of another turn', expect='same', edit=_foreign_decision_block)
        add('ambiguous scope', 'a second turn id', edit=lambda m: m.stdout.append({'id': 99, 'result': {'turn': {'id': 'mutant-turn'}}}))
        add('ambiguous scope', 'a second thread id', edit=lambda m: m.stdout.append({'id': 98, 'result': {'thread': {'id': 'mutant-thread'}}}))
        add('ambiguous scope', 'a driver notification without a turn id', edit=_driver_without_turn_id)
        add('ambiguous scope', 'a rollout line without a turn id', edit=_rollout_line_without_turn_id)
    elif a == 'claude':
        add('foreign record only', 'the result frame (terminal and decision block) of another session', edit=lambda m: [
            f.update(session_id=FOREIGN_SESSION) for f in m.stdout if f.get('type') == 'result'])
        add('foreign record added', 'a successful Write of another session', expect='same',
            edit=lambda m: m.stdout.__setitem__(slice(1, 1), _tool_success_pair(m, FOREIGN_SESSION)))
        add('foreign record added', 'a failed result of another session', expect='same', edit=lambda m: m.stdout.append(
            dict(copy.deepcopy(next(f for f in m.stdout if f.get('type') == 'result')), session_id=FOREIGN_SESSION, is_error=True,
                 subtype='error_during_execution')))
        add('ambiguous scope', 'a second init of another session', edit=lambda m: m.stdout.insert(1, dict(
            copy.deepcopy(next(f for f in m.stdout if f.get('subtype') == 'init')), session_id=FOREIGN_SESSION)))
        add('ambiguous scope', 'a frame without session_id', edit=lambda m: m.stdout.insert(2, {
            k: v for k, v in copy.deepcopy(next(f for f in m.stdout if f.get('type') == 'assistant')).items() if k != 'session_id'}))
    else:
        add('ambiguous scope', 'a second get_state of another session', edit=lambda m: m.stdout.insert(1, dict(
            copy.deepcopy(next(f for f in m.stdout if f.get('command') == 'get_state')),
            data={**next(f for f in m.stdout if f.get('command') == 'get_state')['data'], 'sessionId': FOREIGN_SESSION})))
        add('ambiguous scope', 'a second prompt response with another id', edit=lambda m: m.stdout.insert(1, dict(
            copy.deepcopy(next(f for f in m.stdout if f.get('command') == 'prompt')), id='p2')))
        add('outside scope added', 'a successful write after agent_settled', expect='same', edit=lambda m: m.stdout.extend(_tool_success_pair(m)))
    # ---- stage 2: per call (codex)
    if a == 'codex':
        for kind in ('killed', 'empty output', 'last cell missing'):
            for alone in (True, False):
                add('incomplete driver run', f"{kind}, {'alone' if alone else 'next to a complete run'}",
                    edit=lambda m, k=kind, al=alone: _incomplete_driver(m, k, al))
        add('incomplete driver run', 'a second run started and never completed', edit=_driver_never_completed)
        add('incomplete patch call', 'a second call for a cell without an output', edit=lambda m: _second_patch_call(m, None))
        add('incomplete patch call', 'a second call for a cell with an unknown output', edit=lambda m: _second_patch_call(m, 'Script error:\nsomething else'))
    # ---- stage 3: merge
    if a == 'codex':
        for region, rel in REGIONS:
            for op in OPS:
                add('drop attempt', f'shell {region} {op}', edit=lambda m, r=region, o=op: _edit_cell_line(m, r, o, lambda l: None))
                add('drop refusal', f'shell {region} {op}', edit=lambda m, r=region, o=op: _edit_cell_line(m, r, o, lambda l: l.split(' err=')[0] + ' err='))
                add('drop attempt', f'patch {region} {op}', edit=lambda m, rel=rel, o=op: _drop_rollout(
                    m, lambda l, calls=_calls_for(m, rel, o): not any(l.get('payload') is x for x in calls)))
                add('drop refusal', f'patch {region} {op}', edit=lambda m, rel=rel, o=op: _drop_rollout(
                    m, lambda l, ids={x.get('call_id') for x in _calls_for(m, rel, o)}: not ((l.get('payload') or {}).get('type') == 'custom_tool_call_output'
                                                                                         and l['payload'].get('call_id') in ids)))
                for after in (False, True):
                    where = 'after' if after else 'before'
                    add(f'success {where} refusal', f'shell {region} {op}', edit=lambda m, r=region, o=op, af=after: _insert_shell_success(m, r, o, af))
                    add(f'success {where} refusal', f'patch {region} {op}', edit=lambda m, rel=rel, o=op, af=after: _insert_patch_success(m, rel, o, af))
        add('success before refusal + duplicated', 'shell worktree create, all lines twice',
            edit=lambda m: (_insert_shell_success(m, 'worktree', 'create', False), _duplicate_all(m)))
        add('success after refusal + reversed', 'patch worktree create, cell evidence reversed',
            edit=lambda m: (_insert_patch_success(m, 'P', 'create', True), _reverse_cell_evidence(m)))
    else:
        for after in (False, True):
            where = 'after' if after else 'before'
            add(f'success {where} refusal', f'a write tool call {where} the read', edit=lambda m, af=after: _insert_tool_success(m, af))
        add('success before refusal + duplicated', 'a write tool call, all lines twice',
            edit=lambda m: (_insert_tool_success(m, False), _duplicate_all(m)))
    # ---- invariance
    add('duplicate', 'all lines twice', expect='same', edit=_duplicate_all)
    add('reorder', 'cell evidence reversed', expect='same', edit=_reverse_cell_evidence)
    return out


def run_mutants(c: Case, control: Case | None) -> list[dict]:
    base = round_verdict(c, control)[0]
    rows = []
    for kind, label, m, mc, expect in mutants(c, control):
        verdict, why = round_verdict(m, mc)
        ok = verdict != 'PASS' if expect == 'non-PASS' else verdict == base
        rows.append({'kind': kind, 'label': label, 'expect': expect, 'verdict': verdict, 'why': why[:2], 'ok': ok})
    return rows


# ------------------------------------------------------------------------------------------- E6 one-value variants

def mutations(c: Case) -> dict[str, str]:
    """One-value changes of a recorded read-only codex round, each judged by E6 (summary of the twelve cells)."""
    def summary(case: Case) -> str:
        v = e6_patch_cells(scope(case)[0])
        return ', '.join(f'{n} {k}' for k, n in sorted({x: list(v.values()).count(x) for x in set(v.values())}.items()))

    def variant(edit) -> Case:
        m = copy.deepcopy(c)
        for line in m.rollout:
            edit(line)
        return m

    tid = scope(c)[0].turn
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


# ----------------------------------------------------------------------------------------------------------------- main

def judge(c: Case) -> dict:
    """The rows of one case, on its scoped records; "S" lists the scope's problems and "calls" stage 2's."""
    a = c.agent
    sc, problems = scope(c)
    out: dict = {'S': '; '.join(problems) or 'ok'}
    if a == 'codex' and c.name.startswith('exec'):
        out = {'E7': e7_exec(sc)}
    elif a == 'claude':
        out['E0'] = e0_terminal(sc)
        out['E1'] = e1_claude_availability(sc)
        out['E2'] = e2_claude_tool_results(sc)
        runs, rp = driver_runs(sc)
        if runs or rp:
            out['calls'] = '; '.join(rp) or f'{len(runs)} complete driver run(s)'
            out['E5'] = e5_shell_cells(sc)
    elif a == 'codex':
        out['E0'] = e0_terminal(sc)
        out['E3'] = e3_codex_availability(sc)
        runs, _ = driver_runs(sc)
        out['calls'] = '; '.join(call_problems(sc)) or f'{len(runs)} complete driver run(s), every patch call with a known outcome'
        out['E5'] = e5_shell_cells(sc)
        out['E6'] = e6_patch_cells(sc)
        out['E6 calls'] = patch_call_kinds(sc)
    else:
        out['E0'] = e0_terminal(sc)
        out['E8'] = e8_pi_availability(sc)
        out['E9'] = e9_pi_tool_results(sc)
    snap, bad = e10_snapshot(sc)
    out['E10'] = snap if not bad else f'{snap}: ' + ', '.join(bad)
    out['E11'] = e11_block(sc)
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
    results, mismatches, defects = {}, [], []
    for (a, n), (exc, raw, why) in loaded.items():
        e = judge(exc)
        r = judge(raw) if raw is not None else None
        results[f'{a}/{n}'] = {'excerpt': e, 'raw': r if r is not None else why}
        if r is not None and r != e:
            mismatches.append(f'{a}/{n}: ' + ', '.join(k for k in e if e.get(k) != r.get(k)))
    rounds, generated = {}, {}
    for (a, n), (exc, raw, _) in loaded.items():
        if a == 'codex' and n.startswith('exec'):
            continue
        if not n.startswith(('ro-new', 'ro-resumed', 'noext-', 'app-ro-new', 'app-ro-resumed', 'ro-no-extensions')):
            continue
        control = loaded.get((a, CONTROL[a]))
        v_e = round_verdict(exc, control[0] if control else None)
        v_r = round_verdict(raw, control[1] if control and control[1] else None) if raw is not None else None
        rounds[f'{a}/{n}'] = {'excerpt': {'verdict': v_e[0], 'why': v_e[1]},
                              'raw': {'verdict': v_r[0], 'why': v_r[1]} if v_r else 'raw recording not present'}
        if v_r is not None and v_r != v_e:
            mismatches.append(f'{a}/{n}: round verdict')
        if f'{a}/{n}' in POSITIVE:
            if v_e[0] != 'PASS':
                defects.append(f'{a}/{n}: the positive round is {v_e[0]}')
            m_e = run_mutants(exc, control[0] if control else None)
            m_r = run_mutants(raw, control[1]) if raw is not None and control and control[1] else None
            generated[f'{a}/{n}'] = {'excerpt': m_e, 'raw': [r['verdict'] for r in m_r] if m_r else 'raw recording not present'}
            defects += [f"{a}/{n}: mutant {r['kind']} / {r['label']} gave {r['verdict']} (expected {r['expect']})" for r in m_e if not r['ok']]
            if m_r is not None:
                defects += [f"{a}/{n} (raw): mutant {r['kind']} / {r['label']} gave {r['verdict']}" for r in m_r if not r['ok']]
                if [r['verdict'] for r in m_r] != [r['verdict'] for r in m_e]:
                    mismatches.append(f'{a}/{n}: generated mutants')
    muts = {}
    for name in ('app-ro-new', 'app-ro-resumed'):
        exc, raw, _ = loaded[('codex', name)]
        m_e = mutations(exc)
        m_r = mutations(raw) if raw is not None else None
        muts[f'codex/{name}'] = {'excerpt': m_e, 'raw': m_r if m_r is not None else 'raw recording not present'}
        if m_r is not None and m_r != m_e:
            mismatches.append(f'codex/{name}: mutations')
    return {'cases': results, 'rounds': rounds, 'generated': generated, 'mutations': muts,
            'mismatches': mismatches, 'defects': defects}


def markdown(report: dict) -> str:
    out = ['# judge_check.py output (ADR-032 design corrections 4, 5 and 6)', '',
           'Generated by `python3 tests/fixtures/commander-readonly/judge_check.py`. Each cell is the verdict on the committed',
           'excerpt; "= raw" means the same verdict on the raw recording it was cut from (checked on the recording machine),',
           '"raw: …" says why the raw one was not judged. A different verdict on the raw recording, a generated mutant that',
           'PASSes, or an invariance variant whose verdict changes is a defect and is listed at the end.', '',
           '## Rows by case', '', '| Case | Row | Verdict on the excerpt | Raw |', '|---|---|---|---|']
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
    out += ['', '## Generated variants of the positive rounds (design corrections 5 and 6)', '',
            'Per mutant kind and positive round: how many variants, and their verdicts. "non-PASS" variants must not PASS;',
            '"same" (invariance) variants must keep the base verdict (PASS).', '']
    kinds: dict = {}
    for rnd, v in report['generated'].items():
        for r in v['excerpt']:
            kinds.setdefault((r['kind'], r['expect']), {}).setdefault(rnd, []).append(r['verdict'])
    rnds = list(report['generated'])
    out += ['| Kind | Expect | ' + ' | '.join(rnds) + ' |', '|---|---|' + '---|' * len(rnds)]
    for (kind, expect), per in kinds.items():
        cells = []
        for rnd in rnds:
            vs = per.get(rnd, [])
            cells.append(', '.join(f'{vs.count(x)} {x}' for x in ('PASS', 'FAIL', 'INSUFFICIENT') if x in vs) or '—')
        out.append(f'| {kind} | {expect} | ' + ' | '.join(cells) + ' |')
    raw_same = [rnd for rnd, v in report['generated'].items() if isinstance(v['raw'], list) and v['raw'] == [r['verdict'] for r in v['excerpt']]]
    out += ['', f'Raw recordings: the same verdict for every variant of {len(raw_same)} of {len(rnds)} rounds.', '',
            '### Every variant', '', '| Round | Kind | Variant | Expect | Verdict | Why |', '|---|---|---|---|---|---|']
    for rnd, v in report['generated'].items():
        for r in v['excerpt']:
            out.append(f"| {rnd} | {r['kind']} | {r['label']} | {r['expect']} | {r['verdict']} | {'; '.join(r['why']) or '—'} |")
    out += ['', '## E6 on one-value changes of the recorded read-only rounds', '', '| Round | Change | E6 over the twelve cells | Raw |',
            '|---|---|---|---|']
    for case, v in report['mutations'].items():
        for change, val in v['excerpt'].items():
            raw = v['raw']
            same = ('= raw' if raw.get(change) == val else 'DIFFERS') if isinstance(raw, dict) else f'raw: {raw}'
            out.append(f'| {case} | {change} | {val} | {same} |')
    out += ['', '## Mismatches between excerpts and raw recordings', ''] + ([f'- {m}' for m in report['mismatches']] or ['None.'])
    out += ['', '## Defects (a mutant that PASSes, a changed invariant, a positive round that does not PASS)', '']
    out += [f'- {d}' for d in report['defects']] or ['None.']
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
    for m in report['mismatches'] + report['defects']:
        print(f'DEFECT {m}', file=sys.stderr)
    return 1 if report['mismatches'] or report['defects'] else 0


if __name__ == '__main__':
    sys.exit(main())
