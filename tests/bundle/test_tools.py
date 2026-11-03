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
 open(os.environ['FAKE_TRACE'],'w').write(json.dumps({'argv':args,'stdin':data.decode(),'env':sorted(k for k in os.environ if k.startswith(('CLAUDE','CODEX','HOH_','ANTHROPIC','AI_')))}))
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
    def test_oversized_json_frame(self):
        p=self.root/'raw';p.write_bytes(b'{"text":"'+b'x'*(1024*1024)+b'"}\n');self.assertEqual(inspect_output('pi',p)['malformed_records'],1)
    def test_dry_run_no_attempt_dir(self):
        p=subprocess.run([sys.executable,str(ROOT/'tools/agent_task.py'),'--agent','codex','--prompt-file',str(self.prompt),'--cwd',str(self.root),'--output-dir',str(self.root/'unused'),'--dry-run'],capture_output=True,text=True,env=self.env)
        self.assertEqual(p.returncode,0,p.stderr);self.assertFalse((self.root/'unused').exists())

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
