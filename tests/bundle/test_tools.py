import contextlib,io,json,os,shutil,signal,subprocess,sys,tempfile,time,unittest
from pathlib import Path
from unittest.mock import patch
from helpers import ROOT,init_repository,load_json,json_file
from common import Terminated,execute,install_termination_handlers,ref_for,restore_handlers,safe_path,terminate_group
from candidate import verify,freeze
from agent_task import command_for,inspect_output
from common import flag_supported
from render_prompt import render
import run_check

FAKE=r"""#!/usr/bin/env python3
import json,os,sys,time
name=os.path.basename(sys.argv[0]);args=sys.argv[1:]
if '--help' in args:
 print('--json --dangerously-bypass-approvals-and-sandbox --skip-git-repo-check --output-format --verbose --dangerously-skip-permissions --mode --session-dir --approve --model');sys.exit(0)
if '--version' in args:print('MOCK_AGENT_VERSION');sys.exit(0)
data=sys.stdin.buffer.read()
if os.environ.get('FAKE_TRACE'):
 open(os.environ['FAKE_TRACE'],'w').write(json.dumps({'argv':args,'stdin':data.decode(),'env':sorted(k for k in os.environ if k.startswith(('CLAUDE','CODEX','HOH_','ANTHROPIC','AI_'))),'pi_offline':os.environ.get('PI_OFFLINE')}))
mode=os.getenv('FAKE_MODE','ok')
print('mock diagnostic',file=sys.stderr)
if mode=='exit-error':sys.exit(7)
if mode=='malformed':print('NOT JSON');sys.exit(0)
if mode=='timeout':time.sleep(60)
if mode=='no-terminal':print(json.dumps({'type':'message','text':'done'}));sys.exit(0)
if name=='codex':
 print(json.dumps({'type':'thread.started','thread_id':'mock-thread'}))
 print(json.dumps({'type':'turn.failed' if mode=='semantic-error' else 'turn.completed','usage':{}}))
elif name=='claude':print(json.dumps({'type':'result','subtype':'success','is_error':mode=='semantic-error','session_id':'mock-claude'}))
else:
 print(json.dumps({'type':'message_end','message':{'content':'Unicode \u2028 record not separator','stopReason':'error' if mode=='semantic-error' else 'stop'}},ensure_ascii=False))
 print(json.dumps({'type':'agent_end'}))
"""

def pi_project(root:Path,installed:str|None,locked:str|None,version:str='1.0.0',source:str|None=None)->None:
    """A project whose .pi/settings.json lists pi-demo, optionally installed under .pi/npm and recorded in the plugin lock."""
    (root/'.pi').mkdir(exist_ok=True);(root/'.pi/settings.json').write_text(json.dumps({'packages':[source or 'npm:pi-demo@1.0.0']}))
    if installed is not None:
        (root/'.pi/npm/node_modules/pi-demo').mkdir(parents=True,exist_ok=True)
        (root/'.pi/npm/package-lock.json').write_text(json.dumps({'packages':{'node_modules/pi-demo':{'version':version,'integrity':installed}}}))
    if locked is not None:
        (root/'.hoh-dev').mkdir(exist_ok=True)
        (root/'.hoh-dev/plugin-lock.json').write_text(json.dumps({'plugins':{'demo':{'package':'pi-demo','version':'1.0.0','integrity':locked,'state':'INSTALLED'}}}))

class PiPackages(unittest.TestCase):
    """M0R3-05: project packages Pi would load must be the installations the plugin lock verified."""
    def setUp(self):
        self.t=tempfile.TemporaryDirectory();self.addCleanup(self.t.cleanup);self.root=Path(self.t.name)
    def verify(self):
        from install_pi_plugins import verify_installed
        return verify_installed(self.root)
    def test_no_project_settings(self):self.assertEqual(self.verify(),{'verified':[],'absent':[],'problems':[]})
    def test_verified_installation(self):
        pi_project(self.root,installed='sha512-PINNED',locked='sha512-PINNED');self.assertEqual(self.verify(),{'verified':['pi-demo'],'absent':[],'problems':[]})
    def test_absent_package_is_not_a_problem(self):
        # With PI_OFFLINE=1 Pi skips a missing package instead of installing it; plugins are degradable.
        pi_project(self.root,installed=None,locked=None);self.assertEqual(self.verify(),{'verified':[],'absent':['pi-demo'],'problems':[]})
    def test_problems(self):
        for kwargs,needle in [({'installed':'sha512-TAMPERED','locked':'sha512-PINNED'},'integrity'),({'installed':'sha512-PINNED','locked':None},'plugin lock'),
                              ({'installed':'sha512-PINNED','locked':'sha512-PINNED','version':'2.0.0'},'version'),
                              ({'installed':None,'locked':None,'source':'git:github.com/x/pi-demo'},'not an npm')]:
            with self.subTest(**kwargs):
                shutil.rmtree(self.root/'.pi',ignore_errors=True);shutil.rmtree(self.root/'.hoh-dev',ignore_errors=True)
                pi_project(self.root,**kwargs);r=self.verify()
                self.assertEqual(r['verified'],[]);self.assertTrue(any(needle in p for p in r['problems']),r)
    def test_repository_packages_verified(self):
        # The checked-in .pi/settings.json against this VM's installation (skipped where plugins are not installed).
        from install_pi_plugins import verify_installed
        if not (ROOT/'.pi/npm/package-lock.json').is_file():self.skipTest('Pi packages not installed here')
        self.assertEqual(verify_installed(ROOT)['problems'],[])

class NativeBridge(unittest.TestCase):
    def setUp(self):
        self.t=tempfile.TemporaryDirectory(prefix='hoh mock 中文 ');self.addCleanup(self.t.cleanup);self.root=Path(self.t.name)
        self.bin=self.root/'bin';self.bin.mkdir()
        for name in ['codex','claude','pi']:
            p=self.bin/name;p.write_text(FAKE);p.chmod(0o755)
        self.prompt=self.root/'prompt with spaces.md';self.prompt.write_text('真实Unicode input，不是产品验收。')
        self.env=os.environ|{'PATH':str(self.bin)+os.pathsep+os.environ.get('PATH',''),'FAKE_TRACE':str(self.root/'trace.json'),'PYTHONDONTWRITEBYTECODE':'1'}
    def bridge(self,agent='codex',mode='ok',timeout='10'):
        env=self.env|{'FAKE_MODE':mode};out=self.root/'attempt'
        p=subprocess.run([sys.executable,str(ROOT/'tools/agent_task.py'),'--agent',agent,'--prompt-file',str(self.prompt),'--cwd',str(self.root),'--output-dir',str(out),'--timeout',timeout],capture_output=True,text=True,env=env,timeout=20)
        return p,out
    def test_codex_real_subprocess_mock(self):
        p,out=self.bridge();self.assertEqual(p.returncode,0,p.stderr);r=load_json(out/'attempt.json');self.assertFalse(r['product_accepted']);self.assertEqual(r['status'],'NATIVE_TURN_COMPLETED');self.assertIn('mock diagnostic',(out/'stderr.log').read_text())
    def test_stdin_unicode_and_spaces(self):
        p,_=self.bridge();self.assertEqual(p.returncode,0,p.stderr);t=load_json(self.root/'trace.json');self.assertTrue(t['stdin'].startswith('[HOH_DELEGATED_TASK]'));self.assertTrue(t['stdin'].endswith(self.prompt.read_text()));self.assertEqual(t['argv'][-1],'-')
    def test_pi_json_one_shot(self):
        p,out=self.bridge(agent='pi');self.assertEqual(p.returncode,0,p.stderr);self.assertIn('--approve',load_json(out/'attempt.json')['argv'])
        # M0R3-05: Pi starts with PI_OFFLINE=1, so it never installs configured packages outside the integrity guard.
        self.assertEqual(load_json(self.root/'trace.json')['pi_offline'],'1');self.assertEqual(load_json(out/'attempt.json')['pi_packages']['problems'],[])
    def test_pi_refuses_unverified_project_package(self):
        pi_project(self.root,installed='sha512-TAMPERED',locked='sha512-PINNED')
        p,out=self.bridge(agent='pi');self.assertEqual(p.returncode,78,p.stderr);self.assertIn('pi-demo',p.stderr);self.assertFalse(out.exists())
        self.assertFalse((self.root/'trace.json').exists())
    def test_native_nonzero(self):
        p,out=self.bridge(mode='exit-error');self.assertEqual(p.returncode,7);self.assertEqual(load_json(out/'attempt.json')['status'],'NATIVE_FAILED')
    def test_semantic_failure_zero_exit(self):p,_=self.bridge(mode='semantic-error');self.assertNotEqual(p.returncode,0)
    def test_missing_terminal(self):p,_=self.bridge(mode='no-terminal');self.assertNotEqual(p.returncode,0)
    def test_malformed_json(self):p,_=self.bridge(mode='malformed');self.assertNotEqual(p.returncode,0)
    def test_timeout_record(self):
        p,out=self.bridge(mode='timeout',timeout='0.15');self.assertEqual(p.returncode,124);self.assertTrue(load_json(out/'attempt.json')['timed_out'])
    def test_model_no_shell_interpolation(self):
        model='name; touch /tmp/SHOULD_NOT_RUN';c=command_for('codex',self.root,model);self.assertIn(model,c);self.assertEqual(c[-1],'-')
    def test_unsupported_native_flags(self):
        with self.assertRaisesRegex(ValueError,'unsupported'):command_for('codex',self.root,None,'--json')
    def test_claude_command(self):
        c=command_for('claude',self.root,None);self.assertIn('-p',c);self.assertIn('--verbose',c);self.assertIn('stream-json',c)
    def test_top_level_restricted_to_coordinator_roles(self):
        p=subprocess.run([sys.executable,str(ROOT/'tools/agent_task.py'),'--agent','codex','--prompt-file',str(self.prompt),'--role','qa','--top-level','--dry-run'],capture_output=True,text=True,env=self.env)
        self.assertEqual(p.returncode,2)
    def test_nested_top_level_refused(self):
        p=subprocess.run([sys.executable,str(ROOT/'tools/agent_task.py'),'--agent','claude','--prompt-file',str(self.prompt),'--role','developer','--top-level','--dry-run'],capture_output=True,text=True,env=self.env|{'HOH_BOOTSTRAP_PARENT':'outer'})
        self.assertEqual(p.returncode,2);self.assertIn('already inside bootstrap session',p.stderr)
    def test_parent_session_variables_not_inherited(self):
        parent={'CLAUDECODE':'1','CLAUDE_CODE_SESSION_ID':'parent','CLAUDE_CODE_MESSAGING_TOKEN':'secret','CLAUDE_CODE_MESSAGING_SOCKET':'/tmp/s',
                'CLAUDE_CODE_ENTRYPOINT':'cli','CODEX_SANDBOX':'seatbelt','AI_AGENT':'claude-code_x','CLAUDE_CODE_USE_BEDROCK':'1','ANTHROPIC_MODEL':'m',
                'CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS':'1'}
        self.env=self.env|parent;p,out=self.bridge(agent='claude');self.assertEqual(p.returncode,0,p.stderr)
        seen=set(load_json(self.root/'trace.json')['env'])
        removed={'CLAUDECODE','CLAUDE_CODE_SESSION_ID','CLAUDE_CODE_MESSAGING_TOKEN','CLAUDE_CODE_MESSAGING_SOCKET','CLAUDE_CODE_ENTRYPOINT','CODEX_SANDBOX'}
        self.assertFalse(seen&(removed|{'AI_AGENT'}))
        self.assertTrue({'CLAUDE_CODE_USE_BEDROCK','ANTHROPIC_MODEL','CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS','HOH_BOOTSTRAP_PARENT'}<=seen)  # config and our marker stay
        self.assertTrue(removed|{'AI_AGENT'}<=set(load_json(out/'attempt.json')['env_removed']))
    def test_child_sees_parent_marker(self):
        p,out=self.bridge();self.assertEqual(p.returncode,0,p.stderr)
        self.assertEqual(load_json(out/'attempt.json')['cli_version'],'MOCK_AGENT_VERSION')
    def test_existing_attempt_dir_refused(self):
        (self.root/'attempt').mkdir();p,_=self.bridge();self.assertEqual(p.returncode,64)
    def test_attempt_lives_in_check_evidence_dir(self):
        ev=self.root/'evidence';ev.mkdir()
        p=subprocess.run([sys.executable,str(ROOT/'tools/agent_task.py'),'--agent','claude','--role','qa','--prompt-file',str(self.prompt),'--cwd',str(self.root),'--timeout','10'],
                         capture_output=True,text=True,env=self.env|{'HOH_EVIDENCE_DIR':str(ev),'HOH_CHECK_ID':'independent-qa','HOH_CANDIDATE_ID':'CAND-'+'a'*64},timeout=20)
        self.assertEqual(p.returncode,0,p.stderr);[rec]=list((ev/'attempts').glob('*/attempt.json'));r=load_json(rec)
        self.assertEqual((r['candidate_id'],r['check_id'],r['role']),('CAND-'+'a'*64,'independent-qa','qa'));self.assertEqual(r['stdout_sha256'],ref_for(rec.parent/'stdout.jsonl',rec.parent)['sha256'])
    def test_live_sessions_recorded_under_run_check(self):
        ev=self.root/'evidence';ev.mkdir()
        p=subprocess.run([sys.executable,str(ROOT/'tools/agent_task.py'),'--agent','claude','--prompt-file',str(self.prompt),'--cwd',str(self.root),'--output-dir',str(self.root/'a2'),'--timeout','10'],
                         capture_output=True,text=True,env=self.env|{'HOH_EVIDENCE_DIR':str(ev),'HOH_CHECK_ID':'adapter-live','HOH_CANDIDATE_ID':'CAND-'+'a'*64},timeout=20)
        self.assertEqual(p.returncode,0,p.stderr);s=load_json(ev/'live-sessions.json')
        self.assertEqual([x['session_id'] for x in s['sessions']],['mock-claude']);self.assertEqual(s['sessions'][0]['cli_version'],'MOCK_AGENT_VERSION')
    def test_whole_token_flag_detection(self):
        self.assertFalse(flag_supported('--model NAME','--mode'));self.assertTrue(flag_supported('  --mode <m>','--mode'))
        with self.assertRaisesRegex(ValueError,'--mode'):command_for('pi',self.root,None,'--model --session-dir')
    def test_assistant_text_not_echoed_user_prompt(self):
        p=self.root/'raw';p.write_text('{"type":"message_end","message":{"role":"user","content":[{"type":"text","text":"HOH_NATIVE_SMOKE_OK"}]}}\n')
        self.assertEqual(inspect_output('pi',p)['assistant_text_tail'],'')
    def test_assistant_text_extraction(self):
        p=self.root/'raw';p.write_text('{"type":"item.completed","item":{"type":"agent_message","text":"HOH_NATIVE_SMOKE_OK"}}\n')
        self.assertEqual(inspect_output('codex',p)['assistant_text_tail'],'HOH_NATIVE_SMOKE_OK')
    def test_claude_failure_output(self):
        p=self.root/'raw';p.write_text('{"type":"result","is_error":true,"subtype":"error_max_turns"}\n');self.assertTrue(inspect_output('claude',p)['native_error_observed'])
    def raw(self,*records):
        p=self.root/'raw';p.write_text(''.join(json.dumps(r)+'\n' for r in records));return p
    def test_pi_session_header_is_the_session_id(self):
        # Pi 1.5.0 JSON mode: first line is the session header {"type":"session","id":...} (print-mode.js, session-manager.js).
        o=inspect_output('pi',self.raw({'type':'session','version':3,'id':'pi-session-1','cwd':'/x'},
                                       {'type':'message_end','message':{'role':'assistant','content':[],'stopReason':'stop'}},{'type':'agent_end'}))
        self.assertEqual(o['session_ids'],['pi-session-1']);self.assertFalse(o['native_error_observed'])
    def test_codex_transient_error_then_completed(self):
        o=inspect_output('codex',self.raw({'type':'thread.started','thread_id':'t'},{'type':'error','message':'Reconnecting... 1/5'},{'type':'turn.completed','usage':{}}))
        self.assertTrue(o['terminal_observed']);self.assertFalse(o['native_error_observed']);self.assertEqual(o['transient_errors'],1)
    def test_codex_errors_without_completion_fail(self):
        o=inspect_output('codex',self.raw({'type':'error','message':'Reconnecting... 5/5'},{'type':'turn.failed','error':{'message':'401'}}))
        self.assertTrue(o['native_error_observed'])
    def test_pi_auto_retry_recovered(self):
        o=inspect_output('pi',self.raw({'type':'message_end','message':{'role':'assistant','content':[],'stopReason':'error'}},
                                       {'type':'auto_retry_start','attempt':1},{'type':'auto_retry_end','success':True,'attempt':1},
                                       {'type':'message_end','message':{'role':'assistant','content':[{'type':'text','text':'ok'}],'stopReason':'stop'}},{'type':'agent_end'}))
        self.assertFalse(o['native_error_observed']);self.assertEqual(o['transient_errors'],1)
    def test_pi_auto_retry_exhausted_fails(self):
        o=inspect_output('pi',self.raw({'type':'auto_retry_start','attempt':3},{'type':'auto_retry_end','success':False,'attempt':3,'finalError':'x'},
                                       {'type':'message_end','message':{'role':'assistant','content':[],'stopReason':'error'}},{'type':'agent_end'}))
        self.assertTrue(o['native_error_observed'])
    def test_final_text_is_complete_last_message(self):
        long='x'*20000+'END'
        o=inspect_output('claude',self.raw({'type':'result','subtype':'success','result':long,'session_id':'s'}))
        self.assertEqual(o['_final_text'],long)
    def test_oversized_json_frame(self):
        p=self.root/'raw';p.write_bytes(b'{"text":"'+b'x'*(1024*1024)+b'"}\n');self.assertEqual(inspect_output('pi',p)['malformed_records'],1)
    def test_dry_run_no_attempt_dir(self):
        p=subprocess.run([sys.executable,str(ROOT/'tools/agent_task.py'),'--agent','codex','--prompt-file',str(self.prompt),'--cwd',str(self.root),'--output-dir',str(self.root/'unused'),'--dry-run'],capture_output=True,text=True,env=self.env)
        self.assertEqual(p.returncode,0,p.stderr);self.assertFalse((self.root/'unused').exists())

REPORT_BLOCK='Review done.\n```hoh-report\n'+json.dumps({'role':'reviewer','status':'FAIL','scope':['AC-001-01'],'findings':[{'id':'F1','severity':'high','summary':'AC-001-01 FAIL: x'}]})+'\n```\n'

class IndependentReport(unittest.TestCase):
    """--report-out: identity and hashes come from the bridge after the native process exited."""
    def setUp(self):
        self.t=tempfile.TemporaryDirectory(prefix='hoh-report-');self.addCleanup(self.t.cleanup);self.root=Path(self.t.name)
        (self.root/'contracts').mkdir();shutil.copy(ROOT/'contracts/independent-report.schema.json',self.root/'contracts')
        self.att=self.root/'attempt';self.att.mkdir();(self.att/'stdout.jsonl').write_text('{}\n');(self.att/'attempt.json').write_text('{}\n')
        self.env={'HOH_CANDIDATE_ID':'CAND-'+'a'*64,'HOH_SPEC_SHA256':'b'*64}
    def build(self,text,sessions=('s-rev',),role='reviewer',env=None):
        import agent_task
        with patch.object(agent_task,'ROOT',self.root),patch.dict(os.environ,self.env if env is None else env):
            return agent_task.build_report(role,text,list(sessions),self.att/'stdout.jsonl',self.att/'attempt.json')
    def test_report_filled_by_bridge(self):
        r=self.build(REPORT_BLOCK)
        self.assertEqual((r['session_id'],r['candidate_id'],r['spec_sha256'],r['status']),('s-rev','CAND-'+'a'*64,'b'*64,'FAIL'))
        self.assertEqual(r['raw_session'],{'path':'attempt/stdout.jsonl','sha256':ref_for(self.att/'stdout.jsonl',self.root)['sha256']})
        self.assertEqual(r['native_attempt']['path'],'attempt/attempt.json')
    def test_last_block_wins(self):
        first=REPORT_BLOCK.replace('"FAIL"','"PASS"',1)
        self.assertEqual(self.build(first+REPORT_BLOCK)['status'],'FAIL')
    def test_missing_block(self):
        with self.assertRaisesRegex(ValueError,'no ```hoh-report'):self.build('All good, PASS.')
    def test_agent_cannot_supply_identity_or_hashes(self):
        forged=REPORT_BLOCK.replace('"role"','"session_id": "dev", "role"',1)
        with self.assertRaisesRegex(ValueError,'may only contain'):self.build(forged)
    def test_role_mismatch(self):
        with self.assertRaisesRegex(ValueError,'differs from --role'):self.build(REPORT_BLOCK,role='qa')
    def test_needs_exactly_one_session(self):
        with self.assertRaisesRegex(ValueError,'exactly one'):self.build(REPORT_BLOCK,sessions=('a','b'))
    def test_needs_candidate_binding(self):
        with self.assertRaisesRegex(ValueError,'HOH_SPEC_SHA256'):self.build(REPORT_BLOCK,env={'HOH_CANDIDATE_ID':'CAND-'+'a'*64,'HOH_SPEC_SHA256':''})
    def test_schema_enforced(self):
        bad=REPORT_BLOCK.replace('"high"','"blocker"',1)
        with self.assertRaisesRegex(ValueError,'invalid'):self.build(bad)
    def test_report_out_requires_reviewer_or_qa(self):
        p=self.root/'p.md';p.write_text('x')
        r=subprocess.run([sys.executable,str(ROOT/'tools/agent_task.py'),'--agent','claude','--role','developer','--prompt-file',str(p),'--report-out',str(self.root/'r.json'),'--dry-run'],capture_output=True,text=True)
        self.assertEqual(r.returncode,2);self.assertIn('qa or reviewer',r.stderr)

class TestResultsConverter(unittest.TestCase):
    """scripts/product/test_results.py maps libtest lines to TEST ids (CR-003 per-test evidence)."""
    def test_mapping_and_statuses(self):
        with tempfile.TemporaryDirectory() as d:
            log=Path(d)/'log';out=Path(d)/'out.jsonl'
            log.write_text('running 4 tests\ntest cli::tests::test_004_01_exit_code_table ... ok\n'
                           'test store::test_062_03_outbox_and_test_062_04_fs ... FAILED\ntest misc::helper_works ... ok\n'
                           'test slow::test_038_02_property ... ignored, needs time\ntest latest_test_1234_56 ... ok\n')
            r=subprocess.run([sys.executable,str(ROOT/'scripts/product/test_results.py'),'--log',str(log),'--out',str(out)],capture_output=True,text=True)
            self.assertEqual(r.returncode,0,r.stderr)
            got=[(x['test_id'],x['status']) for x in map(json.loads,out.read_text().splitlines())]
            self.assertEqual(got,[('TEST-004-01','PASS'),('TEST-062-03','FAIL'),('TEST-062-04','FAIL'),('TEST-038-02','SKIPPED')])
            schema=load_json(ROOT/'contracts/test-result.schema.json')
            from jsonschema import Draft202012Validator
            for line in out.read_text().splitlines():Draft202012Validator(schema).validate(json.loads(line))

def scan_metadata(root,graph,dirs=None,features=None,targets=None,member_features=None,path_deps=(),target_dir=None):
    """cargo metadata JSON for persistence_scan.py: graph maps a package to [(extern name, package, kind)]; dirs maps a
    workspace member to its directory under root (default crates/<name>); features lists rusqlite's features."""
    members=['hoh-core','hoh-agents','hoh-app']
    names=set(graph)|{p for deps in graph.values() for _,p,_ in deps}|set(members)
    pid=lambda n:f'id:{n}'
    where=lambda n:(Path(root)/(dirs or {}).get(n,f'crates/{n}')/'Cargo.toml') if n in members else Path(f'/registry/{n}/Cargo.toml')
    return {'workspace_members':[pid(m) for m in members],'target_directory':str(Path(root)/(target_dir or 'target')),
            'packages':[{'id':pid(n),'name':n,'manifest_path':str(where(n)),'features':(member_features or {}).get(n,{}),
                         'source':None if n in members or n in (path_deps or ()) else 'registry+https://github.com/rust-lang/crates.io-index',
                         'targets':[{'kind':[k],'src_path':str(Path(root)/s)} for k,s in (targets or {}).get(n,[])]} for n in sorted(names)],
            'resolve':{'nodes':[{'id':pid(n),'features':(features if n=='rusqlite' and features is not None else ['bundled','default']) if n=='rusqlite' else [],
                                 'deps':[{'name':e,'pkg':pid(p),'dep_kinds':[{'kind':k,'target':None}]} for e,p,k in graph.get(n,[])]} for n in sorted(names)]}}

BASE_GRAPH={'hoh-core':[('rusqlite','rusqlite',None)],'rusqlite':[('libsqlite3_sys','libsqlite3-sys',None)],
            'hoh-agents':[('hoh_core','hoh-core',None)],'hoh-app':[('hoh_core','hoh-core',None),('hoh_agents','hoh-agents',None)]}

def clippy_toml(skip=()):
    sys.path.insert(0,str(ROOT/'scripts/product'))
    import persistence_scan
    return 'disallowed-methods = [\n'+''.join(f'  {{ path = "{p}", reason = "AC-062-05" }},\n' for p in sorted(persistence_scan.REQUIRED) if p not in skip)+']\n'

class PersistenceScan(unittest.TestCase):
    """scripts/product/persistence_scan.py: AC-062-05, SQLite only through the Persistence module."""
    def run_scan(self,files,graph=BASE_GRAPH,out=None,dirs=None,features=None,clippy=None,targets=None,member_features=None,path_deps=(),
                 whitelist=None,symlinks=None,git=None,target_dir=None):
        """whitelist: the files of scripts/product/persistence-whitelist.json; None lists every fixture file the
        content rule would flag (so a test of another rule sees only that rule), False writes no whitelist.
        git: a .gitignore text to make the root a git work tree with."""
        with tempfile.TemporaryDirectory() as d:
            root=Path(d)/'repo';root.mkdir()
            files={'clippy.toml':clippy_toml() if clippy is None else clippy,**files}
            for m in ('hoh-core','hoh-agents','hoh-app'):
                files.setdefault(f"{(dirs or {}).get(m,f'crates/{m}')}/Cargo.toml",'[package]\n')
            if whitelist is None:
                sys.path.insert(0,str(ROOT/'scripts/product'))
                import persistence_scan
                members=tuple(f"{(dirs or {}).get(m,f'crates/{m}')}/" for m in ('hoh-core','hoh-agents','hoh-app'))
                whitelist={rel:'fixture' for rel,text in files.items()
                           if persistence_scan.mentions(text) and not rel.startswith(persistence_scan.PERSISTENCE)
                           and not (rel.endswith('.rs') and rel.startswith(members))}
            if whitelist is not False:
                files.setdefault('scripts/product/persistence-whitelist.json',json.dumps({'files':whitelist}))
            for rel,text in files.items():
                p=root/rel;p.parent.mkdir(parents=True,exist_ok=True);p.write_text(text)
            for rel,target in (symlinks or {}).items():
                p=root/rel;p.parent.mkdir(parents=True,exist_ok=True);p.symlink_to(target)
            if git is not None:
                (root/'.gitignore').write_text(git)
                subprocess.run(['git','init','-q',str(root)],check=True)
            meta=Path(d)/'metadata.json';meta.write_text(json.dumps(scan_metadata(root,graph,dirs,features,targets,member_features,path_deps,target_dir)))
            argv=[sys.executable,str(ROOT/'scripts/product/persistence_scan.py'),'--root',str(root),'--metadata',str(meta)]
            if out:argv+=['--out',str(out)]
            r=subprocess.run(argv,capture_output=True,text=True)
            return r.returncode,json.loads(r.stdout)
    def kinds(self,res):return sorted((h['path'],h['kind']) for h in res['hits'])
    def test_the_persistence_module_may_use_sqlite_and_others_may_name_things_freely(self):
        rc,res=self.run_scan({'crates/hoh-core/src/persistence/mod.rs':'#![allow(clippy::disallowed_methods)]\nfn f(c:&rusqlite::Connection){c.execute("INSERT INTO t VALUES (1)",[]);}',
                              'crates/hoh-app/src/main.rs':'// INSERT INTO t is only a comment\n/* DELETE FROM t */ fn main(){let s="update the docs"; Command.execute(); tx.transaction();}\n#[allow(dead_code)] fn x(){}',
                              'crates/hoh-app/tests/db.rs':'fn t(){ let q = "INSERT INTO t VALUES (1)"; }'})  # SQL in tests is no hit
        self.assertEqual((rc,res['status'],res['hits']),(0,'PASS',[]))  # M2T01R-06: method names alone are not hits
    def test_sqlite_dependencies_outside_hoh_core_are_hits_by_package_identity(self):
        # M2T01R-04: a renamed package, a workspace-inherited one (resolved by cargo) and one reached through another crate.
        graph=dict(BASE_GRAPH)
        graph['hoh-app']=BASE_GRAPH['hoh-app']+[('db','rusqlite',None)]
        graph['hoh-agents']=BASE_GRAPH['hoh-agents']+[('orm','orm',None),('fixture_db','rusqlite','dev')]
        graph['orm']=[('ffi','libsqlite3-sys',None)]
        graph['hoh-core']=BASE_GRAPH['hoh-core']+[('rusqlite','rusqlite','build')]
        rc,res=self.run_scan({'crates/hoh-app/src/main.rs':'fn main(){}'},graph)
        self.assertEqual(rc,1)
        self.assertEqual(sorted(h['match'] for h in res['hits']),['hoh-agents -> orm -> libsqlite3-sys','hoh-app -> rusqlite','hoh-core build -> rusqlite'])
        self.assertEqual(res['dev_dependency_uses'],[{'member':'hoh-agents','package':'rusqlite'}])
    def test_every_repository_file_that_mentions_sqlite_is_whitelisted(self):
        # M2T01R-51: wherever a helper lies and however product code reaches it (Path::join, python3 -m, a driver
        # importing it, an embedded driver), the file that mentions SQLite or holds a SQL write is itself a hit unless
        # the whitelist (AC-062-05's whitelist configuration) names it. Markdown under docs/ and reports/ is prose.
        files={'crates/hoh-app/src/bin/r.rs':'fn main(){std::process::Command::new("python3").arg(std::path::Path::new("scripts").join("db_helper.py"));}',
               'scripts/db_helper.py':'import sqlite3\nsqlite3.connect("x").execute("INSERT INTO t VALUES (42)")\n',
               'tools/driver.py':'import db_helper\n',
               'crates/hoh-app/assets/q.sql':'DELETE FROM t;',
               'resources/prompt.md':'Write the SQLite database.',
               'docs/04-architecture.md':'The writer owns SQLite.',
               'reports/reviews/M2.md':'INSERT INTO is a SQL write.',
               'docs/run.sh':'sqlite3 db < x',
               'scripts/tool.rs':'fn main(){ let _ = "sqlite3"; }',
               'listed.py':'import sqlite3\n',
               'crates/hoh-core/src/persistence/mod.rs':'#![allow(clippy::disallowed_methods)]\n// SQLite'}
        rc,res=self.run_scan(files,whitelist={'clippy.toml':'the boundary configuration','listed.py':'a reviewed helper'})
        self.assertEqual(rc,1)
        self.assertEqual(self.kinds(res),[('crates/hoh-app/assets/q.sql','unlisted-file'),('docs/run.sh','unlisted-file'),
                                          ('resources/prompt.md','unlisted-file'),('scripts/db_helper.py','unlisted-file'),
                                          ('scripts/tool.rs','unlisted-file')])
    def test_the_whitelist_names_only_files_that_need_it(self):
        files={'old.py':'print(1)\n','crates/hoh-core/src/persistence/mod.rs':'#![allow(clippy::disallowed_methods)]\n// SQLite',
               'crates/hoh-app/src/main.rs':'// SQLite\nfn main(){}'}
        rc,res=self.run_scan(files,whitelist={'clippy.toml':'ok','old.py':'no longer mentions it','gone.py':'deleted',
                                              'crates/hoh-core/src/persistence/mod.rs':'covered by the module','crates/hoh-app/src/main.rs':'rust'})
        self.assertEqual(rc,1)
        self.assertEqual(sorted(h['match'] for h in res['hits'] if h['kind']=='whitelist'),
                         ['crates/hoh-app/src/main.rs: Rust sources of the members follow the literal and clippy rules',
                          'crates/hoh-core/src/persistence/mod.rs: inside the Persistence module',
                          'gone.py: not a file of the repository','old.py: mentions no SQLite and holds no SQL write'])
        rc,res=self.run_scan({},whitelist=False)
        self.assertEqual((rc,self.kinds(res)),(1,[('clippy.toml','unlisted-file'),('scripts/product/persistence-whitelist.json','whitelist')]))
    def test_in_a_git_work_tree_the_candidate_files_are_scanned(self):
        # Tracked and untracked files are scanned; ignored ones are not part of any candidate.
        files={'scripts/new_helper.py':'import sqlite3\n','ignored/local.py':'import sqlite3\n'}
        rc,res=self.run_scan(files,whitelist={'clippy.toml':'ok'},git='ignored/\n')
        self.assertEqual((rc,self.kinds(res)),(1,[('scripts/new_helper.py','unlisted-file')]))
    def test_a_moved_build_output_hides_no_candidate_file(self):
        # M2T01R-55: with the build output moved into the repository, a helper beside it is still a candidate file
        # (git's ignore rules alone decide), and the cargo configuration that moved it is refused.
        files={'.cargo/config.toml':'[build]\ntarget-dir = "build"\n','build/helpers/db_helper.py':'import sqlite3\n',
               'build/debug/out.py':'import sqlite3\n'}
        rc,res=self.run_scan(files,whitelist={'clippy.toml':'ok'},git='build/debug/\n',target_dir='build')
        self.assertEqual(rc,1)
        self.assertEqual(self.kinds(res),[('.cargo/config.toml','cargo-config'),('build/helpers/db_helper.py','unlisted-file')])
        self.assertIn('build.target-dir',[h['match'] for h in res['hits']])
    def test_doc_attributes_are_documentation_not_literals(self):
        # M2T01R-56: `#[doc = "..."]` is what `///` writes; a named macro argument called doc is still a value.
        ok={'crates/hoh-app/src/main.rs':'#![doc = "Talks to SQLite only through hoh-core."]\n'
                                          '#[doc = "Uses SQLite only through the Persistence API."]\n'
                                          '#[cfg_attr(docsrs, doc = "INSERT INTO happens in persistence")]\nfn main(){}'}
        self.assertEqual(self.run_scan(ok)[1]['hits'],[])
        bad={'crates/hoh-app/src/main.rs':'fn main(){ let doc = "sqlite3"; let _ = format!("{doc}", doc = "sqlite3"); }'}
        self.assertEqual(self.kinds(self.run_scan(bad)[1]),[('crates/hoh-app/src/main.rs','sqlite-literal')])
    def test_rust_sources_under_a_moved_build_output_are_read(self):
        # M2T01R-59: CARGO_TARGET_DIR=build with a member at build/helper; only git's ignore rules decide.
        files={'build/helper/src/lib.rs':'pub fn w(){ let _ = std::process::Command::new("sqlite3"); }','build/debug/gen.rs':'// sqlite3'}
        rc,res=self.run_scan(files,dirs={'hoh-agents':'build/helper'},git='build/debug/\n',target_dir='build')
        self.assertEqual((rc,self.kinds(res)),(1,[('build/helper/src/lib.rs','sqlite-literal')]))
    def test_documentation_forms_are_documentation(self):
        # M2T01R-60: doc = include_str!(…), doc = concat!(…) and doc text naming a file are documentation.
        ok={'README.md':'HoH stores its state in SQLite.\n','CONTRACT.md':'SQLite is the store.\n',
            'crates/hoh-app/src/main.rs':'#![doc = include_str!("../../../README.md")]\n'
                                          '#[doc = concat!("Talks to ", "SQLite only through persistence.")]\n'
                                          '#[doc = "See CONTRACT.md"]\n#[doc = "CONTRACT.md"]\nfn main(){}'}
        rc,res=self.run_scan(ok)
        self.assertEqual((rc,res['hits']),(0,[]))
    def test_doc_tokens_handed_to_a_macro_are_values(self):
        # M2T01R-61: `#[doc = "sqlite3"]` inside a macro invocation is input the macro may use at run time.
        bad={'crates/hoh-app/src/main.rs':'macro_rules! run { (#[doc = $s:literal]) => { std::process::Command::new($s) }; }\n'
                                           'fn main(){ let _ = run!(#[doc = "sqlite3"]); }'}
        self.assertEqual(self.kinds(self.run_scan(bad)[1]),[('crates/hoh-app/src/main.rs','sqlite-literal')])
    def test_cargo_configurations_may_not_include_others(self):
        # M2T01R-62: an included configuration could hold a profile the checks never see.
        rc,res=self.run_scan({'.cargo/config.toml':'include = ["extra.toml"]\n','.cargo/extra.toml':'[profile.production]\ninherits = "release"\n'})
        self.assertEqual(rc,1)
        self.assertIn('include',[h['match'] for h in res['hits'] if h['kind']=='cargo-config'])
    def test_macro_input_is_checked_whatever_its_form(self):
        # M2T01R-63: a doc comment inside a macro invocation is input the macro may take as #[doc = $s:literal];
        # M2T01R-64: macro names are Rust identifiers, not only ASCII ones.
        rule='macro_rules! {name} {{ (#[doc = $s:literal]) => {{ std::process::Command::new($s.trim()) }}; }}\n'
        # M2T01R-69: a raw keyword (r#while) and a name with a combining mark (e + U+0301) are macro names too.
        # M2T01R-71: an invocation of a macro named r#macro_rules is an invocation, not a definition.
        for call in ['r#macro_rules!(#[doc = "sqlite3"])','r#macro_rules!{ #[cfg(test)] mod helper { fn h() { let _ = "SQLite helper"; } } }']:
            src='macro_rules! r#macro_rules { ($($t:tt)*) => { () }; }\nfn main(){ let _ = '+call+'; }'
            res=self.run_scan({'crates/hoh-app/src/main.rs':src})[1]
            self.assertEqual(self.kinds(res),[('crates/hoh-app/src/main.rs','sqlite-literal')],call)
        for name,call in [('run','run!(\n    /// sqlite3\n)'),('运行','运行!(#[doc = "sqlite3"])'),('r#try_it','r#try_it!(#[doc = "sqlite3"])'),
                          ('r#while','r#while!(#[doc = "sqlite3"])'),('r#while','r#while!(\n    /// sqlite3\n)'),
                          ('e\u0301','e\u0301!(#[doc = "sqlite3"])')]:
            src=rule.format(name=name)+'fn main(){ let _ = '+call+'; }'
            res=self.run_scan({'crates/hoh-app/src/main.rs':src})[1]
            self.assertEqual(self.kinds(res),[('crates/hoh-app/src/main.rs','sqlite-literal')],name)
    def test_docs_a_macro_writes_and_inline_unit_tests_are_not_product_literals(self):
        # M2T01R-65: a macro definition that writes a doc attribute documents what it generates.
        # M2T01R-66/-70: an inline module under exactly #[cfg(test)] (also with a raw name) is test code, like tests/.
        ok={'crates/hoh-app/src/main.rs':'macro_rules! documented { () => { #[doc = "Uses SQLite only through persistence."] fn f() {} }; }\n'
                                          'documented!();\nfn main(){}\n'
                                          '#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() { assert_eq!(["SQL", "ite"].concat(), "SQLite"); }\n}\n'
                                          '#[cfg(test)]\n#[allow(dead_code)]\npub(crate) mod r#unit { fn t() { let _ = "sqlite3"; } }\n'
                                          '#[cfg(test)]\nmod e\u0301 { fn t() { assert_eq!(["SQL", "ite"].concat(), "SQLite"); } }'}
        rc,res=self.run_scan(ok)
        self.assertEqual((rc,res['hits']),(0,[]))
    def test_only_real_inline_cfg_test_modules_are_test_code(self):
        # M2T01R-67: module files are never exempt (resolving them misjudged a file the product compiles); M2T01R-68:
        # #[cfg(test)] handed to a macro is its input; other predicates are product code.
        bad={'crates/hoh-app/src/main.rs':'mod api { pub mod helper; }\nfn main(){}\n#[cfg(not(test))]\nmod v { fn f() { let _ = "sqlite3"; } }\n'
                                           '#[cfg(any(test, unix))]\nmod w { fn f() { let _ = "libsqlite.so"; } }\n'
                                           'product!(#[cfg(test)] mod helper { fn f() { let _ = "SQLite helper"; } });',
             'crates/hoh-app/src/lib.rs':'#[cfg(test)]\nmod api;',
             'crates/hoh-app/src/api.rs':'pub mod helper;',
             'crates/hoh-app/src/api/helper.rs':'pub fn f() { let _ = std::process::Command::new("sqlite3"); }',
             'crates/hoh-app/src/util.rs':'#[cfg(test)]\nmod tests;',
             'crates/hoh-app/src/util/tests.rs':'#[test]\nfn t() { let _ = "sqlite3"; }'}
        res=self.run_scan(bad,targets={'hoh-app':[('bin','crates/hoh-app/src/main.rs'),('lib','crates/hoh-app/src/lib.rs')]})[1]
        self.assertEqual(sorted(h['path'] for h in res['hits'] if h['kind']=='sqlite-literal'),
                         ['crates/hoh-app/src/api/helper.rs','crates/hoh-app/src/main.rs','crates/hoh-app/src/main.rs',
                          'crates/hoh-app/src/main.rs','crates/hoh-app/src/util/tests.rs'])
    def test_every_whitelist_entry_states_its_reason(self):
        # M2T01R-58.
        files={'a.py':'import sqlite3\n','b.py':'import sqlite3\n'}
        rc,res=self.run_scan(files,whitelist={'clippy.toml':'ok','a.py':'','b.py':'  '})
        self.assertEqual((rc,sorted(h['match'] for h in res['hits'])),(1,['a.py: no reason given','b.py: no reason given']))
        rc,res=self.run_scan({},whitelist=['clippy.toml'])
        self.assertEqual(rc,1)
        self.assertIn('scripts/product/persistence-whitelist.json',[h['path'] for h in res['hits'] if h['kind']=='whitelist'])
    def test_symlinks_are_refused(self):
        # M2T01R-52: a product module linked to a test helper (or a linked directory) would run code that the checks
        # classify as test code.
        files={'crates/hoh-app/src/bin/r10/main.rs':'mod helper;\nfn main(){helper::write()}',
               'crates/hoh-app/tests/helper.rs':'pub fn write(){ std::process::Command::new("sqlite3"); }'}
        rc,res=self.run_scan(files,symlinks={'crates/hoh-app/src/bin/r10/helper.rs':'../../../tests/helper.rs',
                                             'crates/hoh-app/src/shared':'../tests'})
        self.assertEqual(rc,1)
        self.assertEqual([k for k in self.kinds(res) if k[1]=='symlink'],
                         [('crates/hoh-app/src/bin/r10/helper.rs','symlink'),('crates/hoh-app/src/shared','symlink')])
    def test_profiles_in_cargo_configurations_are_refused(self):
        # M2T01R-50: a profile declared in a cargo configuration builds code that static.sh never lints.
        files={'.cargo/config.toml':'[profile.production]\ninherits = "release"\npanic = "abort"\n[profile.release]\npanic = "abort"\n[profile.dev]\nopt-level = 1\n'}
        rc,res=self.run_scan(files)
        self.assertEqual(rc,1)
        self.assertEqual(sorted(h['match'] for h in res['hits'] if h['kind']=='cargo-config'),['profile.production','profile.release.panic'])
    def test_files_embedded_from_outside_the_repository_are_refused(self):
        with tempfile.TemporaryDirectory() as outside:
            p=Path(outside)/'plain.txt';p.write_text('nothing here\n')
            rc,res=self.run_scan({'crates/hoh-app/src/main.rs':f'const S: &str = include_str!("{p}");\nfn main(){{}}'})
        self.assertEqual(rc,1)
        self.assertEqual([h['match'] for h in res['hits'] if h['kind']=='included-file'],[f'{p}: outside the repository'])
    def test_the_clippy_boundary_cannot_be_switched_off(self):
        # M2T01R-16..19: the boundary is clippy's disallowed-methods; its configuration and every way to allow the lint
        # outside persistence are checked.
        files={'crates/hoh-app/src/main.rs':'#[allow(clippy::disallowed_methods)]\nfn a(){}\n#[cfg_attr(test, allow(warnings))]\nfn b(){}',
               'crates/hoh-core/src/lib.rs':'#![allow(clippy::style)]\n#[expect(clippy::all)]\nmod m {}\n#[allow(dead_code, clippy::too_many_lines)] fn ok(){}',
               'crates/hoh-agents/clippy.toml':'',
               'crates/hoh-agents/Cargo.toml':'[package]\nname = "hoh-agents"\n[lints.clippy]\ndisallowed_methods = "allow"\n',
               'Cargo.toml':'[workspace]\n[workspace.lints.clippy]\nstyle = { level = "allow", priority = -1 }\n',
               '.cargo/config.toml':'[build]\nrustflags = ["-A", "clippy::disallowed_methods"]\n'}
        rc,res=self.run_scan(files,clippy=clippy_toml(skip={'rusqlite::Connection::execute'})+'')
        self.assertEqual(rc,1)
        self.assertEqual(self.kinds(res),[('.cargo/config.toml','cargo-config'),('Cargo.toml','lint-table'),('clippy.toml','clippy-config'),
                                          ('crates/hoh-agents/Cargo.toml','lint-table'),('crates/hoh-agents/clippy.toml','clippy-config'),
                                          ('crates/hoh-app/src/main.rs','lint-suppressed'),('crates/hoh-app/src/main.rs','lint-suppressed'),
                                          ('crates/hoh-core/src/lib.rs','lint-suppressed'),('crates/hoh-core/src/lib.rs','lint-suppressed')])
        self.assertIn('rusqlite::Connection::execute not disallowed',[h['match'] for h in res['hits']])
        rc,res=self.run_scan({},clippy='disallowed-methods = [ { path = "rusqlite::Connection::open", allow-invalid = true } ]\n')
        self.assertIn('rusqlite::Connection::open: allow-invalid',[h['match'] for h in res['hits']])
    def test_lint_names_are_normalised_and_test_or_example_files_are_not_exempt(self):
        # M2T01R-23: Cargo's hyphenated lint key (inherited through workspace = true), spaced paths kept by rustfmt::skip,
        # cfg_attr and the lint's former name. M2T01R-25: any .rs file may be compiled into the product (a bin path into
        # examples/, a cfg_attr(path) module from tests/), so none is exempt.
        files={'Cargo.toml':'[workspace]\n[workspace.lints.clippy]\ndisallowed-methods = { level = "allow", priority = 1 }\n',
               'crates/hoh-app/src/main.rs':'#[rustfmt::skip]\n#[allow(clippy :: disallowed_methods)]\nfn a(){}\n'
                                            '#[cfg_attr(unix, allow(clippy::disallowed_method))]\nfn b(){}\n'
                                            '#[cfg_attr(unix, path = "../tests/r5outside.rs")]\nmod outside;',
               'crates/hoh-app/tests/r5outside.rs':'#![allow(clippy::disallowed_methods)]\npub fn w(){}',
               'crates/hoh-app/examples/r5.rs':'#![expect(warnings)]\nfn main(){}'}
        rc,res=self.run_scan(files)
        self.assertEqual(rc,1)
        self.assertEqual(self.kinds(res),[('Cargo.toml','lint-table'),('crates/hoh-app/examples/r5.rs','lint-suppressed'),
                                          ('crates/hoh-app/src/main.rs','lint-suppressed'),('crates/hoh-app/src/main.rs','lint-suppressed'),
                                          ('crates/hoh-app/src/main.rs','path-or-include'),('crates/hoh-app/tests/r5outside.rs','lint-suppressed')])
    def test_the_canary_must_refuse_every_disallowed_method(self):
        sys.path.insert(0,str(ROOT/'scripts/product'))
        import persistence_scan as ps
        canary=ps.CANARY
        log=lambda paths,where=canary,extra='':''.join(f'error: use of a disallowed method `{p}`\n --> {where}:1:1\n' for p in paths)+extra+'error: could not compile `hoh-core`\n'
        self.assertEqual(ps.canary_verdict(log(sorted(ps.CANARY_REQUIRED)))['problems'],[])
        missing=sorted(ps.CANARY_REQUIRED)[1:]
        self.assertEqual(ps.canary_verdict(log(missing))['problems'],[f'not refused: {sorted(ps.CANARY_REQUIRED)[0]}'])
        elsewhere=ps.canary_verdict(log(sorted(ps.CANARY_REQUIRED))+log(['rusqlite::Connection::open'],'crates/hoh-app/src/main.rs'))['problems']
        self.assertEqual(len(elsewhere),1);self.assertIn('crates/hoh-app/src/main.rs',elsewhere[0])
        broken=ps.canary_verdict(log(sorted(ps.CANARY_REQUIRED),extra='error[E0308]: mismatched types\n'))['problems']
        self.assertEqual(broken,['error[E0308]: mismatched types'])
        self.assertEqual(len(ps.canary_verdict('')['problems']),len(ps.CANARY_REQUIRED))  # a canary that compiled refuses nothing
    def test_cargo_configuration_outside_the_repository_is_checked(self):
        # M2T01R-30: a wrapper or rustflags in an ancestor's or CARGO_HOME's configuration apply to the check as well.
        with tempfile.TemporaryDirectory() as d:
            base=Path(d);root=base/'repo';home=base/'home'
            (base/'.cargo').mkdir();(base/'.cargo/config.toml').write_text('[build]\nrustc-wrapper = "/tmp/w"\n[env]\nX = "1"\n')
            home.mkdir();(home/'config.toml').write_text('[target.x86_64-unknown-linux-gnu]\nrustflags = ["--cap-lints=allow"]\n[alias]\nclippy = "check"\n[net]\nretry = 3\n')
            for rel,text in {'clippy.toml':clippy_toml(),'crates/hoh-app/src/main.rs':'fn main(){}'}.items():
                p=root/rel;p.parent.mkdir(parents=True,exist_ok=True);p.write_text(text)
            for m in ('hoh-core','hoh-agents','hoh-app'):(root/f'crates/{m}').mkdir(parents=True,exist_ok=True);(root/f'crates/{m}/Cargo.toml').write_text('[package]\n')
            meta=base/'m.json';meta.write_text(json.dumps(scan_metadata(root,BASE_GRAPH)))
            r=subprocess.run([sys.executable,str(ROOT/'scripts/product/persistence_scan.py'),'--root',str(root),'--metadata',str(meta)],
                             capture_output=True,text=True,env={**os.environ,'CARGO_HOME':str(home)})
            res=json.loads(r.stdout)
            self.assertEqual(sorted(h['match'] for h in res['hits'] if h['kind']=='cargo-config'),
                             ['alias.clippy','build.rustc-wrapper','env','target.x86_64-unknown-linux-gnu.rustflags'])
    def test_every_target_root_forbids_the_lint(self):
        # M2T01R-27..29, -31, -33, -34: `forbid` makes the compiler reject any later allow (raw identifier, cfg_attr,
        # macro-generated), so every target root, wherever it lies, must carry it in its leading inner attributes (one in
        # an inline module covers that module only), together with forbid(unsafe_code); no build scripts.
        F='// boundary\n#![forbid(clippy::disallowed_methods)]\n#![forbid(unsafe_code)]\n'
        files={'crates/hoh-agents/src/lib.rs':'//! docs\n'+F+'pub mod x;',
               'crates/hoh-app/src/main.rs':'fn main(){}',
               'crates/hoh-app/src/bin/inner.rs':'#![forbid(unsafe_code)]\nmod m { #![forbid(clippy::disallowed_methods)] }\nfn main(){}',
               'crates/hoh-app/src/bin/no_unsafe.rs':'#![forbid(clippy::disallowed_methods)]\nfn main(){}',
               'outside.rs':'#![forbid(clippy::style)]\n#![forbid(unsafe_code)]\nfn main(){}',
               'crates/hoh-app/tests/t.rs':F+'#[test] fn t(){}',
               'crates/hoh-app/build.rs':F+'fn main(){}',
               'crates/hoh-core/src/lib.rs':'//! core\n#![forbid(unsafe_code)]\n#[cfg(x)]\n#[forbid(clippy::disallowed_methods)]\nmod a;\npub mod persistence;\n'
                                           'pub mod protocol;\npub const VERSION: &str = env!("CARGO_PKG_VERSION");\nfn extra() {}'}
        targets={'hoh-agents':[('lib','crates/hoh-agents/src/lib.rs')],
                 'hoh-app':[('bin','crates/hoh-app/src/main.rs'),('bin','crates/hoh-app/src/bin/inner.rs'),('bin','crates/hoh-app/src/bin/no_unsafe.rs'),
                            ('bin','outside.rs'),('test','crates/hoh-app/tests/t.rs'),('custom-build','crates/hoh-app/build.rs')],
                 'hoh-core':[('lib','crates/hoh-core/src/lib.rs')]}
        rc,res=self.run_scan(files,targets=targets)
        self.assertEqual(rc,1)
        self.assertEqual(sorted((h['path'],h['kind'],h['match']) for h in res['hits']),
                         [('crates/hoh-app/build.rs','build-script','hoh-app build script'),
                          ('crates/hoh-app/src/bin/inner.rs','missing-forbid','hoh-app bin root'),
                          ('crates/hoh-app/src/bin/no_unsafe.rs','missing-forbid','hoh-app bin root: unsafe_code'),
                          ('crates/hoh-app/src/main.rs','missing-forbid','hoh-app bin root'),('crates/hoh-app/src/main.rs','missing-forbid','hoh-app bin root: unsafe_code'),
                          ('crates/hoh-core/src/lib.rs','core-root','fn extra() {}'),('crates/hoh-core/src/lib.rs','missing-forbid','mod protocol'),
                          ('outside.rs','missing-forbid','hoh-app bin root'),('outside.rs','product-root-outside-src',"a product target root outside its member's src/"),
                          ('outside.rs','target-outside-members','a target root outside the member directories')])
    def test_features_clippy_cfgs_and_sqlite_subprocesses_are_hits(self):
        # M2T01R-35..38: member features (code clippy never compiles, or extra dependency APIs), cfg(not(clippy)), and
        # SQLite run as a subprocess from product code; test-only files may mention sqlite.
        F='#![forbid(clippy::disallowed_methods)]\n#![forbid(unsafe_code)]\n'
        files={'crates/hoh-app/src/main.rs':F+'#[cfg(not(clippy))]\nfn hidden(){}\nfn main(){ if cfg!(clippy) {} let _ = std::process::Command::new("sqlite3"); }',
               'crates/hoh-app/src/py.rs':'fn p(){ let _ = ["-c", "import sqlite3"]; }',
               'crates/hoh-app/tests/t.rs':F+'fn t(){ let _ = std::process::Command::new("sqlite3"); }',
               'crates/hoh-core/src/lib.rs':'#![forbid(unsafe_code)]\n#[cfg(hoh_clippy_canary)]\n#[forbid(clippy::disallowed_methods)]\nmod clippy_canary;\npub mod persistence;'}
        targets={'hoh-app':[('bin','crates/hoh-app/src/main.rs'),('test','crates/hoh-app/tests/t.rs')],'hoh-core':[('lib','crates/hoh-core/src/lib.rs')]}
        rc,res=self.run_scan(files,targets=targets,member_features={'hoh-core':{'r8':['rusqlite/backup']}})
        self.assertEqual(rc,1)
        self.assertEqual(sorted((h['path'].rsplit('/',1)[-1],h['kind'],h['match']) for h in res['hits']),
                         [('Cargo.toml','features',"hoh-core declares ['r8']"),('main.rs','clippy-cfg','clippy'),('main.rs','clippy-cfg','not(clippy)'),
                          ('main.rs','sqlite-literal','sqlite3'),('py.rs','sqlite-literal','import sqlite3')])
    def test_embedded_files_test_classification_and_macro_cfgs(self):
        # M2T01R-41..43: an embedded helper run by an interpreter, a test target beside product modules exempting nothing,
        # and a cfg predicate taken from a macro argument.
        F='#![forbid(clippy::disallowed_methods)]\n#![forbid(unsafe_code)]\n'
        files={'crates/hoh-app/src/main.rs':F+'mod helper;\nfn main(){ let _ = include_str!("db_helper.py"); let _ = include_str!("schema.json"); '
                                              'let _ = include_bytes!(concat!(env!("OUT_DIR"), "/x")); }\n'
                                              'macro_rules! hide { ($c:meta) => { #[cfg(not($c))] fn w(){} } }',
               'crates/hoh-app/src/db_helper.py':'import sqlite3\n',
               'crates/hoh-app/src/schema.json':'{"type": "object"}',
               'crates/hoh-app/src/helper/mod.rs':'pub fn w(){ let _ = std::process::Command::new("sqlite3"); }',
               'crates/hoh-app/src/helper/tests.rs':F+'#[test] fn t(){}',
               'crates/hoh-app/tests/ok.rs':F+'fn t(){ let _ = "sqlite3"; }'}
        targets={'hoh-app':[('bin','crates/hoh-app/src/main.rs'),('test','crates/hoh-app/src/helper/tests.rs'),('test','crates/hoh-app/tests/ok.rs')]}
        rc,res=self.run_scan(files,targets=targets)
        self.assertEqual(rc,1)
        self.assertEqual(sorted((h['path'].rsplit('/',1)[-1],h['kind'],h['match']) for h in res['hits']),
                         [('main.rs','clippy-cfg','not($c)'),('main.rs','included-file','db_helper.py: mentions SQLite or holds a SQL write'),
                          ('main.rs','included-file','include_str!/include_bytes! of a computed path'),
                          ('main.rs','named-file','db_helper.py: mentions SQLite or holds a SQL write'),('mod.rs','sqlite-literal','sqlite3')])
    def test_profiles_scripts_roots_include_aliases_path_deps_and_persistence_macros(self):
        # M2T01R-44..49.
        F='#![forbid(clippy::disallowed_methods)]\n#![forbid(unsafe_code)]\n'
        files={'Cargo.toml':'[workspace]\n[profile.production]\ninherits = "release"\n[profile.release]\npanic = "abort"\n',
               'crates/hoh-app/src/main.rs':F+'fn main(){ let _ = std::process::Command::new("python3").arg("scripts/db_helper.py"); }',
               'scripts/db_helper.py':'import sqlite3\n',
               'crates/hoh-app/tool.rs':F+'mod tests { pub mod db_helper; }\nfn main(){}',
               'crates/hoh-app/tests/db_helper.rs':'pub fn w(){}',
               'crates/hoh-app/src/embed.rs':'use std::include_str as embed_text;\nconst S: &str = embed_text!("x.txt");',
               'crates/hoh-core/src/persistence/mod.rs':'macro_rules! gate { () => {} }'}
        graph=dict(BASE_GRAPH);graph['hoh-core']=BASE_GRAPH['hoh-core']+[('db_helper','db-helper',None)]
        targets={'hoh-app':[('bin','crates/hoh-app/src/main.rs'),('bin','crates/hoh-app/tool.rs')]}
        rc,res=self.run_scan(files,graph=graph,targets=targets,path_deps=('db-helper',))
        self.assertEqual(rc,1)
        self.assertEqual(sorted((h['path'].rsplit('/',1)[-1],h['kind'],h['match']) for h in res['hits']),
                         [('Cargo.toml','path-dependency','db-helper is not a workspace member'),('Cargo.toml','profile','profile.production'),
                          ('Cargo.toml','profile','profile.release.panic'),('embed.rs','included-file','an include macro imported under another name'),
                          ('main.rs','named-file','scripts/db_helper.py: mentions SQLite or holds a SQL write'),
                          ('mod.rs','persistence-macro','macro_rules! in persistence'),('tool.rs','product-root-outside-src',"a product target root outside its member's src/")])
    def test_new_rusqlite_features_need_the_boundary_extended(self):
        rc,res=self.run_scan({},features=['bundled','default','backup'])
        self.assertEqual(self.kinds(res),[('Cargo.lock','rusqlite-feature')])
    def test_members_are_found_from_metadata_and_path_or_include_are_hits(self):
        # M2T01R-14/-21: a member outside crates/, a module compiled through #[path], and include!.
        dirs={'hoh-core':'engine/hoh-core'}
        rc,res=self.run_scan({'engine/hoh-core/src/lib.rs':'#[path = "../database_ops.rs"]\nmod ops;\nconst X: &str = include!("../sql.rs");',
                              'engine/hoh-core/database_ops.rs':'#![allow(clippy::disallowed_methods)]\nfn w(){}',
                              'engine/hoh-core/src/persistence/mod.rs':'fn p(){}'},dirs=dirs)
        self.assertEqual(rc,1)
        self.assertEqual(self.kinds(res),[('engine/hoh-core/database_ops.rs','lint-suppressed'),('engine/hoh-core/src/lib.rs','path-or-include')])
        self.assertEqual(res['files_scanned'],3)
    def test_sql_write_literals_are_decoded_and_joined_like_rust(self):
        # M2T01R-03: format! placeholders, concat!, a continuation inside a word, a hex escape, a raw string, upserts.
        src=('fn g(){let a=format!("UPDATE {table} SET state = ?1"); let b=concat!("INSERT ", "INTO t VALUES (1)");\n'
             'let c="INS\\\n      ERT INTO t VALUES (2)"; let d="DELETE\\x20FROM t"; let e=r#"replace into t values (3)"#;\n'
             'let f="INSERT OR IGNORE INTO t VALUES (4)"; let g="... ON CONFLICT(id) DO UPDATE SET x = 1";}')
        rc,res=self.run_scan({'crates/hoh-app/src/api.rs':src})
        self.assertEqual(rc,1)
        self.assertEqual(sorted(h['match'].upper() for h in res['hits']),sorted(['UPDATE {TABLE} SET','INSERT INTO','INSERT INTO','DELETE FROM','REPLACE INTO','INSERT OR IGNORE INTO','ON CONFLICT(ID) DO UPDATE']))
    def test_web_lockfiles_are_searched_through(self):
        # M2T01R-10: a transitive npm alias and a package object's name in a v3 lockfile.
        lock={'lockfileVersion':3,'packages':{'':{'dependencies':{'orm':'1'}},'node_modules/orm':{'version':'1','dependencies':{'db':'npm:better-sqlite3@12'}},
                                              'node_modules/db':{'name':'better-sqlite3','version':'12.0.0'}}}
        rc,res=self.run_scan({'crates/hoh-app/src/main.rs':'fn main(){}','web/package.json':json.dumps({'dependencies':{'orm':'1'}}),
                              'web/package-lock.json':json.dumps(lock)})
        self.assertEqual(rc,1)
        self.assertEqual(sorted(h['match'] for h in res['hits']),['better-sqlite3','npm:better-sqlite3@12'])
    def test_web_sqlite_dependencies_and_imports_are_hits(self):
        rc,res=self.run_scan({'crates/hoh-app/src/main.rs':'fn main(){}',
                              'web/package.json':json.dumps({'dependencies':{'db':'npm:better-sqlite3@12','react':'18'}}),
                              'web/src/a.ts':'import x from "sql.js-wrapper-sqlite";'})
        self.assertEqual(self.kinds(res),[('web/package.json','web-dependency'),('web/src/a.ts','web-import')])
    def test_an_empty_scan_fails_and_the_result_is_recorded(self):
        with tempfile.TemporaryDirectory() as d:
            out=Path(d)/'test-results.jsonl'
            rc,res=self.run_scan({},out=out)
            self.assertEqual((rc,res['status'],res['files_scanned']),(1,'FAIL',0))
            line=json.loads(out.read_text())
            self.assertEqual((line['test_id'],line['status']),('TEST-062-05','FAIL'))
            from jsonschema import Draft202012Validator
            Draft202012Validator(load_json(ROOT/'contracts/test-result.schema.json')).validate(line)

class ProcessLifecycle(unittest.TestCase):
    def test_stdout_stderr_backpressure(self):
        with tempfile.TemporaryDirectory() as d:
            r=Path(d);rc,timed,_=execute([sys.executable,'-c','import sys;sys.stdout.write("x"*2000000);sys.stderr.write("y"*2000000)'],r,r/'out',r/'err',10)
            self.assertEqual(rc,0);self.assertFalse(timed);self.assertEqual((r/'out').stat().st_size,2000000);self.assertEqual((r/'err').stat().st_size,2000000)
    def test_termination_kills_stubborn_grandchild(self):
        with tempfile.TemporaryDirectory() as d:
            r=Path(d);pidfile=r/'pid'
            child='import os,signal,time; signal.signal(signal.SIGTERM,signal.SIG_IGN); open('+repr(str(pidfile))+',"w").write(str(os.getpid())); time.sleep(60)'
            parent='import subprocess,sys,time; subprocess.Popen([sys.executable,"-c",'+repr(child)+']);time.sleep(60)'
            proc=subprocess.Popen([sys.executable,'-c',parent],cwd=r,start_new_session=True,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
            try:
                deadline=time.monotonic()+10
                while time.monotonic()<deadline and not pidfile.exists():time.sleep(.02)
                self.assertTrue(pidfile.exists(),'child did not reach ready marker')
                pid=int(pidfile.read_text());terminate_group(proc,grace=.4)
                stat=Path('/proc')/str(pid)/'stat';deadline=time.monotonic()+2
                while stat.exists() and time.monotonic()<deadline:
                    if stat.read_text().split()[2]=='Z':break
                    time.sleep(.02)
                if stat.exists():self.assertEqual(stat.read_text().split()[2],'Z','grandchild still running')
            finally:
                if proc.poll() is None:terminate_group(proc,grace=.2)
    def test_path_traversal(self):
        with tempfile.TemporaryDirectory() as d:
            with self.assertRaises(ValueError):safe_path(Path(d),'../outside')

PY=sys.executable
PLAN={'ok':{'mode':'mock','argv':[PY,'-c','print("MOCK UNIT CHECK ONLY")']},
      'fail':{'mode':'mock','argv':[PY,'-c','import sys;sys.exit(9)']},
      'mutate':{'mode':'mock','argv':[PY,'-c','open("source.txt","w").write("modified")']},
      'live':{'mode':'live','argv':[PY,'-c','pass']},
      'artifact':{'mode':'mock','argv':[PY,'-c','import os;open(os.path.join(os.environ["HOH_EVIDENCE_DIR"],"result.json"),"w").write("{}")'],'artifacts':['result.json']},
      'missing-artifact':{'mode':'mock','argv':[PY,'-c','pass'],'artifacts':['result.json']},
      'escape':{'mode':'mock','argv':[PY,'-c','pass'],'cwd':'../'}}

class Signals(unittest.TestCase):
    def test_sigterm_stops_child_and_surfaces(self):
        import threading
        with tempfile.TemporaryDirectory() as d:
            r=Path(d);previous=install_termination_handlers()
            try:
                threading.Timer(0.3,lambda:os.kill(os.getpid(),signal.SIGTERM)).start();start=time.monotonic()
                with self.assertRaises(Terminated):execute([sys.executable,'-c','import time;time.sleep(30)'],r,r/'o',r/'e',30,grace=0.5)
                self.assertLess(time.monotonic()-start,10)
            finally:restore_handlers(previous)

class CandidateAndChecks(unittest.TestCase):
    def setUp(self):
        self.t=tempfile.TemporaryDirectory(prefix='hoh-candidate-');self.addCleanup(self.t.cleanup);self.root=Path(self.t.name);self.c,self.cr=init_repository(self.root,PLAN)
    def test_symlinked_artifact_rejected(self):
        p=self.root/'.hoh-dev/environment.json';real=self.root/'.hoh-dev/env-real.json';p.rename(real);p.symlink_to(real)
        with self.assertRaisesRegex(ValueError,'symlink'):verify(self.root,self.c)
    def test_clean_candidate(self):verify(self.root,self.c)
    def test_dirty_source(self):
        (self.root/'source.txt').write_text('changed')
        with self.assertRaisesRegex(ValueError,'dirty'):verify(self.root,self.c)
    def test_new_untracked_source(self):
        (self.root/'new.rs').write_text('new')
        with self.assertRaisesRegex(ValueError,'dirty'):verify(self.root,self.c)
    def test_environment_mutation(self):
        (self.root/'.hoh-dev/environment.json').write_text('{}')
        with self.assertRaisesRegex(ValueError,'changed'):verify(self.root,self.c)
    def test_forged_candidate_id(self):
        self.c['candidate_id']='CAND-'+'0'*64
        with self.assertRaisesRegex(ValueError,'identity'):verify(self.root,self.c)
    def runcheck(self,check,extra=(),out='check'):
        out=self.root/'.hoh-dev'/out
        args=['--check-id',check,'--candidate-file',str(self.root/'.hoh-dev/candidate.json'),'--out',str(out),*extra]
        err=io.StringIO()
        with patch.object(run_check,'ROOT',self.root),patch.object(run_check,'ref_for',lambda p:ref_for(p,self.root)),contextlib.redirect_stdout(io.StringIO()),contextlib.redirect_stderr(err):rc=run_check.main(args)
        return rc,(load_json(out/'execution.json') if (out/'execution.json').exists() else None),err.getvalue()
    def test_real_check_record(self):
        rc,r,_=self.runcheck('ok');self.assertEqual(rc,0);self.assertTrue(r['source_unchanged']);self.assertEqual((r['mode'],r['cwd']),('mock','.'))
        self.assertEqual(r['command'],PLAN['ok']['argv']);self.assertEqual(r['producer_attempt_id'],r['record_id'])
    def test_check_failing_exit(self):rc,r,_=self.runcheck('fail');self.assertEqual(rc,9);self.assertEqual(r['exit_code'],9)
    def test_check_mutating_source(self):
        rc,r,_=self.runcheck('mutate');self.assertNotEqual(rc,0);self.assertFalse(r['source_unchanged'])
    def test_mode_comes_from_plan(self):
        rc,r,_=self.runcheck('live');self.assertEqual(r['mode'],'live')
        with self.assertRaises(SystemExit):self.runcheck('ok',['--mode','live'],out='c2')
    def test_unplanned_command_refused(self):
        rc,r,err=self.runcheck('ok',['--','true']);self.assertEqual(rc,1);self.assertIsNone(r);self.assertIn('differs from the frozen check plan',err)
    def test_planned_command_echo_allowed(self):rc,r,_=self.runcheck('ok',['--',*PLAN['ok']['argv']]);self.assertEqual(rc,0)
    def test_candidate_file_outside_project_refused(self):
        with tempfile.TemporaryDirectory() as d:
            outside=Path(d)/'candidate.json';outside.write_text((self.root/'.hoh-dev/candidate.json').read_text())
            err=io.StringIO()
            with patch.object(run_check,'ROOT',self.root),contextlib.redirect_stdout(io.StringIO()),contextlib.redirect_stderr(err):
                rc=run_check.main(['--check-id','ok','--candidate-file',str(outside),'--out',str(self.root/'.hoh-dev/x')])
            self.assertEqual(rc,1);self.assertIn('inside the project',err.getvalue());self.assertFalse((self.root/'.hoh-dev/x').exists())
    def test_untracked_check_plan_refused(self):
        (self.root/'.hoh-dev/plan.json').write_text((self.root/'plan.json').read_text())
        with self.assertRaisesRegex(ValueError,'committed file'):freeze(self.root,self.root/'spec.md',self.root/'.hoh-dev/environment.json',self.root/'.hoh-dev/plan.json')
    def test_unknown_check_refused(self):rc,r,err=self.runcheck('nonexistent');self.assertEqual(rc,1);self.assertIn('not in the frozen check plan',err)
    def test_artifact_hashed(self):
        rc,r,_=self.runcheck('artifact');self.assertEqual(rc,0);self.assertEqual([a['name'] for a in r['artifacts']],['result.json'])
        self.assertTrue(r['artifacts'][0]['path'].startswith('.hoh-dev/check/artifacts/'))
    def test_missing_artifact_fails(self):
        rc,r,_=self.runcheck('missing-artifact');self.assertNotEqual(rc,0);self.assertEqual(r['missing_artifacts'],['result.json'])
    def test_cwd_escape_refused(self):rc,r,err=self.runcheck('escape');self.assertEqual(rc,1);self.assertIn('escapes',err)
    def test_evidence_dir_append_only(self):
        self.runcheck('ok');
        with self.assertRaises(SystemExit):self.runcheck('ok')
    def test_candidate_manifest_immutable(self):
        out=self.root/'.hoh-dev/cand.json';out.write_text('{}')
        import candidate as cand
        with patch.object(cand,'ROOT',self.root),contextlib.redirect_stderr(io.StringIO()):
            rc=cand.main(['freeze','--spec',str(self.root/'spec.md'),'--environment',str(self.root/'.hoh-dev/environment.json'),'--test-plan',str(self.root/'plan.json'),'--out',str(out)])
        self.assertEqual(rc,1);self.assertEqual(out.read_text(),'{}')

class PromptRenderer(unittest.TestCase):
    def test_substitution(self):self.assertEqual(render('goal={{GOAL}}',{'GOAL':'中文'}),'goal=中文')
    def test_missing_field(self):
        with self.assertRaises(ValueError):render('{{GOAL}}',{})
    def test_empty_field(self):
        with self.assertRaises(ValueError):render('{{GOAL}}',{'GOAL':' '})
    def test_nested_field(self):
        with self.assertRaises(ValueError):render('{{GOAL}}',{'GOAL':'{{MISSING}}'})
    def test_all_role_templates(self):
        c=load_json(ROOT/'examples/prompt-context.json')
        for p in (ROOT/'resources/prompts').glob('*.md'):
            with self.subTest(role=p.name):self.assertNotIn('{{',render(p.read_text(),c))
