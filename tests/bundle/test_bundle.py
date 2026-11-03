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
        self.assertEqual(tasks['M1-T01']['depends_on'],['M0-T02'])
        self.assertIn('M5-T01',tasks['M7-T01']['depends_on'])
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

class SetupAndLedger(unittest.TestCase):
    def setUp(self):
        self.t=tempfile.TemporaryDirectory(prefix='hoh 包 path ');self.addCleanup(self.t.cleanup);self.root=Path(self.t.name)/'project';self.root.mkdir()
        for d in ['scripts','tools','config','contracts','examples']:shutil.copytree(ROOT/d,self.root/d,ignore=shutil.ignore_patterns('__pycache__'))
        for f in ['LAUNCH_PROMPT.md','requirements-dev.txt']:shutil.copy(ROOT/f,self.root/f)
    def shell(self,*args):return subprocess.run(['bash',str(self.root/'scripts'/args[0]),*args[1:]],capture_output=True,text=True,timeout=30)
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
        p=self.shell('start.sh','codex','--dry-run');self.assertEqual(p.returncode,0,p.stderr);self.assertIn('--dangerously-bypass',p.stdout)
    def test_start_headless_dry(self):
        p=self.shell('start.sh','pi','--headless','--dry-run');self.assertEqual(p.returncode,0,p.stderr);self.assertTrue(json.loads(p.stdout)['dry_run'])
    def test_plugin_plan_is_dry(self):
        p=subprocess.run([sys.executable,str(self.root/'tools/install_pi_plugins.py'),'--dry-run'],capture_output=True,text=True);self.assertEqual(p.returncode,0,p.stderr);self.assertIn('@juicesharp/rpiv-todo',p.stdout);self.assertFalse((self.root/'.hoh-dev').exists())
    def ledger(self,*args):
        with patch.object(taskboard,'ROOT',self.root),contextlib.redirect_stdout(io.StringIO()),contextlib.redirect_stderr(io.StringIO()):return taskboard.main(list(args))
    def test_ledger_dependency_reject(self):
        with self.assertRaises(SystemExit):self.ledger('set','M0-T02','running')
    def test_ledger_done_needs_note(self):
        with self.assertRaises(SystemExit):self.ledger('set','M0-T01','done')
    def test_ledger_progress_and_dependencies(self):
        self.assertEqual(self.ledger('set','M0-T01','done','--note','TEST fixture only'),0)
        self.assertEqual(self.ledger('set','M0-T02','running'),0)
        state=load_json(self.root/'.hoh-dev/task-state.json');self.assertEqual(len(state['history']),2)
