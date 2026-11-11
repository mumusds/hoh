#!/usr/bin/env python3
"""Check "traceability" (REQ-052; docs/adr/ADR-027.md principle one and decision 10).

Run by tools/run_check.py as `bash scripts/product/traceability.sh <profile>`, in this order:
1. the self-tests: a TEST of the profile whose scenario check is this one is satisfied only by this run's own lines.
   The tests under tests/bundle whose names hold `test_NNN_MM` for such a TEST run, and their results are written to
   $HOH_EVIDENCE_DIR/test-results.jsonl (none found: no line, and the TEST is NOT_RUN);
2. the members: every execution.json under .hoh-dev/ and .hoh/evidence/ that claims the candidate (or that cannot be
   read and holds its id) is judged by principle one: the rule table (veto), the outcome function, and the conclusion
   function per member and TEST. Each TEST's status follows from them over all members;
3. the report $HOH_EVIDENCE_DIR/traceability-report.json. Exit 0 iff every TEST of the profile is satisfied and no
   member is invalid, else 1.
Like tools/release_gate.py it checks that recorded files are consistent and belong to the candidate, not that they
were not forged. The same functions are the product's (hoh_core::verification) less the rules that need its store.
"""
from __future__ import annotations
import argparse, hashlib, json, os, re, sys, unittest
from datetime import datetime
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'tools'))
from jsonschema import Draft202012Validator, FormatChecker  # noqa: E402
from common import atomic_json, load_json, safe_path, sha256  # noqa: E402
from candidate import identity  # noqa: E402

TOOLS = {'tools/run_check.py', 'hoh'}
SEARCH = ('.hoh-dev', '.hoh/evidence')
EMPTY = hashlib.sha256(b'').hexdigest()
ARTIFACTS = {'live_sessions': 'live-sessions.json', 'soak_timeline': 'timeline.jsonl',
             'test_results': 'test-results.jsonl'}
TEST_ID = re.compile(r'(?:^|[^0-9A-Za-z])test_(\d{3})_(\d{2})(?![0-9])')
SATISFIED = ('PASS', 'DEGRADED')
SCHEMAS = ('execution', 'test-result', 'live-session', 'soak-event', 'candidate')


class Test:
    """A TEST of the profile: its scenario check and whether its requirement may degrade."""
    def __init__(self, check: str | None, degradable: bool):
        self.check, self.degradable = check, degradable


class World:
    """What members are judged against: the project root, the candidate, its frozen plan, its release policy (when
    the candidate tracks contracts/release-policy.json) and the profile."""
    def __init__(self, root: Path, candidate: dict, profile: str, check_id: str = 'traceability',
                 requirements: list | None = None, scenarios: dict[str, str] | None = None):
        self.root = root.resolve()
        if candidate.get('candidate_id') != identity(candidate):
            raise ValueError('candidate identity mismatch')
        self.candidate, self.cid, self.profile, self.check_id = candidate, candidate['candidate_id'], profile, check_id
        plan = safe_path(self.root, candidate['test_plan']['path'])
        if sha256(plan) != candidate['test_plan_sha256']:
            raise ValueError('the check plan is not the candidate\'s')
        self.plan = load_json(plan)['checks']
        tracked = {f['path'] for f in candidate['source_files']}
        self.policy = load_json(self.root / 'contracts/release-policy.json') \
            if 'contracts/release-policy.json' in tracked else None
        if self.policy is not None and profile not in self.policy['profiles']:
            raise ValueError(f'unknown release profile: {profile}')
        self.names = self.policy['artifacts'] if self.policy else ARTIFACTS
        self.agents = self.policy['profiles'][profile].get('live_agents', {}) if self.policy else {}
        self.requirements, self.scenarios = requirements, scenarios
        # HoH's locked contracts, beside this script: a project's records follow them whatever the project is.
        self.schemas = {n: Draft202012Validator(load_json(ROOT / 'contracts' / f'{n}.schema.json'),
                                                format_checker=FormatChecker()) for n in SCHEMAS}

    def rule(self, check: str):
        """The policy's entry for `check`: None without a policy, False when the policy has no such check."""
        if self.policy is None:
            return None
        return self.policy['checks'].get(check, False)

    def degradable(self, check: str) -> bool:
        rule = self.rule(check)
        return bool(rule) and rule.get('degradable') is True

    def tests(self) -> dict[str, Test]:
        """The profile's TESTs: every AC of a requirement that targets the profile, by its declared test_id, with the
        scenario check contracts/acceptance-scenarios.json names."""
        requirements = self.requirements
        if requirements is None:
            requirements = load_json(safe_path(self.root, self.candidate['spec']['path']))['requirements']
        scenarios = self.scenarios
        if scenarios is None:
            scenarios = {s['id']: s['check_id']
                         for s in load_json(self.root / 'contracts/acceptance-scenarios.json')['scenarios']}
        found = {}
        for req in requirements:
            if self.profile in req['release_targets']:
                for ac in req['acceptance']:
                    found[ac['test_id']] = Test(scenarios.get(ac['test_id']), req['release_class'] == 'degradable')
        return found

    def holds(self, ref: dict) -> bool:
        """The file `ref` names is there and is what it recorded."""
        try:
            path = safe_path(self.root, ref['path'])
            return path.is_file() and sha256(path) == ref['sha256']
        except (ValueError, OSError, KeyError, TypeError):
            return False

    def inside(self, path: str, directory: Path) -> bool:
        """Resolved containment: '..' segments and links cannot leave the directory."""
        try:
            return safe_path(self.root, path).is_relative_to(directory)
        except ValueError:
            return False

    def candidate_at(self, ref: dict) -> bool:
        """The manifest `ref` names is what it recorded and an export of this candidate (rule 3)."""
        if not self.holds(ref):
            return False
        try:
            data = load_json(safe_path(self.root, ref['path']))
        except (ValueError, OSError):
            return False
        return (isinstance(data, dict) and self.schemas['candidate'].is_valid(data)
                and data['candidate_id'] == identity(data) == self.cid
                and all(data[k]['sha256'] == data[k + '_sha256'] for k in ('spec', 'environment', 'test_plan')))


class Member:
    """An execution record that claims the candidate, or that cannot be read and holds its id (damaged)."""
    def __init__(self, path: Path, rel: str, record: dict | None, damaged: bool):
        self.path, self.rel, self.record, self.damaged = path, rel, record, damaged
        self.dir = path.parent
        check = record.get('check_id') if isinstance(record, dict) else None
        self.check = check if isinstance(check, str) else None
        self.observations: list = []
        self.lines: list = []
        self.reasons: list[str] = ['RECORD_INVALID'] if damaged else []
        self.outcome: tuple[str, str | None] = ('INVALID', None)
        self.sessions: dict | None = None


def when(value) -> datetime:
    d = datetime.fromisoformat(value.replace('Z', '+00:00'))
    if d.tzinfo is None:
        raise ValueError('timestamp must include timezone')
    return d


def interval(record: dict) -> float | None:
    """The record's interval in seconds, when its times parse and do not run backwards."""
    try:
        seconds = (when(record['finished_at']) - when(record['started_at'])).total_seconds()
    except (ValueError, TypeError, AttributeError, KeyError):
        return None
    return seconds if seconds >= 0 else None


def agrees(record: dict) -> bool:
    """The duration agrees with the interval within max(5 s, 1 %), as tools/release_gate.py has it."""
    elapsed = interval(record)
    return elapsed is not None and abs(elapsed - record['duration_seconds']) <= max(5, elapsed * .01)


def claims_success(record: dict) -> bool:
    return record['exit_code'] == 0 and not record['timed_out']


def find(world: World) -> list[Member]:
    """Every member under the search roots, in path order; links to directories are not followed."""
    found = []
    for base in SEARCH:
        for dirpath, dirnames, filenames in os.walk(world.root / base):
            dirnames.sort()
            if 'execution.json' not in filenames:
                continue
            path = Path(dirpath) / 'execution.json'
            rel = str(path.relative_to(world.root))
            try:
                data = path.read_bytes()
                resolved = safe_path(world.root, rel)
            except (OSError, ValueError):
                continue
            try:
                record = json.loads(data)
            except ValueError:
                record = None
            claims = isinstance(record, dict) and record.get('candidate_id') == world.cid
            shaped = isinstance(record, dict) and world.schemas['execution'].is_valid(record)
            if claims and shaped:
                found.append(Member(resolved, rel, record, False))
            elif claims or (not shaped and world.cid.encode() in data):
                found.append(Member(resolved, rel, record if isinstance(record, dict) else None, True))
    return found


def observe(world: World, m: Member) -> None:
    """The member's observations (decision 8): its result lines; without any, its artifacts with its own conclusion."""
    r = m.record
    ref = next((a for a in r['artifacts'] if a['name'] == world.names['test_results']), None)
    if ref is not None:
        try:
            data = safe_path(world.root, ref['path']).read_bytes()
        except (OSError, ValueError):
            data = b''
        for raw in data.split(b'\n'):
            if raw.strip():
                try:
                    m.lines.append(json.loads(raw))
                except ValueError:
                    m.lines.append(raw.decode('utf-8', 'replace'))
    fallback = [{'artifact': a['name'], 'path': a['path'], 'sha256': a['sha256'], 'exit_code': r['exit_code'],
                 'timed_out': r['timed_out']} for a in r['artifacts']]
    # Only a list equal to the artifacts' fallback is taken for it, never a line by its keys (as the product does).
    if m.lines == fallback:
        m.lines = []
    m.observations = list(m.lines) or fallback


def sessions(world: World, m: Member) -> tuple[bool, dict | None]:
    """Whether the member lists a live-sessions artifact, and the document there if it parses."""
    ref = next((a for a in m.record['artifacts'] if a['name'] == world.names['live_sessions']), None)
    if ref is None:
        return False, None
    try:
        doc = load_json(safe_path(world.root, ref['path']))
    except (ValueError, OSError):
        return True, None
    return True, doc if isinstance(doc, dict) else None


def judge(world: World, m: Member, members: list[Member]) -> None:
    """The rule table's vetoes on one member (principle one; rule 17 needs the product's store and is not here)."""
    r, c = m.record, world.candidate
    success = claims_success(r)
    reasons = []

    def need(ok: bool, code: str) -> None:
        if not ok:
            reasons.append(code)
    # Rule 2.
    need(r['spec_sha256'] == c['spec_sha256'], 'SPEC_MISMATCH')
    need(r['environment_sha256'] == c['environment_sha256'], 'ENVIRONMENT_MISMATCH')
    need(r['test_plan_sha256'] == c['test_plan_sha256'], 'TEST_PLAN_MISMATCH')
    # Rule 9: an interval that runs backwards vetoes; a duration that disagrees only leaves the record incomplete.
    need(interval(r) is not None, 'INTERVAL_INVALID')
    # Rule 6.
    need(r['source_unchanged'] is True, 'SOURCE_CHANGED')
    # Rule 7.
    need(world.holds(r['stdout']), 'OUTPUT_CHANGED')
    need(world.holds(r['stderr']), 'ERROR_OUTPUT_CHANGED')
    # Rule 12: only a record claiming success.
    need(not success or not r['missing_artifacts'], 'ARTIFACT_MISSING')
    need(not success or bool(m.observations), 'NO_OBSERVATIONS')
    # Rule 1: every line of the results file is a result line.
    need(all(isinstance(o, dict) and world.schemas['test-result'].is_valid(o) for o in m.lines),
         'RESULT_LINE_INVALID')
    need(all(world.holds(a) for a in r['artifacts']), 'ARTIFACT_CHANGED')
    # Rule 11: a record claiming success hides no failure.
    need(not success or not any(isinstance(o, dict) and o.get('status') == 'FAIL' for o in m.lines),
         'EXIT_INCONSISTENT')
    # Rule 4.
    planned = world.plan.get(r['check_id'])
    rule = world.rule(r['check_id'])
    if planned is None:
        need(False, 'CHECK_NOT_PLANNED')
    else:
        need(r['command'] == planned['argv'], 'COMMAND_MISMATCH')
        need(r['mode'] == planned['mode'], 'MODE_MISMATCH')
        need(Path(r['cwd']) == Path(planned.get('cwd', '.')), 'CWD_MISMATCH')
        accounted = {a['name'] for a in r['artifacts']} | set(r['missing_artifacts'])
        need(accounted == set(planned.get('artifacts', [])), 'ARTIFACTS_DIFFER_FROM_PLAN')
        if rule:
            need(rule['mode'] == planned['mode'], 'PLAN_MODE_NOT_POLICY')
    need(rule is not False, 'CHECK_NOT_IN_POLICY')
    # Rule 3.
    need(world.candidate_at(r['candidate_manifest']), 'MANIFEST_MISMATCH')
    # Rule 5.
    need(r['runner']['tool'] in TOOLS, 'RUNNER_TOOL')
    need(r['runner']['euid'] != 0, 'RUNNER_ROOT')
    # Rule 7: inside its own directory, which no other member's overlaps.
    need(all(world.inside(ref['path'], m.dir) for ref in [r['stdout'], r['stderr']] + r['artifacts']),
         'OUTSIDE_RECORD_DIR')
    need(not any(o is not m and (o.dir == m.dir or o.dir.is_relative_to(m.dir) or m.dir.is_relative_to(o.dir))
                 for o in members), 'RECORD_DIR_OVERLAP')
    # Rule 19.
    need(not any(o is not m and o.record is not None and not o.damaged and o.record['record_id'] == r['record_id']
                 for o in members), 'RECORD_ID_REUSED')
    # Rule 10: a live record's sessions, there whether or not the plan declares them.
    if r['mode'] == 'live':
        listed, doc = sessions(world, m)
        if listed:
            bound = (doc is not None and world.schemas['live-session'].is_valid(doc)
                     and doc['check_id'] == r['check_id'] and doc['harness_candidate_id'] == world.cid)
            need(bound, 'LIVE_SESSIONS_UNBOUND')
            m.sessions = doc if bound else None
        else:
            need(not success or world.names['live_sessions'] in r['missing_artifacts'], 'LIVE_SESSIONS_MISSING')
    m.reasons = reasons


def based(line) -> bool:
    """A DEGRADED line carries its basis: a `detail` naming the reason and the upstream reference (rule 14)."""
    return isinstance(line.get('detail'), str) and bool(line['detail'].strip())


def outcome(world: World, m: Member) -> tuple[str, str | None]:
    """Principle one's outcome function: the first of its seven ordered rules that holds."""
    if m.reasons:
        return 'INVALID', 'INSUFFICIENT' if m.reasons == ['NO_OBSERVATIONS'] else None
    r = m.record
    if r['timed_out']:
        return 'FAIL', 'TIMED_OUT'
    rule = world.rule(r['check_id']) or None
    complete = not r['missing_artifacts'] and (r['mode'] != 'live' or m.sessions is not None) and agrees(r)
    lines = [o for o in m.lines if isinstance(o, dict)]
    degraded = [o for o in lines if o.get('status') == 'DEGRADED']
    diagnosed = any(s != EMPTY for s in [r['stdout']['sha256'], r['stderr']['sha256']]
                    + [a['sha256'] for a in r['artifacts']])
    compliant = (diagnosed and bool(rule) and rule.get('degradable') is True and bool(degraded)
                 and all(based(o) for o in degraded) and not any(o.get('status') == 'FAIL' for o in lines))
    if compliant and complete:
        return 'DEGRADED', None
    if r['exit_code'] == 0:
        plugins = not (rule and rule.get('requires_plugins') is True) or (
            m.sessions is not None and all(isinstance(s.get('plugins'), list) and s['plugins']
                                           for s in m.sessions['sessions'] if s.get('agent') == 'pi'))
        if not plugins:
            return 'FAIL', 'PLUGINS_MISSING'
        if complete:
            return 'PASS', None
    code = r['exit_code']
    if code == 3:
        return 'BLOCKED', None
    if code == 127:
        return 'FAIL', 'NOT_STARTED'
    if code == 130 or code < 0:
        return 'FAIL', 'INTERRUPTED'
    if code == 0:
        return 'FAIL', 'INCOMPLETE'
    return 'FAIL', None


def members(world: World) -> list[Member]:
    """Every member, judged: its observations, its vetoes and its outcome."""
    found = find(world)
    for m in found:
        if not m.damaged:
            observe(world, m)
    for m in found:
        if not m.damaged:
            judge(world, m, found)
            m.outcome = outcome(world, m)
    return found


def profile_agents(world: World, m: Member) -> bool:
    """Rule 16: the live sessions hold every agent the profile requires of this check."""
    required = set(world.agents.get(m.record['check_id'], []))
    if not required:
        return True
    return m.sessions is not None and required <= {s['agent'] for s in m.sessions['sessions']}


def soak(world: World, m: Member) -> bool:
    """Rule 18: a soak run long enough, its timeline valid, ordered, inside the run, bound per event to the candidate,
    with enough distinct accepted live iterations."""
    if world.policy is None or m.record['check_id'] != 'soak':
        return True
    r, policy = m.record, world.policy
    wall = interval(r)
    if wall is None or min(wall, r['duration_seconds']) < policy['minimum_soak_seconds']:
        return False
    ref = next((a for a in r['artifacts'] if a['name'] == world.names['soak_timeline']), None)
    if ref is None or not world.holds(ref):
        return False
    start, finish, previous, ids = when(r['started_at']), when(r['finished_at']), None, set()
    for raw in safe_path(world.root, ref['path']).read_bytes().split(b'\n'):
        if not raw.strip():
            continue
        try:
            event = json.loads(raw)
        except ValueError:
            return False
        if not world.schemas['soak-event'].is_valid(event) or event['harness_candidate_id'] != world.cid:
            return False
        at = when(event['occurred_at'])
        if (previous is not None and at < previous) or not start <= at <= finish:
            return False
        previous = at
        if event['kind'] == 'iteration.accepted' and event['mode'] == 'live':
            if event['iteration_id'] in ids:
                return False
            ids.add(event['iteration_id'])
    return len(ids) >= policy['minimum_live_iterations']


def conclude(world: World, m: Member, test_id: str, test: Test) -> str | None:
    """Principle one's conclusion function: what member `m` concludes on TEST `test_id`."""
    if m.reasons:
        return 'INVALID'
    status = m.outcome[0]
    scenario = test.check is not None and m.check == test.check
    lines = [o for o in m.lines if isinstance(o, dict) and o.get('test_id') == test_id]
    degraded = [o for o in lines if o.get('status') == 'DEGRADED']
    compliant = scenario and test.degradable and world.degradable(m.check) and all(based(o) for o in degraded)
    if any(o.get('status') == 'FAIL' for o in lines) or (degraded and not compliant):
        return 'FAIL'
    if (scenario and status == 'PASS' and lines and all(o.get('status') == 'PASS' for o in lines)
            and profile_agents(world, m) and soak(world, m)):
        return 'PASS'
    if scenario and status == 'DEGRADED' and degraded and all(o.get('status') in SATISFIED for o in lines):
        return 'DEGRADED'
    return None


def own_conclusion(world: World, lines: list[dict], test_id: str, test: Test) -> str | None:
    """This run's self-test lines on a TEST of this check: its outcome comes only after it exits, so all PASS."""
    mine = [o for o in lines if o['test_id'] == test_id]
    degraded = [o for o in mine if o['status'] == 'DEGRADED']
    compliant = test.degradable and world.degradable(world.check_id) and all(based(o) for o in degraded)
    if any(o['status'] == 'FAIL' for o in mine) or (degraded and not compliant):
        return 'FAIL'
    if mine and all(o['status'] == 'PASS' for o in mine):
        return 'PASS'
    return None


def report(world: World, found: list[Member], own: list[dict]) -> dict:
    """Each TEST's status over all members (principle one), the invalid members, and what is unsatisfied."""
    tests = {}
    for test_id, test in sorted(world.tests().items()):
        mine = test.check == world.check_id
        concluded = []
        for m in found:
            if m.reasons:
                continue
            c = conclude(world, m, test_id, test)
            # An earlier run of this check takes part in vetoes and failures only (decision 10).
            if c and not (mine and m.check == world.check_id and c in SATISFIED):
                concluded.append((m.rel, c))
        if mine:
            c = own_conclusion(world, own, test_id, test)
            if c:
                concluded.append(('this run', c))
        scenario = [m for m in found if test.check is not None and m.check == test.check]
        named = [m for m in found
                 if m in scenario or any(isinstance(o, dict) and o.get('test_id') == test_id for o in m.lines)]
        good = [c for _, c in concluded if c in SATISFIED]
        if any(c == 'FAIL' for _, c in concluded):
            status = 'FLAKY' if good else 'FAIL'
        elif any(m.reasons for m in named):
            status = 'INVALID'
        elif good:
            status = 'PASS' if 'PASS' in good else 'DEGRADED'
        elif mine or scenario:
            status = 'NOT_RUN'
        else:
            status = 'MISSING'
        passing = sorted({o['name'] for m in scenario if not mine and not m.reasons
                          and conclude(world, m, test_id, test) in SATISFIED
                          for o in m.lines if isinstance(o, dict) and o.get('test_id') == test_id}
                         | {o['name'] for o in own if mine and o['test_id'] == test_id and o['status'] in SATISFIED})
        tests[test_id] = {'check': test.check, 'members': sorted(m.rel for m in named), 'passing': passing,
                          'status': status}
    invalid = [{'path': m.rel, 'check_id': m.check, 'damaged': m.damaged, 'reasons': m.reasons}
               for m in found if m.reasons]
    unsatisfied = [{'id': t, 'status': v['status']} for t, v in tests.items() if v['status'] not in SATISFIED]
    return {'candidate_id': world.cid, 'profile': world.profile, 'tests': tests, 'unsatisfied': unsatisfied,
            'invalid': invalid}


def cases(suite) -> list:
    if isinstance(suite, unittest.TestCase):
        return [suite]
    return [t for s in suite for t in cases(s)]


class Recorder(unittest.TextTestResult):
    """Each self-test's own status: PASS, FAIL (failure, error, expected failure, unexpected success) or SKIPPED."""
    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self.status: dict[str, str] = {}

    def addSuccess(self, test):
        super().addSuccess(test)
        self.status.setdefault(test.id(), 'PASS')

    def addFailure(self, test, err):
        super().addFailure(test, err)
        self.status[test.id()] = 'FAIL'

    def addError(self, test, err):
        super().addError(test, err)
        self.status[test.id()] = 'FAIL'

    def addSkip(self, test, reason):
        super().addSkip(test, reason)
        self.status.setdefault(test.id(), 'SKIPPED')

    def addExpectedFailure(self, test, err):
        super().addExpectedFailure(test, err)
        self.status[test.id()] = 'FAIL'

    def addUnexpectedSuccess(self, test):
        super().addUnexpectedSuccess(test)
        self.status[test.id()] = 'FAIL'

    def addSubTest(self, test, subtest, err):
        super().addSubTest(test, subtest, err)
        if err is not None:
            self.status[test.id()] = 'FAIL'


def self_test(root: Path, test_ids: list[str]) -> list[dict]:
    """Run the tests under tests/bundle whose names hold `test_NNN_MM` for one of `test_ids`: one result line each
    (contracts/test-result.schema.json); a selected test that reports nothing (its class failed to set up) is FAIL."""
    if not test_ids:
        return []
    loader = unittest.TestLoader()
    suite = loader.discover(str(root / 'tests/bundle'), pattern='test_*.py', top_level_dir=str(root / 'tests/bundle'))
    for error in loader.errors:
        print('traceability: tests/bundle discovery: ' + error.strip().splitlines()[-1], file=sys.stderr)
    wanted = set(test_ids)
    chosen = []
    for case in cases(suite):
        method = case.id().rsplit('.', 1)[-1]
        ids = sorted({f'TEST-{a}-{b}' for a, b in TEST_ID.findall(method)} & wanted)
        if ids:
            chosen.append((case, ids))
    if not chosen:
        return []
    runner = unittest.TextTestRunner(stream=sys.stderr, verbosity=2, resultclass=Recorder)
    result = runner.run(unittest.TestSuite(case for case, _ in chosen))
    return [{'schema_version': '1.0', 'test_id': t, 'name': case.id()[:512],
             'status': result.status.get(case.id(), 'FAIL')} for case, ids in chosen for t in ids]


def main(argv=None) -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument('profile', help='The release profile whose TESTs are judged (contracts/release-policy.json).')
    a = p.parse_args(argv)
    out, candidate_file = os.environ.get('HOH_EVIDENCE_DIR'), os.environ.get('HOH_CANDIDATE_FILE')
    if not out or not candidate_file:
        print('traceability: run it through tools/run_check.py (HOH_EVIDENCE_DIR, HOH_CANDIDATE_FILE)', file=sys.stderr)
        return 64
    try:
        world = World(ROOT, load_json(Path(candidate_file)), a.profile, os.environ.get('HOH_CHECK_ID', 'traceability'))
        tests = world.tests()
    except (ValueError, KeyError, TypeError, OSError) as exc:
        print(f'traceability: {exc}', file=sys.stderr)
        return 1
    own = self_test(ROOT, sorted(t for t, test in tests.items() if test.check == world.check_id))
    out_dir = Path(out)
    with (out_dir / world.names['test_results']).open('w', encoding='utf-8') as f:
        for line in own:
            f.write(json.dumps(line, ensure_ascii=False) + '\n')
    found = members(world)
    result = report(world, found, own)
    atomic_json(out_dir / 'traceability-report.json', result)
    for item in result['invalid']:
        print(f"traceability: INVALID {item['path']}: {', '.join(item['reasons'])}", file=sys.stderr)
    for item in result['unsatisfied']:
        print(f"traceability: {item['status']} {item['id']}", file=sys.stderr)
    ok = not result['unsatisfied'] and not result['invalid']
    print(f"traceability: {len(tests)} TESTs, {len(found)} members, "
          f"{len(result['unsatisfied'])} unsatisfied, {len(result['invalid'])} invalid", file=sys.stderr)
    return 0 if ok else 1


if __name__ == '__main__':
    sys.exit(main())
