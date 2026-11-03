import contextlib,io,json,os,shutil,subprocess,sys,tempfile,unittest
from pathlib import Path
from unittest.mock import patch
from helpers import ROOT,load_json
from jsonschema import Draft202012Validator
from validate_bundle import validate
import taskboard

class BundleContracts(unittest.TestCase):
    def test_whole_bundle(self):
        r=validate(ROOT);self.assertEqual(r['status'],'PASS',r['errors']);self.assertFalse(r['product_accepted'])
    def test_protocol_examples(self):
        v=Draft202012Validator(load_json(ROOT/'contracts/protocol.schema.json'))
        for file in ['context-handoff.json','hcp-message.json','command.json']:
            with self.subTest(file=file):v.validate(load_json(ROOT/'examples'/file))
    def test_protocol_rejects_extra_field(self):
        v=Draft202012Validator(load_json(ROOT/'contracts/protocol.schema.json'));e=load_json(ROOT/'examples/command.json');e['invented_field']=1
        self.assertFalse(v.is_valid(e))
    def test_total_release_scope(self):
        r=load_json(ROOT/'contracts/requirements.json')['requirements'];self.assertEqual(len(r),70);self.assertTrue(all(x['required_release'] for x in r))
    def test_live_auth_does_not_block_offline_core(self):
        tasks={t['id']:t for t in load_json(ROOT/'contracts/development-tasks.json')['tasks']}
        def ancestors(i):
            seen=set();stack=list(tasks[i]['depends_on'])
            while stack:
                t=stack.pop()
                if t not in seen:seen.add(t);stack.extend(tasks[t]['depends_on'])
            return seen
        for t in tasks:
            if t.startswith(('M1-','M2-')):self.assertNotIn('M0-T04',ancestors(t),t)
        plugin=next(t for t in tasks.values() if 'REQ-048' in t['requirements'] and t['kind']=='implement')
        self.assertIn('M0-T04',plugin['depends_on'])
    def test_release_classes_and_interim_scope(self):
        r=load_json(ROOT/'contracts/requirements.json')['requirements']
        self.assertEqual({x['id'] for x in r if x['release_class']=='degradable'},{'REQ-048','REQ-049','REQ-050'})
        self.assertTrue(all('v1' in x['release_targets'] for x in r))
        self.assertTrue(all(x['spec_maturity']=='detailed' for x in r if x['milestone'] in ('M1','M2')))
    def test_unimplemented_checks_fail(self):
        plan=load_json(ROOT/'tests/check-plan.json')
        for cid,entry in plan['checks'].items():
            if entry['argv'][-3:-2]==['scripts/product/not-implemented.sh'] or 'not-implemented.sh' in ' '.join(entry['argv']):
                with self.subTest(check=cid):
                    p=subprocess.run(entry['argv'],cwd=ROOT,capture_output=True,text=True,timeout=30)
                    self.assertEqual(p.returncode,3);self.assertIn('NOT_IMPLEMENTED',p.stderr)
    def test_trivial_plan_command_rejected(self):
        import validate_bundle
        with tempfile.TemporaryDirectory() as d:
            root=Path(d)/'kit';shutil.copytree(ROOT,root,ignore=shutil.ignore_patterns('.git','.hoh-dev','__pycache__','target','npm'))
            plan=load_json(root/'tests/check-plan.json');plan['checks']['recovery']['argv']=['true']
            (root/'tests/check-plan.json').write_text(json.dumps(plan))
            r=validate_bundle.validate(root);self.assertTrue(any('always-succeeding' in e for e in r['errors']),r['errors'])
    def test_original_skills_available(self):
        skills=load_json(ROOT/'resources/skills/registry.json')['skills'];self.assertEqual(len(skills),12)
        for s in skills:self.assertTrue((ROOT/s['entry']).is_file())
    def test_bootstrap_agent_entries(self):
        for d in ['.agents','.claude','.pi']:self.assertTrue((ROOT/d/'skills/hoh-bootstrap/SKILL.md').exists())
    def test_product_check_does_not_accept_empty_product(self):
        # Valid only while this is a design kit. A completed implementation has its own tests.
        if (ROOT/'Cargo.toml').exists():return
        p=subprocess.run(['bash',str(ROOT/'scripts/check.sh'),'--product'],capture_output=True,text=True)
        self.assertEqual(p.returncode,3);self.assertIn('NOT_IMPLEMENTED',p.stderr)
    def test_missing_manifest_fails_closed(self):
        p=subprocess.run([sys.executable,str(ROOT/'tools/release_gate.py'),'--manifest',str(ROOT/'never-existed.json')],capture_output=True,text=True)
        self.assertEqual(p.returncode,1);self.assertEqual(json.loads(p.stderr)['status'],'FAIL')

class ValidatorScope(unittest.TestCase):
    def test_product_files_are_not_kit_files(self):
        with tempfile.TemporaryDirectory() as d:
            root=Path(d)/'kit';shutil.copytree(ROOT,root,ignore=shutil.ignore_patterns('.git','.hoh-dev','__pycache__','target','npm'))
            (root/'web').mkdir();(root/'web/tsconfig.json').write_text('{ // comments are valid here\n}\n')
            (root/'crates/core').mkdir(parents=True);(root/'crates/core/README.md').write_text('```rust\n/// see [Foo](crate::Foo)\n```\n')
            (root/'tests/fixtures').mkdir(parents=True,exist_ok=True);(root/'tests/fixtures/broken.json').write_text('{not json')
            r=validate(root);self.assertEqual(r['status'],'PASS',r['errors'])

class SetupAndLedger(unittest.TestCase):
    def setUp(self):
        self.t=tempfile.TemporaryDirectory(prefix='hoh 包 path ');self.addCleanup(self.t.cleanup);self.root=Path(self.t.name)/'project';self.root.mkdir()
        for d in ['scripts','tools','config','contracts','examples']:shutil.copytree(ROOT/d,self.root/d,ignore=shutil.ignore_patterns('__pycache__'))
        for f in ['LAUNCH_PROMPT.md','requirements-dev.txt']:shutil.copy(ROOT/f,self.root/f)
    def shell(self,*args,env=None):
        e={k:v for k,v in os.environ.items() if k!='HOH_BOOTSTRAP_PARENT'}|(env or {})
        return subprocess.run(['bash',str(self.root/'scripts'/args[0]),*args[1:]],capture_output=True,text=True,timeout=30,env=e)
    def test_bootstrap_requires_yolo_ack(self):self.assertEqual(self.shell('bootstrap.sh','--dry-run').returncode,64)
    def test_bootstrap_dry_no_writes(self):
        before=set(self.root.rglob('*'));p=self.shell('bootstrap.sh','--yolo','--dry-run');self.assertEqual(p.returncode,0,p.stderr);self.assertEqual(set(self.root.rglob('*')),before)
    def test_unknown_bootstrap_option(self):self.assertEqual(self.shell('bootstrap.sh','--yolo','--made-up').returncode,64)
    def test_offline_setup_idempotent(self):
        p=self.shell('bootstrap.sh','--yolo','--skip-install');self.assertIn(p.returncode,[0,3],p.stderr)
        prog=self.root/'.hoh-dev/PROGRESS.md';self.assertTrue(prog.exists());prog.write_text('KEEP_EXISTING_PROGRESS')
        p=self.shell('bootstrap.sh','--yolo','--skip-install');self.assertIn(p.returncode,[0,3],p.stderr);self.assertEqual(prog.read_text(),'KEEP_EXISTING_PROGRESS')
        self.assertTrue((self.root/'.hoh-dev/env.sh').is_file())
    def test_start_interactive_dry(self):
        p=self.shell('start.sh','codex','--dry-run');self.assertEqual(p.returncode,0,p.stderr);self.assertIn('--dangerously-bypass',p.stdout);self.assertIn('HOH_TOP_LEVEL_BOOTSTRAP',p.stdout)
    def test_start_defaults_to_main_developer(self):
        p=self.shell('start.sh','--headless','--dry-run');self.assertEqual(p.returncode,0,p.stderr);self.assertEqual(json.loads(p.stdout)['argv'][0],'claude')
    def test_start_refuses_nested_top_level(self):
        p=self.shell('start.sh','--dry-run',env={'HOH_BOOTSTRAP_PARENT':'outer'});self.assertEqual(p.returncode,78)
    def test_start_headless_dry(self):
        p=self.shell('start.sh','pi','--headless','--dry-run');self.assertEqual(p.returncode,0,p.stderr);self.assertTrue(json.loads(p.stdout)['dry_run'])
    def test_plugin_plan_is_dry(self):
        p=subprocess.run([sys.executable,str(self.root/'tools/install_pi_plugins.py'),'--dry-run'],capture_output=True,text=True);self.assertEqual(p.returncode,0,p.stderr);self.assertIn('@juicesharp/rpiv-todo',p.stdout);self.assertFalse((self.root/'.hoh-dev').exists())
    def ledger(self,*args):
        with patch.object(taskboard,'ROOT',self.root),contextlib.redirect_stdout(io.StringIO()),contextlib.redirect_stderr(io.StringIO()):return taskboard.main(list(args))
    def refused(self,*args):
        with self.assertRaises(SystemExit):self.ledger(*args)
    def seed(self,done):
        (self.root/'.hoh-dev').mkdir(exist_ok=True)
        (self.root/'.hoh-dev/task-state.json').write_text(json.dumps({'tasks':{t:{'task_id':t,'status':'done','owner':'seed','note':'TEST seed'} for t in done},'history':[]}))
    def state(self,t):return load_json(self.root/'.hoh-dev/task-state.json')['tasks'][t]
    def test_ledger_dependency_reject(self):self.refused('claim','M0-T02','--owner','dev')
    def test_ledger_running_only_via_claim(self):self.refused('set','M0-T01','running')
    def test_ledger_claim_needs_owner(self):self.refused('claim','M0-T01')
    def test_ledger_done_needs_note(self):
        self.ledger('claim','M0-T01','--owner','dev');self.refused('set','M0-T01','done','--owner','dev')
    def test_ledger_only_owner_finishes(self):
        self.ledger('claim','M0-T01','--owner','dev');self.refused('set','M0-T01','done','--owner','someone-else','--note','x')
    def test_ledger_illegal_transition(self):self.refused('set','M0-T01','done','--note','skipped running')
    def test_ledger_progress_and_dependencies(self):
        self.assertEqual(self.ledger('claim','M0-T01','--owner','dev'),0)
        self.assertEqual(self.ledger('set','M0-T01','done','--owner','dev','--note','TEST fixture only'),0)
        self.assertEqual(self.ledger('claim','M0-T02','--owner','dev'),0)
        state=load_json(self.root/'.hoh-dev/task-state.json');self.assertEqual(len(state['history']),3);self.assertEqual(self.state('M0-T02')['owner'],'dev')
    def test_ledger_single_writer(self):
        self.seed(['M0-T01','M0-T02','M0-T03','M1-T01'])
        self.assertEqual(self.ledger('claim','M1-T02','--owner','a'),0)
        self.refused('claim','M2-T01','--owner','b')
        self.assertEqual(self.ledger('claim','M0-T04','--owner','c'),0)  # non-writer research runs in parallel
    def test_ledger_outline_requirements_not_ready(self):
        tasks=load_json(self.root/'contracts/development-tasks.json')['tasks']
        self.seed([t['id'] for t in tasks if t['milestone'] in ('M0','M1','M2','V01')]+['M3-T01'])
        req={r['id']:r for r in load_json(self.root/'contracts/requirements.json')['requirements']}
        target=next(t for t in tasks if t['milestone']=='M3' and t['kind']=='implement')
        if all(req[r]['spec_maturity']=='detailed' for r in target['requirements']):self.skipTest('M3 already detailed')
        self.refused('claim',target['id'],'--owner','dev')
    def test_ledger_reopen_blocks_started_dependents(self):
        self.seed(['M0-T01','M0-T02'])
        self.assertEqual(self.ledger('claim','M0-T03','--owner','dev'),0)
        self.assertEqual(self.ledger('set','M0-T01','pending','--note','preflight was wrong'),0)
        self.assertEqual(self.state('M0-T02')['status'],'blocked');self.assertEqual(self.state('M0-T03')['status'],'blocked')
