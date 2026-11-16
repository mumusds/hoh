//! The M2-T13 test world (docs/adr/ADR-031.md decision 14, slice S5): a project of its own with a home, a runtime
//! directory and a PATH of its own, on which fake `codex`, `claude` and `pi` CLIs answer the probe and the compat
//! suite from their recordings and play the roles of a run through the product executor:
//!
//! - the Commander (on codex or claude) is the pinned script of M2T13D-52: from its turn's wake header (nonce, output
//!   file, run_id, role_epoch, expected_revision, spec digest, causation) and the run's state it submits, in one
//!   batch, a record_assumption and a create_tasks (each until one of them is applied: one REJECTED is submitted
//!   again) and the transition_run steps CREATED→DISCOVERING→SPECIFYING→READY→RUNNING still ahead, each at the
//!   revision the one before it leaves; once the run is RUNNING (or in any other state) it submits nothing, so it never
//!   moves a run past RUNNING;
//! - the Developer logs each start, writes stderr noise with a marker, works for a while, commits a change and
//!   writes its TaskResult;
//! - QA and the Reviewer run planned checks through their `run-check` helper, write a COMPLETED (or other)
//!   TaskResult and end with a `hoh-report` block of the configured status.
//!
//! What a fake does is read from `world.json` at each turn ([`World::developer`], [`World::verifier`],
//! [`World::commander`], [`World::unauthorized`]); every turn, Commander batch, Developer start and check is logged as
//! a JSON line ([`World::log`]). The world's processes (hosts its commands started, fake turns) are stopped when it
//! is dropped. A test binary using it declares `mod pty;` beside `mod world;`.
//!
//! A test that needs only the Commander's decisions runs an in-process host whose runner is the scripted [`Team`]
//! instead ([`World::team_host`]): its Commander is a Rust function of the turn, and it takes no work.

#![allow(dead_code)] // each test binary uses a part of it

use crate::pty;
use hoh_app::host::{Host, HostConfig};
use hoh_core::persistence::selftest::{PROBE_ARG, ProbeCommand};
use serde_json::{Value, json};
use std::ffi::{OsStr, OsString};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

pub use pty::R;

/// The product binary.
pub const HOH: &str = env!("CARGO_BIN_EXE_hoh");

/// The recorded protocol fixtures the fakes answer from.
const PROTOCOL: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/protocol");

/// hoh-agents' fake pi RPC server, read by path (no test code shared across crates).
const PI_RPC: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../hoh-agents/tests/fakes/pi_rpc.py"
);

/// How long one plain `hoh` command of the world may take before it is killed and its test fails.
pub const LIMIT: Duration = Duration::from_secs(60);

/// The built-in roles' agents in the world's project file: the Commander on codex, every working and verifying role on
/// claude (whose fake gives each new session a native id of its own, so QA's and the Reviewer's sessions are never
/// the Developer's), the researcher on pi.
pub const TEAM: [(&str, &str); 6] = [
    ("commander", "codex"),
    ("cto", "claude"),
    ("developer", "claude"),
    ("qa", "claude"),
    ("reviewer", "claude"),
    ("researcher", "pi"),
];

/// The tools on the world's PATH besides the fakes (and `hoh`): what the fakes, the host and its checks run. The
/// first ones must exist; the rest are linked when the machine has them.
const TOOLS: [&str; 8] = [
    "bash", "sh", "python3", "git", "cat", "cp", "mkdir", "timeout",
];
const MORE_TOOLS: [&str; 23] = [
    "env",
    "sleep",
    "head",
    "tail",
    "tr",
    "date",
    "rm",
    "mv",
    "ls",
    "grep",
    "sed",
    "stty",
    "setsid",
    "kill",
    "ps",
    "tty",
    "true",
    "false",
    "sha256sum",
    "wc",
    "tee",
    "touch",
    "chmod",
];

/// One acceptance criterion of the world's specification: its scenario check passes or fails.
#[derive(Debug, Clone)]
pub struct Criterion {
    pub id: String,
    pub body: String,
    pub test_id: String,
    pub passes: bool,
}

impl Criterion {
    /// Its check in the world's plan.
    pub fn check_id(&self) -> String {
        format!("check-{}", self.id.to_lowercase())
    }
}

/// One requirement of the world's specification.
#[derive(Debug, Clone)]
pub struct Requirement {
    pub id: String,
    pub title: String,
    pub criteria: Vec<Criterion>,
}

/// The default specification: REQ-901 with AC-901-01, whose check passes.
pub fn fixture_spec() -> Vec<Requirement> {
    vec![Requirement {
        id: "REQ-901".to_owned(),
        title: "完成 fixture 目标".to_owned(),
        criteria: vec![Criterion {
            id: "AC-901-01".to_owned(),
            body: "fixture 目标已完成".to_owned(),
            test_id: "TEST-901-01".to_owned(),
            passes: true,
        }],
    }]
}

/// How the fake Developer works a turn.
#[derive(Debug, Clone)]
pub struct Developer {
    /// How long it works before it commits and reports.
    pub seconds: f64,
    /// Bytes of noise written to stderr first, in lines starting with `stderr_marker`.
    pub stderr_bytes: u64,
    pub stderr_marker: String,
    /// Its TaskResult's status.
    pub status: String,
    /// Whether it commits a change (one line in `fake-work.txt`) before it reports, so each candidate differs.
    pub commit: bool,
}

impl Default for Developer {
    fn default() -> Self {
        Developer {
            seconds: 0.0,
            stderr_bytes: 0,
            stderr_marker: "HOH-WORLD-NOISE".to_owned(),
            status: "COMPLETED".to_owned(),
            commit: true,
        }
    }
}

/// How a fake verifier (qa or reviewer) works a turn.
#[derive(Debug, Clone)]
pub struct Verifier {
    /// Its TaskResult's status (CR-018: a QA FAIL is a COMPLETED result with a FAIL report).
    pub result: String,
    /// Its hoh-report block's status: PASS, FAIL or UNKNOWN.
    pub report: String,
    /// The planned checks it runs through its `run-check` helper first.
    pub checks: Vec<String>,
    /// Its report's findings.
    pub findings: Vec<Value>,
    /// Its report's scope; empty: the in-scope criteria its prompt lists.
    pub scope: Vec<String>,
}

impl Default for Verifier {
    fn default() -> Self {
        Verifier {
            result: "COMPLETED".to_owned(),
            report: "PASS".to_owned(),
            checks: Vec::new(),
            findings: Vec::new(),
            scope: Vec::new(),
        }
    }
}

/// What the pinned fake Commander submits.
#[derive(Debug, Clone)]
pub struct Commander {
    /// Its record_assumption's statement and basis.
    pub statement: String,
    pub basis: String,
    /// The suffixes of the developer tasks its create_tasks plans (`<run_id>-<suffix>`), each for every requirement of
    /// the project's `contracts/requirements.json`.
    pub tasks: Vec<String>,
    /// The decision kinds whose first submission for a run names a revision one past the run's, so the Runtime
    /// rejects it (STALE_REVISION) and the script submits it again.
    pub stale_once: Vec<String>,
}

impl Default for Commander {
    fn default() -> Self {
        Commander {
            statement: "fixture 默认：沿用项目已批准的规格，不另作假设".to_owned(),
            basis: "项目的 contracts/requirements.json".to_owned(),
            tasks: vec!["develop".to_owned()],
            stale_once: Vec::new(),
        }
    }
}

/// The fakes' turn (Python, standard library only): see the module's head. `argv`: the world's directory, the agent
/// (`claude`, `codex-app-server`, `codex-exec`), the protocol fixtures, then the agent's own arguments.
const AGENT_PY: &str = r##"import hashlib, json, os, re, sqlite3, subprocess, sys, time, uuid

WORLD, AGENT, RECORDINGS = sys.argv[1], sys.argv[2], sys.argv[3]
ARGS = sys.argv[4:]
ORDER = ['CREATED', 'DISCOVERING', 'SPECIFYING', 'READY', 'RUNNING']


def settings():
    try:
        with open(os.path.join(WORLD, 'world.json'), encoding='utf-8') as f:
            return json.load(f)
    except FileNotFoundError:
        return {}


def append(name, record):
    with open(os.path.join(WORLD, name), 'a', encoding='utf-8') as f:
        f.write(json.dumps(record, ensure_ascii=False) + '\n')


def frames(*path):
    with open(os.path.join(RECORDINGS, *path), encoding='utf-8') as f:
        # LF only: str.splitlines() would also split on U+2028/U+2029 inside JSON strings.
        return [json.loads(line) for line in f.read().split('\n') if line.strip()]


def texts(raw):
    """The prompt as it came, and every string of the JSON lines in it."""
    found = [raw]
    for line in raw.split('\n'):
        try:
            stack = [json.loads(line)]
        except ValueError:
            continue
        while stack:
            x = stack.pop()
            if isinstance(x, str):
                found.append(x)
            elif isinstance(x, dict):
                stack.extend(x.values())
            elif isinstance(x, list):
                stack.extend(x)
    return '\n'.join(found)


def section(text, title):
    """The `- key：value` lines of the first section whose heading starts with `title`, each key cut at its first
    full-width parenthesis."""
    at = text.find('\n' + title)
    if at < 0:
        return {}
    fields = {}
    for line in text[at + 1:].split('\n')[1:]:
        if line.startswith('#'):
            break
        if line.startswith('- ') and '：' in line:
            key, value = line[2:].split('：', 1)
            fields[key.split('（', 1)[0].strip()] = value.strip()
    return fields


def bridge_write(path, record):
    os.makedirs(os.path.dirname(path) or '.', exist_ok=True)
    with open(path, 'a', encoding='utf-8') as f:
        f.write(json.dumps(record, ensure_ascii=False) + '\n')


def task_result(ident, status, summary, next_action):
    bridge_write(ident['输出文件'], {
        'schema_version': '1.0', 'run_id': ident['run_id'], 'task_id': ident['task_id'],
        'attempt_id': ident['attempt_id'], 'role': ident['role'], 'role_epoch': int(ident['role_epoch']),
        'status': status, 'summary': summary, 'candidate_id': None, 'evidence': [], 'issues': [],
        'next_action': next_action})


def requirements(wake, c):
    if c.get('requirements'):
        return c['requirements']
    root = wake.get('项目根') or os.getcwd()
    with open(os.path.join(root, 'contracts', 'requirements.json'), encoding='utf-8') as f:
        return [r['id'] for r in json.load(f)['requirements']]


def written(run):
    """Every message the fake Commander wrote for `run` before this turn, from its own log: (attempt_id, message)."""
    try:
        with open(os.path.join(WORLD, 'commander.jsonl'), encoding='utf-8') as f:
            batches = [json.loads(line) for line in f.read().split('\n') if line.strip()]
    except FileNotFoundError:
        return []
    return [(b['attempt_id'], m) for b in batches if b['run_id'] == run for m in b['messages']]


# A turn that ended: its lines were recorded (or discarded) before it did.
ENDED = ('SETTLED', 'FAILED', 'CANCELLED', 'UNKNOWN_EFFECT')


def verdict(db, run, attempt, message):
    """What became of `message`, written in turn `attempt`: its decision's state (APPLIED; REJECTED; PROPOSED, a kind
    the product does not apply yet, which stays so), 'ENDED' when its line ended without a decision, None while it
    waits for its disposal."""
    mid = message['message_id']
    row = db.execute('SELECT state FROM decisions WHERE run_id = ? AND decision_id LIKE ?',
                     (run, '%/' + mid)).fetchone()
    if row:
        return row[0]
    line = db.execute("SELECT json_extract(payload_json, '$.output_id') FROM events WHERE run_id = ? "
                      "AND kind = 'supervisor.output' AND json_extract(payload_json, '$.attempt_id') = ? "
                      "AND instr(json_extract(payload_json, '$.text'), ?) > 0", (run, attempt, mid)).fetchone()
    if line:
        disposed = db.execute("SELECT 1 FROM events WHERE run_id = ? AND kind = 'supervisor.output_disposed' "
                              "AND json_extract(payload_json, '$.output_id') = ?", (run, line[0])).fetchone()
        return 'ENDED' if disposed else None
    discarded = db.execute("SELECT 1 FROM events WHERE run_id = ? AND kind = 'supervisor.output_discarded' "
                           "AND json_extract(payload_json, '$.attempt_id') = ?", (run, attempt)).fetchone()
    turn = db.execute('SELECT state FROM attempts WHERE attempt_id = ?', (attempt,)).fetchone()
    return 'ENDED' if discarded or (turn and turn[0] in ENDED) else None


def commander(text, wake, cfg):
    """The pinned script (M2T13D-52): record_assumption and create_tasks once per run, the transitions still ahead up
    to RUNNING, nothing in any other state. "Once" is by what became of the ones written before (read from the store):
    one APPLIED, one kept PROPOSED (a kind the product does not apply yet) or one still waiting for its disposal is
    the run's; one REJECTED or ended without a decision is submitted again. A kind in `stale_once` names a revision one
    past the run's (STALE_REVISION) the first time it is submitted for a run."""
    c = cfg.get('commander', {})
    run, state, revision = wake['run_id'], wake.get('Run 状态', ''), int(wake['expected_revision'])
    spec = re.search(r'spec_sha256：([0-9a-f]{64})', text)
    spec = spec.group(1) if spec else '0' * 64
    before = written(run)
    root = cfg.get('project') or wake.get('项目根') or os.getcwd()
    db = sqlite3.connect(os.path.join(root, '.hoh', 'state.db'), timeout=10)
    try:
        held = {m['payload']['kind'] for attempt, m in before
                if verdict(db, run, attempt, m) in ('APPLIED', 'PROPOSED', None)}
    finally:
        db.close()

    def once(kind):
        if kind in held:
            return False
        held.add(kind)
        return True

    payloads = []
    if state in ORDER[:-1]:
        if once('record_assumption'):
            payloads.append({'kind': 'record_assumption', 'statement': c.get('statement', 'fixture'),
                             'basis': c.get('basis', 'fixture')})
        for target in ORDER[ORDER.index(state) + 1:]:
            if target in ('READY', 'RUNNING') and once('create_tasks'):
                reqs = requirements(wake, c)
                payloads.append({'kind': 'create_tasks', 'plan': {'tasks': [
                    {'task_id': run + '-' + t, 'title': 'fixture ' + t, 'role': 'developer', 'writer': True,
                     'depends_on': [], 'requirement_ids': reqs} for t in c.get('tasks', ['develop'])]}})
            payloads.append({'kind': 'transition_run', 'to_state': target})
    attempt = wake['attempt_id']
    key = 'fake-' + hashlib.sha256(attempt.encode()).hexdigest()[:24]
    submitted = {m['payload']['kind'] for _, m in before}
    messages = []
    for i, payload in enumerate(payloads):
        stale = payload['kind'] in c.get('stale_once', []) and payload['kind'] not in submitted
        messages.append({
            'schema_version': '1.0', 'message_id': key + '-d' + str(i), 'run_id': run, 'task_id': wake['task_id'],
            'attempt_id': attempt, 'sender_role': 'commander', 'recipient_role': 'runtime',
            'role_epoch': int(wake['role_epoch']), 'correlation_id': key, 'causation_id': wake.get('causation_id'),
            'idempotency_key': key + '-d' + str(i), 'expected_revision': revision + i + (1 if stale else 0),
            'spec_sha256': spec, 'candidate_id': None, 'kind': 'decision', 'payload': payload})
    for message in messages:
        bridge_write(wake['输出文件'], {'nonce': wake['nonce'], 'message': message})
    append('commander.jsonl', {'agent': AGENT, 'run_id': run, 'state': state, 'revision': revision,
                               'attempt_id': attempt, 'messages': messages})
    return 'Run 状态 %s：提交了 %d 条决策' % (state, len(messages))


def developer(ident, cfg):
    d = cfg.get('developer', {})
    append('calls.jsonl', {'agent': AGENT, 'pid': os.getpid(), 'run_id': ident.get('run_id'),
                           'task_id': ident.get('task_id'), 'attempt_id': ident.get('attempt_id'),
                           'start_ns': time.time_ns()})
    left = int(d.get('stderr_bytes', 0))
    if left:
        line = (d.get('stderr_marker', 'HOH-WORLD-NOISE') + ' ' + 'x' * 120 + '\n').encode()
        while left > 0:
            piece = line[:left]
            sys.stderr.buffer.write(piece)
            left -= len(piece)
        sys.stderr.buffer.flush()
    time.sleep(float(d.get('seconds', 0)))
    if d.get('commit', True):
        with open('fake-work.txt', 'a', encoding='utf-8') as f:
            f.write(ident['attempt_id'] + '\n')
        git = ['git', '-c', 'user.name=fake', '-c', 'user.email=fake@example.invalid']
        subprocess.run(git + ['add', 'fake-work.txt'], check=True, stdout=subprocess.DEVNULL)
        subprocess.run(git + ['commit', '-q', '-m', 'fake work ' + ident['attempt_id']], check=True,
                       stdout=subprocess.DEVNULL)
    task_result(ident, d.get('status', 'COMPLETED'), 'done by the fake developer', 'verify')
    return '已完成'


def verifier(role, ident, text, cfg):
    v = cfg.get('verifiers', {}).get(role, {})
    for check in v.get('checks', []):
        done = subprocess.run([os.environ['HOH_RUN_CHECK'], check], stdout=subprocess.PIPE,
                              stderr=subprocess.STDOUT)
        append('checks.jsonl', {'agent': AGENT, 'role': role, 'attempt_id': ident.get('attempt_id'),
                                'check': check, 'exit': done.returncode,
                                'output': done.stdout.decode('utf-8', 'replace')[-2000:]})
    scope = v.get('scope') or [k for k in section(text, '#### 验收标准') if k.startswith('AC-')] or ['fixture']
    block = {'role': role, 'status': v.get('report', 'PASS'), 'scope': scope, 'findings': v.get('findings', [])}
    task_result(ident, v.get('result', 'COMPLETED'), 'verified by the fake ' + role, 'report')
    return '验证完成\n```hoh-report\n' + json.dumps(block, ensure_ascii=False) + '\n```\n'


def turn(prompt):
    """The turn's work by its prompt's header: its final message (None: the recording's) and its failure (None, or
    'unauthorized': nothing done)."""
    cfg = settings()
    text = texts(prompt)
    wake = section(text, '#### 本次唤醒')
    if 'nonce' in wake:
        role, ident = 'commander', wake
    else:
        ident = section(text, '#### 本次 Attempt')
        role = ident.get('role', '')
    record = {'agent': AGENT, 'pid': os.getpid(), 'role': role, 'run_id': ident.get('run_id'),
              'task_id': ident.get('task_id'), 'attempt_id': ident.get('attempt_id'),
              'marker': os.environ.get('HOH_ATTEMPT_ID'), 'cwd': os.getcwd()}
    os.makedirs(os.path.join(WORLD, 'prompts'), exist_ok=True)
    with open(os.path.join(WORLD, 'prompts', '%s.%d.txt' % (AGENT, os.getpid())), 'w', encoding='utf-8') as f:
        f.write(prompt)
    append('turns.jsonl', dict(record, event='start', at_ns=time.time_ns()))
    final, failure = None, None
    if role in cfg.get('unauthorized', {}).get(AGENT.split('-')[0], []):
        failure = 'unauthorized'
    elif role == 'commander':
        final = commander(text, wake, cfg)
    elif role == 'developer':
        final = developer(ident, cfg)
    elif role in ('qa', 'reviewer'):
        final = verifier(role, ident, text, cfg)
    append('turns.jsonl', dict(record, event='end', at_ns=time.time_ns(), failure=failure))
    return final, failure


def emit(frame):
    sys.stdout.write(json.dumps(frame, ensure_ascii=False) + '\n')
    sys.stdout.flush()


def claude():
    """claude -p: the prompt on stdin; the recorded success turn, its session the resumed one or a new one, its result
    the turn's final message; unauthorized: the recorded unauthenticated turn, exit 1."""
    prompt = sys.stdin.buffer.read().decode('utf-8', 'replace')
    session = ARGS[ARGS.index('--resume') + 1] if '--resume' in ARGS else str(uuid.uuid4())

    def out(frame):
        if 'session_id' in frame:
            frame['session_id'] = session
        emit(frame)

    recorded = frames('claude', '2.1.318', 'success', 'stdout.jsonl')
    out(recorded[0])
    final, failure = turn(prompt)
    if failure:
        for frame in frames('claude', '2.1.318', 'unauthenticated', 'stdout.jsonl')[1:]:
            out(frame)
        return 1
    for frame in recorded[1:]:
        if frame.get('type') == 'result' and final is not None:
            frame['result'] = final
        out(frame)
    return 0


def replaced(frame, old, new):
    return json.loads(json.dumps(frame).replace(old, new)) if old else frame


def segments(recording):
    """Each recorded response with the notifications after it, by the method of the request it answers."""
    requests = {}
    with open(os.path.join(RECORDINGS, 'codex', '0.166.0', recording, 'stdin.txt'), encoding='utf-8') as f:
        for line in f:
            try:
                m = json.loads(line)
            except ValueError:
                continue
            if 'id' in m and 'method' in m:
                requests[m['id']] = m['method']
    found, current = {}, None
    for frame in frames('codex', '0.166.0', recording, 'stdout.jsonl'):
        if 'method' not in frame and 'id' in frame:
            current = (frame, [])
            found.setdefault(requests.get(frame['id']), current)
        elif current is not None:
            current[1].append(frame)
    return found


def codex_app_server():
    """codex app-server: answers each request from the recorded app-server-turn (thread/resume from app-server-resume),
    a new thread a new id; a turn's work runs once turn/started is out, its final message replaces the recorded
    agent message; unauthorized: turn/completed failed with codexErrorInfo unauthorized (the synthesis of
    hoh-agents' codex_adapter tests)."""
    turn_rec, resume_rec = segments('app-server-turn'), segments('app-server-resume')
    thread = None
    for line in sys.stdin:
        try:
            m = json.loads(line)
        except ValueError:
            continue
        method, mid = m.get('method'), m.get('id')
        append('codex-requests.jsonl', {'pid': os.getpid(), 'method': method, 'id': mid})
        if mid is None:
            continue
        if method == 'initialize':
            response, notes = turn_rec['initialize']
        elif method == 'thread/start':
            response, notes = turn_rec['thread/start']
            old, thread = response['result']['thread']['id'], str(uuid.uuid4())
            response, notes = replaced(response, old, thread), [replaced(n, old, thread) for n in notes]
        elif method == 'thread/resume':
            response, notes = resume_rec['thread/resume']
            old, thread = response['result']['thread']['id'], m['params']['threadId']
            response, notes = replaced(response, old, thread), [replaced(n, old, thread) for n in notes]
        elif method == 'turn/start':
            response, notes = turn_rec['turn/start']
            recorded_thread = turn_rec['thread/start'][0]['result']['thread']['id']
            old_turn, new_turn = response['result']['turn']['id'], str(uuid.uuid4())
            fix = lambda f: replaced(replaced(f, recorded_thread, thread or recorded_thread), old_turn, new_turn)
            response, notes = fix(response), [fix(n) for n in notes]
            response['id'] = mid
            emit(response)
            started = next(i for i, n in enumerate(notes) if n.get('method') == 'turn/started')
            for note in notes[:started + 1]:
                emit(note)
            prompt = ''.join(i.get('text', '') for i in m['params'].get('input', []) if i.get('type') == 'text')
            final, failure = turn(prompt)
            first_delta = True
            for note in notes[started + 1:]:
                kind, params = note.get('method'), note.get('params', {})
                item = params.get('item', {})
                if kind == 'item/agentMessage/delta' and final is not None:
                    params['delta'], first_delta = (final if first_delta else ''), False
                if item.get('type') == 'agentMessage' and kind == 'item/completed' and final is not None:
                    item['text'] = final
                if kind == 'turn/completed':
                    for it in params['turn'].get('items', []):
                        if it.get('type') == 'agentMessage' and final is not None:
                            it['text'] = final
                    if failure:
                        params['turn']['status'] = 'failed'
                        params['turn']['error'] = {'message': '401 Unauthorized', 'codexErrorInfo': 'unauthorized',
                                                   'additionalDetails': None}
                emit(note)
            continue
        else:
            response, notes = {'id': mid, 'result': {}}, []
        response = dict(response, id=mid)
        emit(response)
        for note in notes:
            emit(note)
    return 0


def codex_exec():
    """codex exec: the prompt on stdin; the recorded exec turn, a new thread id, its agent message the final one."""
    prompt = sys.stdin.buffer.read().decode('utf-8', 'replace')
    recorded = frames('codex', '0.166.0', 'exec', 'stdout.jsonl')
    old, thread = recorded[0]['thread_id'], str(uuid.uuid4())
    emit(replaced(recorded[0], old, thread))
    final, failure = turn(prompt)
    if failure:
        return 1
    for frame in recorded[1:]:
        item = frame.get('item', {})
        if item.get('type') == 'agent_message' and final is not None:
            item['text'] = final
        emit(replaced(frame, old, thread))
    return 0


sys.exit({'claude': claude, 'codex-app-server': codex_app_server, 'codex-exec': codex_exec}[AGENT]())
"##;

/// The product binary's WAL self-test probe.
pub fn probe() -> ProbeCommand {
    ProbeCommand {
        program: HOH.into(),
        args: vec![PROBE_ARG.into()],
        env: Vec::new(),
        answer_timeout: Duration::from_secs(10),
    }
}

/// A test's world: see the module's head. Its directory is under /tmp, short, as a host's socket path needs.
#[derive(Debug)]
pub struct World {
    dir: tempfile::TempDir,
    setsid: PathBuf,
    /// The project directory's name.
    name: String,
}

impl World {
    /// A new world: its directories, the tools and fakes on its PATH, the fakes' default settings and the project's
    /// `.hoh/config.toml` with [`TEAM`]. Nothing is registered or started.
    pub fn new() -> R<World> {
        World::named("p")
    }

    /// [`World::new`] with the project directory named `name` (the Workspace's top bar shows it), short as a socket
    /// path needs.
    pub fn named(name: &str) -> R<World> {
        let dir = tempfile::Builder::new().prefix("hohw").tempdir_in("/tmp")?;
        let world = World {
            dir,
            setsid: found("setsid")?,
            name: name.to_owned(),
        };
        for d in [
            world.project(),
            world.home(),
            world.bin(),
            world.tools(),
            world.fakes(),
            world.state(),
        ] {
            std::fs::create_dir_all(d)?;
        }
        std::fs::create_dir_all(world.runtime_dir())?;
        std::fs::set_permissions(world.runtime_dir(), std::fs::Permissions::from_mode(0o700))?;
        for tool in TOOLS {
            std::os::unix::fs::symlink(found(tool)?, world.tools().join(tool))?;
        }
        for tool in MORE_TOOLS {
            if let Ok(path) = found(tool) {
                std::os::unix::fs::symlink(path, world.tools().join(tool))?;
            }
        }
        std::os::unix::fs::symlink(HOH, world.tools().join("hoh"))?;
        std::fs::write(world.fakes().join("agent.py"), AGENT_PY)?;
        world.write_fakes()?;
        world.commander(&Commander::default())?;
        world.developer(&Developer::default())?;
        for role in ["qa", "reviewer"] {
            world.verifier(role, &Verifier::default())?;
        }
        world.configure(&TEAM, "")?;
        // The fake Commander reads the store there (what became of the decisions it wrote).
        world.set(&["project"], json!(world.project()))?;
        Ok(world)
    }

    /// The world's root directory.
    pub fn root(&self) -> &Path {
        self.dir.path()
    }

    /// The project.
    pub fn project(&self) -> PathBuf {
        self.root().join(&self.name)
    }

    /// `HOME` of the world's commands.
    pub fn home(&self) -> PathBuf {
        self.root().join("home")
    }

    /// `XDG_RUNTIME_DIR` of the world's commands (no user manager answers there: a host is started directly).
    pub fn runtime_dir(&self) -> PathBuf {
        self.root().join("run")
    }

    /// The fake agent CLIs.
    pub fn bin(&self) -> PathBuf {
        self.root().join("bin")
    }

    /// The tools on the PATH besides the fakes, and `hoh`.
    pub fn tools(&self) -> PathBuf {
        self.root().join("tools")
    }

    /// The fakes' turn script, settings and logs.
    pub fn fakes(&self) -> PathBuf {
        self.root().join("w")
    }

    /// The state directory of an in-process host's Commander sessions (a host the client starts has its own under
    /// [`World::home`]).
    pub fn state(&self) -> PathBuf {
        self.root().join("state")
    }

    /// The PATH of the world's commands and hosts: the fakes, then the tools.
    pub fn path(&self) -> OsString {
        let mut path = self.bin().into_os_string();
        path.push(":");
        path.push(self.tools());
        path
    }

    /// The environment of the world's commands (nothing else of the test's).
    pub fn env(&self) -> Vec<(&'static str, OsString)> {
        vec![
            ("HOME", self.home().into_os_string()),
            ("PATH", self.path()),
            ("XDG_RUNTIME_DIR", self.runtime_dir().into_os_string()),
            ("LANG", "C.UTF-8".into()),
        ]
    }

    /// The fake CLIs: each answers its probe and compat suite as its recording does and runs a turn with
    /// [`AGENT_PY`]. Written once: a fake's file never changes after (the stamps its registration was earned on).
    fn write_fakes(&self) -> R {
        let (agent, w) = (
            self.fakes().join("agent.py").display().to_string(),
            self.fakes().display().to_string(),
        );
        let run = |name: &str| format!("exec python3 '{agent}' '{w}' {name} '{PROTOCOL}' \"$@\"");
        self.script(
            "claude",
            &format!(
                "f='{PROTOCOL}/claude/2.1.318'\n\
                 case \"$1\" in --version) echo '2.1.318 (Claude Code)'; exit 0;; --help) cat \"$f/help.txt\"; exit 0;; \
                 auth) exit 1;; esac\n{}\n",
                run("claude")
            ),
        )?;
        self.script(
            "codex",
            &format!(
                "f='{PROTOCOL}/codex/0.166.0'\n\
                 case \"$1\" in\n\
                  --version) echo 'codex-cli 0.166.0'; exit 0;;\n\
                  --help) cat \"$f/help.txt\"; exit 0;;\n\
                  exec) [ \"$2\" = --help ] && {{ cat \"$f/help-exec.txt\"; exit 0; }}\n   {};;\n\
                  app-server) case \"$2\" in\n\
                    --help) cat \"$f/help-app-server.txt\"; exit 0;;\n\
                    generate-json-schema) [ \"$3\" = --out ] && mkdir -p \"$4\" && cp -R \"$f/app-server-schema/.\" \"$4/\"; exit 0;;\n\
                   esac\n   {};;\n\
                 esac\nexit 2\n",
                run("codex-exec"),
                run("codex-app-server")
            ),
        )?;
        // pi plays no working role in the world (the researcher's, never dispatched): its turn replays the recorded
        // rpc-prompt.
        let pi = self.bin().join("pi.d");
        std::fs::create_dir_all(&pi)?;
        let mut commands = serde_json::Map::new();
        for line in
            std::fs::read_to_string(format!("{PROTOCOL}/pi/1.5.0/rpc-prompt/stdin.txt"))?.lines()
        {
            let Ok(c) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if let (Some(id), Some(kind)) = (c["id"].as_str(), c["type"].as_str()) {
                commands.insert(id.to_owned(), json!(kind));
            }
        }
        std::fs::write(
            pi.join("commands.json"),
            Value::Object(commands).to_string(),
        )?;
        self.script(
            "pi",
            &format!(
                "f='{PROTOCOL}/pi/1.5.0'; d='{d}'\n\
                 case \"$1\" in --version) echo '1.5.0'; exit 0;; --help) cat \"$f/help.txt\"; exit 0;; esac\n\
                 exec timeout 60 python3 '{PI_RPC}' \"$f/rpc-prompt/stdout.jsonl\" \"$d/commands.json\" \"$d/log.$$\" -- \"$@\"\n",
                d = pi.display()
            ),
        )
    }

    /// The executable `name` in [`World::bin`] running `body` (bash).
    fn script(&self, name: &str, body: &str) -> R {
        let path = self.bin().join(name);
        std::fs::write(&path, format!("#!/bin/bash\n{body}"))?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
        Ok(())
    }

    /// The project's `.hoh/config.toml`: `toml` (top-level keys first, then tables), then each `(role, agent)` of
    /// `team` as `[team.<role>]`. Read by a host as it starts and by each new run.
    pub fn configure(&self, team: &[(&str, &str)], toml: &str) -> R {
        let mut text = format!("config_version = \"1.0\"\n{toml}\n");
        for (role, agent) in team {
            text.push_str(&format!("[team.{role}]\nagent = \"{agent}\"\n"));
        }
        std::fs::create_dir_all(self.project().join(".hoh"))?;
        std::fs::write(self.project().join(".hoh/config.toml"), text)?;
        Ok(())
    }

    /// The project without its `.hoh/config.toml`: its configuration is the built-in one (a host then writes no
    /// ConfigureTeam of its own as it starts, docs/adr/ADR-022.md decision 6).
    pub fn unconfigure(&self) -> R {
        std::fs::remove_file(self.project().join(".hoh/config.toml"))?;
        std::fs::remove_dir(self.project().join(".hoh"))?;
        Ok(())
    }

    /// The project as a committed Git repository with specification `spec` in `contracts/requirements.json` and its
    /// check plan in `tests/check-plan.json` (each criterion's check writes its TEST's result line, PASS or FAIL as
    /// the criterion says, and fails with it); the specification's sha256.
    pub fn commit_project(&self, spec: &[Requirement]) -> R<String> {
        let requirements = json!({"requirements": spec.iter().map(|r| json!({
            "id": r.id, "title": r.title, "acceptance": r.criteria.iter().map(|c| json!({
                "id": c.id, "body": c.body, "test_id": c.test_id, "check_id": c.check_id(),
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>()});
        let mut checks = serde_json::Map::new();
        for c in spec.iter().flat_map(|r| &r.criteria) {
            let status = if c.passes { "PASS" } else { "FAIL" };
            let line = json!({"schema_version": "1.0", "test_id": c.test_id,
                "name": format!("{}_world", c.test_id.to_lowercase().replace('-', "_")), "status": status});
            checks.insert(
                c.check_id(),
                json!({"mode": "product", "artifacts": ["test-results.jsonl"], "timeout_seconds": 60,
                    "argv": ["sh", "-c", format!("printf '%s\\n' '{line}' > \"$HOH_EVIDENCE_DIR/test-results.jsonl\"; test {status} = PASS")]}),
            );
        }
        let plan = json!({"schema_version": "1.0", "plan_id": "m2t13-world", "checks": checks});
        let text = format!("{}\n", serde_json::to_string_pretty(&requirements)?);
        for (path, body) in [
            ("src/lib.rs", "pub fn fixture() {}\n".to_owned()),
            ("contracts/requirements.json", text.clone()),
            (
                "tests/check-plan.json",
                format!("{}\n", serde_json::to_string_pretty(&plan)?),
            ),
            (".gitignore", ".hoh/\n".to_owned()),
        ] {
            let file = self.project().join(path);
            std::fs::create_dir_all(file.parent().ok_or("no parent")?)?;
            std::fs::write(file, body)?;
        }
        self.git(&["init", "-q"])?;
        self.git(&["add", "."])?;
        self.git(&["commit", "-q", "-m", "fixture project"])?;
        Ok(sha256_hex(text.as_bytes()))
    }

    /// `git args…` in the project; its stdout.
    pub fn git(&self, args: &[&str]) -> R<String> {
        let out = Command::new(self.tools().join("git"))
            .args([
                "-c",
                "user.name=world",
                "-c",
                "user.email=world@example.invalid",
            ])
            .args(args)
            .current_dir(self.project())
            .env_clear()
            .envs(self.env())
            .output()?;
        if !out.status.success() {
            return Err(format!("git {args:?}: {}", String::from_utf8_lossy(&out.stderr)).into());
        }
        Ok(String::from_utf8(out.stdout)?)
    }

    /// The committed specification's rows in the store (specs, requirements, acceptance_criteria), as an approved
    /// specification has them (docs/adr/ADR-031.md decision 1: the fixture writes them; ADR-028 decision 4: its
    /// sha256 is the digest of the project's `contracts/requirements.json`); the spec_id.
    pub fn seed_spec(&self) -> R<String> {
        let text = std::fs::read(self.project().join("contracts/requirements.json"))?;
        let spec: Value = serde_json::from_slice(&text)?;
        let id = "spec-world";
        let mut sql = format!(
            "INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('{id}', :p, '{}', 1);\n",
            sha256_hex(&text)
        );
        for r in spec["requirements"].as_array().ok_or("no requirements")? {
            sql.push_str(&format!(
                "INSERT INTO requirements(requirement_id, spec_id, title, required_release) VALUES ({}, '{id}', {}, 1);\n",
                sql_text(&r["id"]),
                sql_text(&r["title"])
            ));
            for c in r["acceptance"].as_array().ok_or("no acceptance")? {
                sql.push_str(&format!(
                    "INSERT INTO acceptance_criteria(ac_id, requirement_id, spec_id, body, check_id) VALUES \
                     ({}, {}, '{id}', {}, {});\n",
                    sql_text(&c["id"]),
                    sql_text(&r["id"]),
                    sql_text(&c["body"]),
                    sql_text(&c["check_id"])
                ));
            }
        }
        self.exec(&sql)?;
        Ok(id.to_owned())
    }

    /// Run `run` on specification `spec` with the wake of its adoption, as StartRun gives a run the project's latest
    /// approved specification (docs/adr/ADR-031.md decision 1): written here only where the product has not (before
    /// slice S2b it does not; once S2b is in, StartRun adopts the spec itself and this writes nothing), so the tests
    /// using it hold before and after it.
    pub fn adopt(&self, run: &str, spec: &str) -> R {
        let sha = self.query(&format!(
            "SELECT sha256 FROM specs WHERE spec_id = '{spec}'"
        ))?;
        self.exec(&format!(
            "UPDATE runs SET spec_id = '{spec}' WHERE run_id = '{run}' AND spec_id IS NULL;
             INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at)
               SELECT :p, 'adopted-{run}', '{run}', 'spec.changed', json_object('run_id', '{run}', 'spec_id', '{spec}',
                 'spec_revision', 1, 'spec_sha256', '{sha}', 'from', NULL, 'cause', 'adopted'), '2026-11-15T00:00:00Z'
               WHERE NOT EXISTS (SELECT 1 FROM events WHERE run_id = '{run}' AND kind = 'spec.changed');"
        ))
    }

    /// The check ids of the committed plan.
    pub fn checks(&self) -> R<Vec<String>> {
        let plan: Value = serde_json::from_slice(&std::fs::read(
            self.project().join("tests/check-plan.json"),
        )?)?;
        Ok(plan["checks"]
            .as_object()
            .map(|c| c.keys().cloned().collect())
            .unwrap_or_default())
    }

    /// Each fake agent registered in the project's compat manifest by the product's compat suite
    /// (`hoh_app::compat::register`, docs/adr/ADR-030.md decision 10), which must PASS; before any host starts, so no
    /// host waits for a suite.
    pub fn register(&self) -> R {
        for agent in [
            hoh_agents::compat::Agent::Codex,
            hoh_agents::compat::Agent::Claude,
            hoh_agents::compat::Agent::Pi,
        ] {
            let registered = hoh_app::compat::register(&self.project(), agent, &self.path())?;
            if format!("{:?}", registered.entry.suite.status) != "Pass" {
                return Err(
                    format!("{agent:?}'s compat suite did not pass: {registered:?}").into(),
                );
            }
        }
        Ok(())
    }

    /// The project's store made, as a host's first start makes it (a host without a runner, started and stopped).
    pub async fn init_store(&self) -> R {
        let mut config = HostConfig::new(&self.project());
        config.port = 0;
        config.store.wal_probe = Some(probe());
        Host::start(config).await?.shutdown().await;
        Ok(())
    }

    /// An in-process host of the project without a runner: it takes commands and serves clients; nothing wakes or
    /// works. Its clock is `clock` when given.
    pub async fn start_plain_host(&self, clock: Option<std::sync::Arc<FakeClock>>) -> R<Host> {
        let mut config = HostConfig::new(&self.project());
        config.port = 0;
        config.store.wal_probe = Some(probe());
        if let Some(clock) = clock {
            config.clock = hoh_app::host::HostClock(clock);
        }
        Ok(Host::start(config).await?)
    }

    /// An in-process host of the project whose runner is the scripted `team` ([`Team`]): every built-in role needs no
    /// capability and the Commander sessions' directories are the world's own; its clock is `clock` when given. A
    /// start refused while another start holds the project is tried again for 5 s.
    pub async fn team_host(
        &self,
        team: std::sync::Arc<Team>,
        clock: Option<std::sync::Arc<FakeClock>>,
    ) -> R<Host> {
        let mut config = HostConfig::new(&self.project());
        config.port = 0;
        config.store.wal_probe = Some(probe());
        if let Some(clock) = clock {
            config.clock = hoh_app::host::HostClock(clock);
        }
        config.runner = Some(hoh_app::host::RunnerConfig {
            executor: team,
            context: hoh_core::decision::Context {
                required_capabilities: TEAM
                    .iter()
                    .map(|(role, _)| ((*role).to_owned(), Default::default()))
                    .collect(),
                commander_dir: Some(self.state().join("hoh/commander")),
                ..hoh_core::decision::Context::default()
            },
        });
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            match Host::start(config.clone()).await {
                Err(e) if e.exit_code == 69 && Instant::now() < end => {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                other => return Ok(other?),
            }
        }
    }

    /// Each vacant role binding of run `run_id` given a session of its own (`world-<run>-<role>`), as staffing gives
    /// it on a host whose executor has an agent registry: the scripted [`Team`] has none, so its host leaves them
    /// vacant and its Commander is never given a turn.
    pub fn staff(&self, run_id: &str) -> R {
        self.exec(&format!(
            "INSERT INTO sessions(session_id, agent, role, agent_version, capabilities_json)
               SELECT 'world-' || run_id || '-' || role, agent, role, '1', '{{}}' FROM role_bindings
               WHERE run_id = '{run_id}' AND session_id IS NULL;
             UPDATE role_bindings SET session_id = 'world-' || run_id || '-' || role
               WHERE run_id = '{run_id}' AND session_id IS NULL;"
        ))
    }

    /// The product's host of the project in this process (docs/adr/ADR-022.md decision 17): the effective
    /// configuration from the project's file, the product executor on the world's PATH, pi's settings and the
    /// Commander sessions' directories the world's own, any free port.
    pub fn host_config(&self) -> R<HostConfig> {
        let effective = hoh_core::config::load(&self.project())?;
        let registry = std::sync::Arc::new(hoh_app::host::agents::Registry::with_pi_dir(
            self.path(),
            Some(self.root().join("pi-agent")),
        ));
        let mut config = HostConfig::product_with(&self.project(), effective, HOH.into(), registry);
        config.port = 0;
        config.store.wal_probe = Some(probe());
        if let Some(runner) = config.runner.as_mut() {
            runner.context.commander_dir = Some(self.state().join("hoh/commander"));
        }
        Ok(config)
    }

    /// [`World::host_config`] started (waiting while another start holds the project).
    pub async fn start_host(&self) -> R<Host> {
        let config = self.host_config()?;
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            match Host::start(config.clone()).await {
                Err(e) if e.exit_code == 69 && Instant::now() < end => {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                other => return Ok(other?),
            }
        }
    }

    /// The fake Developer's settings.
    pub fn developer(&self, d: &Developer) -> R {
        self.set(
            &["developer"],
            json!({"seconds": d.seconds, "stderr_bytes": d.stderr_bytes, "stderr_marker": d.stderr_marker,
                "status": d.status, "commit": d.commit}),
        )
    }

    /// The fake verifier `role`'s (qa or reviewer) settings.
    pub fn verifier(&self, role: &str, v: &Verifier) -> R {
        self.set(
            &["verifiers", role],
            json!({"result": v.result, "report": v.report, "checks": v.checks, "findings": v.findings,
                "scope": v.scope}),
        )
    }

    /// The pinned fake Commander's settings.
    pub fn commander(&self, c: &Commander) -> R {
        self.set(
            &["commander"],
            json!({"statement": c.statement, "basis": c.basis, "tasks": c.tasks, "stale_once": c.stale_once}),
        )
    }

    /// The turns of `roles` on `agent` (`codex` or `claude`) fail unauthorized, doing nothing: codex's
    /// `turn/completed` failed with `codexErrorInfo: unauthorized`, claude's recorded unauthenticated turn.
    pub fn unauthorized(&self, agent: &str, roles: &[&str]) -> R {
        self.set(&["unauthorized", agent], json!(roles))
    }

    /// `value` at `path` of `world.json`, the rest kept.
    fn set(&self, path: &[&str], value: Value) -> R {
        let file = self.fakes().join("world.json");
        let mut all: Value = match std::fs::read(&file) {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({}),
            Err(e) => return Err(e.into()),
        };
        let (last, parents) = path.split_last().ok_or("an empty path")?;
        let mut at = &mut all;
        for key in parents {
            if !at[*key].is_object() {
                at[*key] = json!({});
            }
            at = &mut at[*key];
        }
        at[*last] = value;
        // Replaced whole: a turn reading it meanwhile sees the old settings or the new, never half of them.
        let staged = self.fakes().join(".world.json.new");
        std::fs::write(&staged, serde_json::to_vec_pretty(&all)?)?;
        std::fs::rename(staged, file)?;
        Ok(())
    }

    /// The JSON lines of the fakes' log `name`: `turns.jsonl` (each turn's start and end: agent, role, ids, marker,
    /// cwd), `calls.jsonl` (each Developer start), `commander.jsonl` (each Commander batch with its messages),
    /// `checks.jsonl` (each check a verifier ran), `codex-requests.jsonl` (each JSON-RPC message a codex App Server
    /// fake read); none yet: empty.
    pub fn log(&self, name: &str) -> R<Vec<Value>> {
        match std::fs::read_to_string(self.fakes().join(name)) {
            Ok(text) => Ok(text
                .split('\n')
                .filter(|l| !l.trim().is_empty())
                .map(serde_json::from_str)
                .collect::<Result<_, _>>()?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(e.into()),
        }
    }

    /// `hoh args…` in the project with the world's environment only; stdin from /dev/null.
    pub fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(HOH);
        command
            .args(args)
            .current_dir(self.project())
            .env_clear()
            .envs(self.env())
            .stdin(Stdio::null());
        command
    }

    /// [`World::command`] run to its end (killed after [`LIMIT`]): its status, stdout and stderr.
    pub fn hoh(&self, args: &[&str]) -> R<Output> {
        let child = self
            .command(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        finish(child, LIMIT)
    }

    /// `program args…` in the project with the world's environment and `TERM=xterm-256color`, the PTY its stdio and,
    /// when `ctty`, its controlling terminal (`setsid --ctty`; else a session of its own without one); returned once
    /// it leads its session and, when `ctty`, has the PTY as its controlling terminal.
    pub fn spawn_pty(
        &self,
        pty: &pty::Pty,
        program: &OsStr,
        args: &[&str],
        ctty: bool,
    ) -> R<pty::Hoh> {
        let mut command = Command::new(&self.setsid);
        if ctty {
            command.arg("--ctty");
        }
        command
            .arg(program)
            .args(args)
            .current_dir(self.project())
            .env_clear()
            .envs(self.env())
            .env("TERM", "xterm-256color")
            .stdin(pty.stdio()?)
            .stdout(pty.stdio()?)
            .stderr(pty.stdio()?);
        let child = pty::Hoh(command.spawn()?);
        // Returned once it leads its session and, with `ctty`, once the PTY is its controlling terminal: setsid runs in
        // the same process, calls setsid(2), then with --ctty takes the terminal (TIOCSCTTY) a moment later, then execs
        // the program, so a session of its own does not yet mean a terminal. A program that has already ended (a
        // zombie, which /proc shows with no terminal) had both.
        let pid = child.0.id();
        let end = Instant::now() + Duration::from_secs(10);
        loop {
            let (session, tty) = session_and_tty(pid)?;
            if session == pid && (!ctty || tty != 0 || ended(pid)?) {
                break;
            }
            if Instant::now() > end {
                return Err(if session == pid {
                    format!("{program:?} leads a session of its own but has no controlling terminal after 10 s")
                } else {
                    format!("{program:?} did not get a session of its own within 10 s")
                }
                .into());
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        Ok(child)
    }

    /// `hoh args…` as [`World::spawn_pty`] starts it, the PTY its controlling terminal.
    pub fn hoh_pty(&self, pty: &pty::Pty, args: &[&str]) -> R<pty::Hoh> {
        self.spawn_pty(pty, OsStr::new(HOH), args, true)
    }

    /// The project's store.
    pub fn db(&self) -> PathBuf {
        self.project().join(".hoh/state.db")
    }

    /// The first column of the first row `sql` gives, as text ("" for none or NULL).
    pub fn query(&self, sql: &str) -> R<String> {
        Ok(self
            .python(
                "import sqlite3,sys; r=sqlite3.connect(sys.argv[1], timeout=10).execute(sys.stdin.read()).fetchone(); \
                 print('' if r is None or r[0] is None else r[0])",
                sql,
            )?
            .trim_end_matches('\n')
            .to_owned())
    }

    /// Every row `sql` gives, as an object of its columns.
    pub fn rows(&self, sql: &str) -> R<Vec<Value>> {
        Ok(serde_json::from_str(&self.python(
            "import sqlite3,sys,json; c=sqlite3.connect(sys.argv[1], timeout=10); c.row_factory=sqlite3.Row; \
             print(json.dumps([dict(r) for r in c.execute(sys.stdin.read())]))",
            sql,
        )?)?)
    }

    /// Run `sql` (with `:p` the project's id) on the store, committed.
    pub fn exec(&self, sql: &str) -> R {
        self.python(
            "import sqlite3,sys; c=sqlite3.connect(sys.argv[1], timeout=10); \
             p=c.execute('SELECT project_id FROM projects').fetchone()[0]; \
             c.executescript(sys.stdin.read().replace(':p', repr(p))); c.commit(); c.close()",
            sql,
        )
        .map(drop)
    }

    /// Wait (at most `limit`) until `sql` gives `want`.
    pub fn wait(&self, sql: &str, want: &str, limit: Duration) -> R {
        let end = Instant::now() + limit;
        loop {
            let got = self.query(sql)?;
            if got == want {
                return Ok(());
            }
            if Instant::now() > end {
                return Err(format!("{sql}: {got:?}, not {want:?} within {limit:?}").into());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// [`World::wait`] for an async test.
    pub async fn until(&self, sql: &str, want: &str, limit: Duration) -> R {
        let end = Instant::now() + limit;
        loop {
            let got = self.query(sql)?;
            if got == want {
                return Ok(());
            }
            if Instant::now() > end {
                return Err(format!("{sql}: {got:?}, not {want:?} within {limit:?}").into());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// `script` (Python, its argv[1] the store) with `stdin`; its stdout.
    fn python(&self, script: &str, stdin: &str) -> R<String> {
        use std::io::Write as _;
        let mut child = Command::new(self.tools().join("python3"))
            .args(["-c", script])
            .arg(self.db())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        child
            .stdin
            .take()
            .ok_or("no stdin")?
            .write_all(stdin.as_bytes())?;
        let out = child.wait_with_output()?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).into_owned().into());
        }
        Ok(String::from_utf8(out.stdout)?)
    }

    /// The `hoh __host <project>` processes of the project (hosts its commands started), by their command lines.
    pub fn hosts(&self) -> R<Vec<u32>> {
        let wanted = format!("{HOH}\0__host\0{}\0", self.project().display());
        Ok(hoh_agents::identity::running_with(|c| {
            c == wanted.as_bytes()
        })?)
    }

    /// Every process of the world but this one: its command line names the world's directory, or it works in it.
    pub fn processes(&self) -> Vec<u32> {
        let me = std::process::id();
        let root = self.root().display().to_string();
        let canonical = std::fs::canonicalize(self.root()).unwrap_or_else(|_| self.root().into());
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
            .filter(|pid| *pid != me)
            .filter(|pid| {
                std::fs::read(format!("/proc/{pid}/cmdline"))
                    .is_ok_and(|c| String::from_utf8_lossy(&c).contains(&root))
                    || std::fs::read_link(format!("/proc/{pid}/cwd")).is_ok_and(|cwd| {
                        cwd.starts_with(&canonical) || cwd.starts_with(self.root())
                    })
            })
            .collect()
    }

    /// Stop what the world's commands left running: its hosts (SIGTERM, SIGKILL after 10 s), then every other
    /// process of the world (SIGKILL). An in-process host is the test's to shut down before.
    pub fn stop(&self) {
        let signal = |pid: u32, signal: rustix::process::Signal| {
            if let Some(pid) = i32::try_from(pid)
                .ok()
                .and_then(rustix::process::Pid::from_raw)
            {
                let _ = rustix::process::kill_process(pid, signal);
            }
        };
        for pid in self.hosts().unwrap_or_default() {
            signal(pid, rustix::process::Signal::TERM);
        }
        let end = Instant::now() + Duration::from_secs(10);
        while self.hosts().is_ok_and(|h| !h.is_empty()) && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(50));
        }
        for _ in 0..3 {
            let left = self.processes();
            if left.is_empty() {
                break;
            }
            for pid in left {
                signal(pid, rustix::process::Signal::KILL);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for World {
    /// Its processes stopped, its directory removed here and not only by the TempDir, whose removal ignores its errors:
    /// a removal that fails reports why ([`leftover`]) on stderr, written directly so that the test harness, which
    /// captures only the print macros, shows it for a passing test too. Nothing is tried again.
    fn drop(&mut self) {
        use std::io::Write as _;
        self.stop();
        if let Err(e) = std::fs::remove_dir_all(self.root()) {
            let _ = std::io::stderr().write_all(leftover(self.root(), &e).as_bytes());
        }
    }
}

/// Why `root` was not removed (`error`): every entry left under it with its size and its last write (seconds and
/// nanoseconds since the epoch), and every process with a file open under it (`fd <n> -> <path>`) or working there
/// (`cwd <dir>`), with its command line.
pub fn leftover(root: &Path, error: &std::io::Error) -> String {
    use std::fmt::Write as _;
    use std::os::unix::fs::MetadataExt as _;
    let mut out = format!(
        "hoh test world: {} was not removed: {error}\nleft (path, bytes, last write):\n",
        root.display()
    );
    let mut todo = vec![root.to_path_buf()];
    while let Some(dir) = todo.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if let Ok(m) = std::fs::symlink_metadata(&path) {
                let _ = writeln!(
                    out,
                    "  {} {} {}.{:09}",
                    path.display(),
                    m.len(),
                    m.mtime(),
                    m.mtime_nsec()
                );
                if m.is_dir() {
                    todo.push(path);
                }
            }
        }
    }
    out.push_str("holders:\n");
    for entry in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        let mut found = Vec::new();
        if let Ok(cwd) = std::fs::read_link(format!("/proc/{pid}/cwd"))
            && cwd.starts_with(root)
        {
            found.push(format!("cwd {}", cwd.display()));
        }
        for fd in std::fs::read_dir(format!("/proc/{pid}/fd"))
            .into_iter()
            .flatten()
            .flatten()
        {
            if let Ok(target) = std::fs::read_link(fd.path())
                && target.starts_with(root)
            {
                found.push(format!(
                    "fd {} -> {}",
                    fd.file_name().to_string_lossy(),
                    target.display()
                ));
            }
        }
        if !found.is_empty() {
            let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
            let _ = writeln!(
                out,
                "  pid {pid} [{}] {}",
                String::from_utf8_lossy(&cmdline)
                    .replace('\0', " ")
                    .trim_end(),
                found.join(", ")
            );
        }
    }
    out
}

/// `child`'s output once it ends; past `limit` it is killed and this is an error.
pub fn finish(mut child: Child, limit: Duration) -> R<Output> {
    fn drain(
        pipe: Option<impl std::io::Read + Send + 'static>,
    ) -> std::sync::mpsc::Receiver<Vec<u8>> {
        let (sent, got) = std::sync::mpsc::channel();
        if let Some(mut pipe) = pipe {
            std::thread::spawn(move || {
                let mut bytes = Vec::new();
                let _ = pipe.read_to_end(&mut bytes);
                let _ = sent.send(bytes);
            });
        } else {
            let _ = sent.send(Vec::new());
        }
        got
    }
    let (stdout, stderr) = (drain(child.stdout.take()), drain(child.stderr.take()));
    let end = Instant::now() + limit;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() > end {
            let _ = child.kill();
            let _ = child.wait();
            let stderr = stderr
                .recv_timeout(Duration::from_secs(5))
                .unwrap_or_default();
            return Err(format!(
                "still running after {limit:?}; stderr: {}",
                String::from_utf8_lossy(&stderr)
            )
            .into());
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let pipe = |got: std::sync::mpsc::Receiver<Vec<u8>>| {
        got.recv_timeout(Duration::from_secs(5))
            .map_err(|_| "a pipe stayed open after the command ended")
    };
    Ok(Output {
        status,
        stdout: pipe(stdout)?,
        stderr: pipe(stderr)?,
    })
}

/// The system time plus what the test adds: a host's clock (`HostConfig::clock`) for deadlines and retry waits.
#[derive(Debug, Default)]
pub struct FakeClock(std::sync::Mutex<Duration>);

impl FakeClock {
    pub fn advance(&self, by: Duration) {
        if let Ok(mut ahead) = self.0.lock() {
            *ahead += by;
        }
    }
}

impl hoh_core::delivery::Clock for FakeClock {
    fn now(&self) -> std::time::SystemTime {
        std::time::SystemTime::now() + self.0.lock().map(|d| *d).unwrap_or_default()
    }
}

/// A Commander turn of the scripted [`Team`], as its supervision input gives it.
#[derive(Debug, Clone)]
pub struct Turn {
    pub run_id: String,
    pub attempt_id: String,
    /// The run's state and revision and the Commander's epoch the turn was given.
    pub state: String,
    pub revision: i64,
    pub role_epoch: i64,
    /// The wake it answers (its decisions' causation) and the whole summary it was shown (its events, tasks, roles).
    pub wake: String,
    pub summary: Value,
}

/// What the scripted Commander decides on a turn: the decision payloads it submits, in order, each at the revision the
/// one before it leaves (the turn's revision, then one more for each).
pub type Script = dyn Fn(&Turn) -> Vec<Value> + Send + Sync;

/// An executor of an in-process host (verification_dispatch.rs's pattern, [`World::team_host`]) that plays the
/// Commander by a script: each supervision turn is a process of its own process group, held at its start gate, that
/// ends as soon as the gate opens, its bridge lines the script's decisions for the turn, each a valid `Message` of the
/// turn's nonce, epoch and wake. It takes no work: every other dispatch is refused at its delivery, so no work attempt
/// starts. Every turn is kept with what it submitted ([`Team::turns`]), every dispatch it refused too
/// ([`Team::refused`]); a turn still running when it is dropped is ended.
pub struct Team {
    script: Box<Script>,
    turns: std::sync::Mutex<Vec<(Turn, Vec<Value>)>>,
    refused: std::sync::Mutex<Vec<String>>,
    /// Each turn's process group (its leader's pid) and its leader's start time.
    agents: std::sync::Mutex<Vec<(u32, Option<String>)>>,
}

impl std::fmt::Debug for Team {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Team")
            .field("turns", &self.turns)
            .field("refused", &self.refused)
            .finish_non_exhaustive()
    }
}

impl Team {
    /// A Team whose Commander decides by `script`.
    pub fn new(
        script: impl Fn(&Turn) -> Vec<Value> + Send + Sync + 'static,
    ) -> std::sync::Arc<Team> {
        std::sync::Arc::new(Team {
            script: Box::new(script),
            turns: std::sync::Mutex::default(),
            refused: std::sync::Mutex::default(),
            agents: std::sync::Mutex::default(),
        })
    }

    /// Every Commander turn so far, with the messages it submitted.
    pub fn turns(&self) -> Vec<(Turn, Vec<Value>)> {
        self.turns.lock().map(|t| t.clone()).unwrap_or_default()
    }

    /// The attempt ids of the work dispatches it refused (none started).
    pub fn refused(&self) -> Vec<String> {
        self.refused.lock().map(|r| r.clone()).unwrap_or_default()
    }
}

/// The Team's turns end with it: each process group whose leader is still the one it started is SIGKILLed.
impl Drop for Team {
    fn drop(&mut self) {
        let agents = self
            .agents
            .lock()
            .map(|mut a| std::mem::take(&mut *a))
            .unwrap_or_default();
        for (pgid, start) in agents {
            let same =
                start.is_some() && hoh_agents::identity::start_id(pgid).ok().flatten() == start;
            if let (true, Some(pgid)) = (
                same,
                i32::try_from(pgid)
                    .ok()
                    .and_then(rustix::process::Pid::from_raw),
            ) {
                let _ = rustix::process::kill_process_group(pgid, rustix::process::Signal::KILL);
            }
        }
    }
}

impl hoh_app::host::Executor for Team {
    fn deliver(
        &self,
        work: &hoh_app::host::Work,
    ) -> tokio::sync::oneshot::Receiver<Result<(), String>> {
        let (answer, ack) = tokio::sync::oneshot::channel();
        if work.role == "commander" {
            let _ = answer.send(Ok(()));
        } else {
            if let Ok(mut refused) = self.refused.lock() {
                refused.push(work.attempt_id.clone());
            }
            let _ = answer.send(Err("the scripted Team takes no work".to_owned()));
        }
        ack
    }

    fn start(
        &self,
        work: &hoh_app::host::Work,
    ) -> Result<hoh_app::host::Started, hoh_app::host::Refusal> {
        use std::os::unix::process::CommandExt as _;
        let refusal = |reason: &str, detail: Option<String>| hoh_app::host::Refusal {
            reason: reason.to_owned(),
            class: hoh_core::retry::FailureClass::Transient { retry_after: None },
            detail,
            details: None,
        };
        let input = work
            .assignment
            .supervision
            .clone()
            .filter(|_| work.role == "commander")
            .ok_or_else(|| refusal("NOT_SCRIPTED", None))?;
        let turn = Turn {
            run_id: work.run_id.clone(),
            attempt_id: work.attempt_id.clone(),
            state: input.summary["run"]["state"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            revision: input.run_revision,
            role_epoch: input.role_epoch,
            wake: input.wake.clone(),
            summary: input.summary.clone(),
        };
        let spec = input.summary["run"]["spec_sha256"]
            .as_str()
            .map_or_else(|| "0".repeat(64), str::to_owned);
        let messages: Vec<Value> = (self.script)(&turn)
            .into_iter()
            .zip(0..)
            .map(|(payload, i)| {
                json!({
                    "schema_version": "1.0", "message_id": format!("d{i}"), "run_id": turn.run_id,
                    "task_id": work.task_id, "attempt_id": turn.attempt_id, "sender_role": "commander",
                    "recipient_role": "runtime", "role_epoch": turn.role_epoch, "correlation_id": "team",
                    "causation_id": turn.wake, "idempotency_key": format!("{}/d{i}", turn.attempt_id),
                    "expected_revision": turn.revision + i, "spec_sha256": spec, "candidate_id": null,
                    "kind": "decision", "payload": payload,
                })
            })
            .collect();
        let lines: Vec<String> = messages
            .iter()
            .map(|m| json!({"nonce": input.nonce, "message": m}).to_string())
            .collect();
        if let Ok(mut turns) = self.turns.lock() {
            turns.push((turn, messages));
        }
        let mut child = Command::new("sh")
            .args(["-c", "read -r go; exit 0"])
            .process_group(0)
            .env(hoh_app::host::ATTEMPT_ENV, work.marker())
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| refusal("SPAWN", Some(e.to_string())))?;
        let pid = child.id();
        if let Ok(mut agents) = self.agents.lock() {
            agents.push((pid, hoh_agents::identity::start_id(pid).ok().flatten()));
        }
        let stdin = child.stdin.take();
        let gate = hoh_app::host::Gate::new(move || {
            use std::io::Write as _;
            stdin
                .ok_or("no stdin")?
                .write_all(b"go\n")
                .map_err(|e| e.to_string())
        });
        let (end, ended) = tokio::sync::oneshot::channel();
        std::thread::spawn(move || {
            let status = child.wait();
            let until = Instant::now() + Duration::from_secs(5);
            let mut group_gone = hoh_agents::identity::group_empty(pid);
            while !group_gone && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(10));
                group_gone = hoh_agents::identity::group_empty(pid);
            }
            let _ = end.send(hoh_app::host::Outcome {
                success: status.as_ref().is_ok_and(std::process::ExitStatus::success),
                detail: "answered".to_owned(),
                group_gone,
                class: hoh_core::retry::FailureClass::Transient { retry_after: None },
                observed: hoh_core::persistence::Observed {
                    messages: lines,
                    ..Default::default()
                },
                unknown_effect: false,
            });
        });
        Ok(hoh_app::host::Started {
            pid,
            gate,
            ended,
            prompt: None,
            ran: None,
        })
    }
}

/// The scripted Commander's transition_run to `to`.
pub fn transition(to: &str) -> Value {
    json!({"kind": "transition_run", "to_state": to})
}

/// The transition_run steps along CREATED→DISCOVERING→SPECIFYING→READY→RUNNING after `from` up to `to` (none when `to`
/// is not ahead of `from` on that path).
pub fn steps(from: &str, to: &str) -> Vec<Value> {
    const ORDER: [&str; 5] = ["CREATED", "DISCOVERING", "SPECIFYING", "READY", "RUNNING"];
    let at = |state: &str| ORDER.iter().position(|s| *s == state);
    match (at(from), at(to)) {
        (Some(from), Some(to)) if from < to => {
            ORDER[from + 1..=to].iter().map(|s| transition(s)).collect()
        }
        _ => Vec::new(),
    }
}

/// The scripted Commander's request_user_decision: `question` for a person, `question_id` its own id.
pub fn ask(question_id: &str, question: &str) -> Value {
    json!({"kind": "request_user_decision", "question_id": question_id, "question": question,
        "requires_user": true})
}

/// The session id and the controlling terminal (0: none) of process `pid` (`/proc/<pid>/stat` fields 6 and 7).
pub fn session_and_tty(pid: u32) -> R<(u32, i64)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let after = stat.rsplit_once(") ").ok_or("no stat")?.1;
    let fields: Vec<&str> = after.split(' ').collect();
    Ok((
        fields.get(3).ok_or("no session")?.parse()?,
        fields.get(4).ok_or("no tty")?.parse()?,
    ))
}

/// Whether process `pid` (a child not yet reaped) has ended: its state in `/proc/<pid>/stat` is Z.
fn ended(pid: u32) -> R<bool> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let after = stat.rsplit_once(") ").ok_or("no stat")?.1;
    Ok(after.starts_with('Z'))
}

/// One HTTP exchange with an in-process host over its TCP port: the status and the JSON body.
pub async fn exchange(
    host: &Host,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> R<(u16, Value)> {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let body = body.map(Value::to_string).unwrap_or_default();
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nX-HoH-Token: {token}\r\n",
        port = host.port,
        token = host.token,
    );
    if !body.is_empty() {
        request.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            body.len()
        ));
    }
    request.push_str("\r\n");
    request.push_str(&body);
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", host.port)).await?;
    stream.write_all(request.as_bytes()).await?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await?;
    let text = String::from_utf8(raw)?;
    let (head, body) = text.split_once("\r\n\r\n").ok_or("no response head")?;
    let status: u16 = head.split(' ').nth(1).ok_or("no status")?.parse()?;
    Ok((status, serde_json::from_str(body)?))
}

/// A run started on an in-process host (StartRun, headless AUTO): its id.
pub async fn start_run(host: &Host, key: &str, goal: &str) -> R<String> {
    let body = json!({
        "schema_version": "1.0", "command_id": format!("c-{key}"), "idempotency_key": key,
        "expected_revision": 0, "kind": "StartRun", "run_id": null,
        "payload": {"goal": goal, "mode": "AUTO", "interactive": false},
    });
    let (status, accepted) = exchange(host, "POST", "/v1/commands", Some(&body)).await?;
    if status != 202 {
        return Err(format!("StartRun: {status} {accepted}").into());
    }
    Ok(accepted["run_id"].as_str().ok_or("no run_id")?.to_owned())
}

/// The directory of attempt `attempt_id`'s logs (`.hoh/attempts/<attempt id, '/' as %2F>`).
pub fn attempt_dir(project: &Path, attempt_id: &str) -> PathBuf {
    project
        .join(".hoh/attempts")
        .join(attempt_id.replace('%', "%25").replace('/', "%2F"))
}

/// The lowercase hex sha256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    <sha2::Sha256 as sha2::Digest>::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// `value` (a JSON string) as an SQL text literal.
fn sql_text(value: &Value) -> String {
    format!(
        "'{}'",
        value.as_str().unwrap_or_default().replace('\'', "''")
    )
}

/// `tool`'s path on the machine (/usr/bin, /bin, /usr/sbin, /sbin).
fn found(tool: &str) -> R<PathBuf> {
    Ok(["/usr/bin", "/bin", "/usr/sbin", "/sbin"]
        .iter()
        .map(|d| Path::new(d).join(tool))
        .find(|p| p.exists())
        .ok_or(format!("no {tool} on this machine"))?)
}
