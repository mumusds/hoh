#!/usr/bin/env python3
"""Checks "independent-qa" and "independent-review" (REQ-052, REQ-054; docs/adr/ADR-027.md decision 11).

Run by tools/run_check.py as `bash scripts/product/independent.sh <qa|reviewer> <profile>`. The role's prompt
(resources/prompts/<role>.md) is rendered with the frozen candidate and principle one's judgement of its members
(scripts/product/traceability.py), written to $HOH_EVIDENCE_DIR/judgement.json. Fresh native sessions then run through
tools/agent_task.py in order (qa: pi, then claude; reviewer: codex, codex with gpt-6.1-sol, then claude) until one
writes a report: a report written, PASS, FAIL or UNKNOWN, ends the search; a session that wrote none moves on. The
reporting session's report, attempt record and raw stream are copied to the check's declared artifacts
independent-report.json, native-attempt.json and raw-session.jsonl, checked against the digests the report names;
agent_task itself writes live-sessions.json. Every attempt is listed in $HOH_EVIDENCE_DIR/independent-attempts.json.
Exit 0 iff the report is PASS with no critical or high finding; 3 iff no attempt wrote a report and each was blocked
from outside (the CLI never started, or its record shows an authentication, quota or capacity failure); else 1.
"""
from __future__ import annotations
import argparse, json, os, shutil, subprocess, sys, time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'tools'))
sys.path.insert(0, str(ROOT / 'scripts/product'))
from common import atomic_json, load_json, safe_path, sha256  # noqa: E402
from render_prompt import render  # noqa: E402
import traceability  # noqa: E402

ORDER = {'qa': [('pi', None), ('claude', None)],
         'reviewer': [('codex', None), ('codex', 'gpt-6.1-sol'), ('claude', None)]}
CHECK = {'qa': 'independent-qa', 'reviewer': 'independent-review'}
# tools/run_check.py's grace after its deadline (CHILD_GRACE): the attempts end before it.
GRACE = 15.0
# tools/agent_task.py's exits before its CLI starts: the CLI is missing (127), its help or flags, the user, Pi's
# packages or Codex's workspace refused it (78), or the bridge refused the call (64).
NOT_STARTED = {127, 78, 64}
# What each CLI writes for an authentication, quota or capacity failure, taken from its installed build: codex-cli
# 0.166.0 (strings of its binary; the capacity one is also in this project's attempt records), Claude Code 2.1.319
# (strings of its binary), Pi 1.5.0 (@earendil-works/pi-ai: its terminal rate-limit test and its authentication
# errors). Matched without case in the CLI's error output only (docs/adr/ADR-027.md decision 11, M2T10D-155).
BLOCKS = {
    'codex': ('Selected model is at capacity', 'Policy-restricted capacity is unavailable', 'Flex capacity unavailable',
              'Quota exceeded. Check your plan', 'hit your usage limit', 'Not logged in',
              'authentication session could not be refreshed', 'access token could not be refreshed'),
    'claude': ('Not logged in', 'Please run /login', 'Invalid API key', 'Authentication required',
               'authentication_error', 'Credit balance is too low', 'usage limit reached', 'rate_limit_error',
               'overloaded_error'),
    'pi': ('No API key found for', 'Authentication failed', 'GoUsageLimitError', 'FreeUsageLimitError',
           'Monthly usage limit reached', 'available balance', 'insufficient_quota', 'out of budget', 'quota exceeded'),
}
ARTIFACTS = ('independent-report.json', 'native-attempt.json', 'raw-session.jsonl')


def judgement(candidate: dict, profile: str, check: str) -> dict:
    """Principle one's judgement of every member of the candidate: its vetoes, outcome and each TEST's status, as this
    check sees them: every earlier run of the traceability check counts as it is (M2T10R-51)."""
    world = traceability.World(ROOT, candidate, profile, check_id=check)
    found = traceability.members(world)
    report = traceability.report(world, found, [])
    report['members'] = [{'path': m.rel, 'check_id': m.check, 'damaged': m.damaged, 'reasons': m.reasons,
                          'outcome': m.outcome[0], 'outcome_reason': m.outcome[1]} for m in found]
    return report


def context(role: str, profile: str, candidate: dict, judged: dict, judgement_path: Path, observations: Path) -> dict:
    """The rendered prompt's three layers after the role contract (resources/prompts/<role>.md)."""
    name = {'qa': 'QA', 'reviewer': 'review'}[role]
    statuses: dict[str, int] = {}
    for test in judged['tests'].values():
        statuses[test['status']] = statuses.get(test['status'], 0) + 1
    return {
        'GOAL': (f'Independent {name} of the frozen candidate {candidate["candidate_id"]} (commit '
                 f'{candidate["git_commit"]}) for release profile {profile}: judge from the code at that commit and the '
                 'recorded evidence whether the profile\'s requirements are met. Your report is evidence, not the gate.'),
        'PROJECT_CONTEXT': (f'The checkout {ROOT} is the candidate\'s commit. Read AGENTS.md, CONTRACT.md, '
                            f'{candidate["spec"]["path"]} (the requirements whose release_targets contain {profile}), '
                            'contracts/acceptance-scenarios.json, docs/10-acceptance.md, docs/07-algorithms.md section 6, '
                            'and the documents each requirement\'s doc_refs name.'),
        'EVIDENCE_CONTEXT': (f'Principle one\'s judgement of every execution record of this candidate '
                             f'(docs/adr/ADR-027.md), with each member\'s vetoes and outcome and each TEST\'s status: '
                             f'{judgement_path}. {len(judged["members"])} members, {len(judged["invalid"])} invalid; '
                             f'TEST statuses {json.dumps(statuses, sort_keys=True)}; unsatisfied: '
                             f'{json.dumps(judged["unsatisfied"][:200])}; invalid: '
                             f'{json.dumps(judged["invalid"][:50])}. Records are claims to check, not proof.'),
        'TASK_CONTEXT': (f'Role {role}, check {CHECK[role]}. Do not modify, add or delete any tracked file, and create '
                         f'no file in the checkout except under {observations} (ignored by git), where your notes, '
                         'logs and experiment outputs go; a planned check you run goes through tools/run_check.py with '
                         f'its --out under {observations}; an experiment that needs a writable tree runs in a copy under '
                         '/tmp that you delete. Never touch the user\'s .hoh/ or agent credentials; start no agent '
                         'session. A fixture or mock result is never live. End with the ```hoh-report block of the '
                         'output contract: status PASS only when the scope was fully examined and no critical or high '
                         'defect was found.'),
    }


def error_texts(directory: Path) -> list[str]:
    """The CLI's own error output in an attempt directory: its stderr, and the error fields of its JSON stream."""
    texts = []
    try:
        texts.append((directory / 'stderr.log').read_text(encoding='utf-8', errors='replace')[-65536:])
    except OSError:
        pass
    try:
        lines = (directory / 'stdout.jsonl').read_bytes().splitlines()
    except OSError:
        lines = []
    for raw in lines:
        try:
            event = json.loads(raw)
        except ValueError:
            continue
        if not isinstance(event, dict):
            continue
        for part in (event, event.get('message')):
            if not isinstance(part, dict):
                continue
            if part.get('type') in ('error', 'turn.failed') and isinstance(part.get('message'), str):
                texts.append(part['message'])
            if part.get('is_error') is True and isinstance(part.get('result'), str):
                texts.append(part['result'])
            for key in ('error', 'errorMessage'):
                value = part.get(key)
                if isinstance(value, str):
                    texts.append(value)
                elif isinstance(value, dict) and isinstance(value.get('message'), str):
                    texts.append(value['message'])
    return texts


def blocked(agent: str, directory: Path | None) -> str | None:
    """The recorded pattern an attempt's error output shows, if any."""
    if directory is None:
        return None
    texts = [t.lower() for t in error_texts(directory)]
    return next((p for p in BLOCKS[agent] if any(p.lower() in t for t in texts)), None)


def attempt(role: str, agent: str, model: str | None, prompt: Path, report: Path, timeout: float) -> dict:
    """One fresh native session through tools/agent_task.py: its exit, attempt directory and whether it reported."""
    argv = [sys.executable, str(ROOT / 'tools/agent_task.py'), '--agent', agent, '--role', role, '--prompt-file',
            str(prompt), '--cwd', str(ROOT), '--timeout', f'{timeout:.0f}', '--report-out', str(report)]
    if model:
        argv += ['--model', model]
    done = subprocess.run(argv, cwd=ROOT, stdout=subprocess.PIPE, text=True)
    record = None
    for line in reversed(done.stdout.splitlines()):
        try:
            record = json.loads(line).get('attempt_record')
            break
        except (ValueError, AttributeError):
            continue
    directory = Path(record).parent if record else None
    reported = report.is_file()
    pattern = None if reported else blocked(agent, directory)
    return {'agent': agent, 'model': model, 'exit_code': done.returncode,
            'attempt_dir': str(directory) if directory else None, 'report': str(report) if reported else None,
            # agent_task passes a started CLI's own exit code through: only with no attempt record did it end before
            # the CLI started (M2T10R-53).
            'external': not reported and ((directory is None and done.returncode in NOT_STARTED) or pattern is not None),
            'pattern': pattern}


def deliver(out: Path, chosen: dict) -> dict:
    """Copy the reporting attempt's report, record and raw stream to the declared artifacts, each the bytes the report
    names; refused (ValueError) otherwise."""
    report = load_json(Path(chosen['report']))
    directory = Path(chosen['attempt_dir']).resolve()
    for key, source in (('native_attempt', directory / 'attempt.json'), ('raw_session', directory / 'stdout.jsonl')):
        ref = report[key]
        if safe_path(ROOT, ref['path']) != source or sha256(source) != ref['sha256']:
            raise ValueError(f'the report\'s {key} is not its attempt\'s {source.name}')
    shutil.copyfile(directory / 'attempt.json', out / 'native-attempt.json')
    shutil.copyfile(directory / 'stdout.jsonl', out / 'raw-session.jsonl')
    shutil.copyfile(chosen['report'], out / 'independent-report.json')
    if (sha256(out / 'native-attempt.json'), sha256(out / 'raw-session.jsonl')) != (
            report['native_attempt']['sha256'], report['raw_session']['sha256']):
        raise ValueError('a copied artifact differs from the bytes the report names')
    return report


def main(argv=None) -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument('role', choices=sorted(ORDER))
    p.add_argument('profile')
    a = p.parse_args(argv)
    started = time.monotonic()
    out, candidate_file = os.environ.get('HOH_EVIDENCE_DIR'), os.environ.get('HOH_CANDIDATE_FILE')
    check = os.environ.get('HOH_CHECK_ID', CHECK[a.role])
    if not out or not candidate_file:
        print('independent: run it through tools/run_check.py (HOH_EVIDENCE_DIR, HOH_CANDIDATE_FILE)', file=sys.stderr)
        return 64
    out_dir = Path(out)
    try:
        candidate = load_json(Path(candidate_file))
        plan = load_json(safe_path(ROOT, candidate['test_plan']['path']))['checks'][check]
        judged = judgement(candidate, a.profile, check)
    except (ValueError, KeyError, TypeError, OSError) as exc:
        print(f'independent: {exc}', file=sys.stderr)
        return 1
    budget = float(plan.get('timeout_seconds', 3600)) - GRACE
    # Beside the record's directory, never in it: a check a session runs there through run_check is a record of its own,
    # whose directory must not lie in this one's (rule 7, M2T10R-52).
    record_dir = out_dir.parent
    observations = record_dir.parent / f'{record_dir.name}-observations'
    observations.mkdir(exist_ok=True)
    judgement_path = out_dir / 'judgement.json'
    atomic_json(judgement_path, judged)
    template = (ROOT / f'resources/prompts/{a.role}.md').read_text(encoding='utf-8')
    prompt = out_dir / 'prompt.md'
    prompt.write_text(render(template, context(a.role, a.profile, candidate, judged, judgement_path, observations)),
                      encoding='utf-8')
    (out_dir / 'reports').mkdir(exist_ok=True)
    attempts, chosen = [], None
    for n, (agent, model) in enumerate(ORDER[a.role], 1):
        remaining = budget - (time.monotonic() - started)
        if remaining < 1:
            print('independent: the check\'s time ran out before every agent was tried', file=sys.stderr)
            break
        report = out_dir / 'reports' / f'{n}-{agent}{"-" + model if model else ""}.json'
        tried = attempt(a.role, agent, model, prompt, report, remaining)
        attempts.append(tried)
        print(f'independent: {agent}{" " + model if model else ""}: exit {tried["exit_code"]}, '
              f'{"report written" if tried["report"] else "external block: " + str(tried["pattern"] or "not started") if tried["external"] else "no report"}',
              file=sys.stderr)
        if tried['report']:
            chosen = tried
            break
    atomic_json(out_dir / 'independent-attempts.json', {'role': a.role, 'check_id': check, 'attempts': attempts})
    if chosen is None:
        external = bool(attempts) and all(t['external'] for t in attempts) and len(attempts) == len(ORDER[a.role])
        print(f'independent: no report; {"BLOCKED" if external else "FAIL"}', file=sys.stderr)
        return 3 if external else 1
    try:
        report = deliver(out_dir, chosen)
    except (ValueError, KeyError, OSError) as exc:
        print(f'independent: {exc}', file=sys.stderr)
        return 1
    blocking = [f['id'] for f in report['findings'] if f['severity'] in ('critical', 'high')]
    print(f'independent: report {report["status"]}, session {report["session_id"]}, '
          f'{len(report["findings"])} findings{", blocking: " + ", ".join(blocking) if blocking else ""}', file=sys.stderr)
    return 0 if report['status'] == 'PASS' and not blocking else 1


if __name__ == '__main__':
    sys.exit(main())
