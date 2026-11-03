"""Protocol fixture recorder (M0-T06): case validators, bounded process stop, staged publication, manifest provenance."""
import copy,importlib.util,json,os,shutil,signal,subprocess,sys,tempfile,time,unittest
from pathlib import Path
from unittest.mock import patch
from helpers import ROOT

spec=importlib.util.spec_from_file_location('record',ROOT/'tests/fixtures/protocol/record.py')
record=importlib.util.module_from_spec(spec);spec.loader.exec_module(record)
FIXTURES=ROOT/'tests/fixtures/protocol'

def load_case(agent:str,version:str,case:str):
    d=FIXTURES/agent/version/case
    return record.frames((d/'stdout.jsonl').read_bytes()),json.loads((d/'meta.json').read_text()),record.sent_messages((d/'stdin.txt').read_bytes())

def context(agent:str,version:str)->dict:
    """Identifiers the recorder passes from earlier cases, rebuilt from the committed fixtures."""
    d=FIXTURES/agent/version;ctx={}
    if agent=='claude':
        fs=record.frames((d/'success/stdout.jsonl').read_bytes())
        ctx['session_id']=next(f['session_id'] for f in fs if f.get('type')=='system' and f.get('subtype')=='init')
    if agent=='codex':ctx['thread_id']=json.loads((d/'app-server-turn/meta.json').read_text())['thread_id']
    if agent=='pi':
        fs=record.frames((d/'rpc-prompt/stdout.jsonl').read_bytes())
        ctx['session_id']=record.pi_response(fs,'s1')['data']['sessionId']
    return ctx

def committed_cases():
    for meta in sorted(FIXTURES.glob('*/*/*/meta.json')):
        m=json.loads(meta.read_text());yield m['agent'],m['cli_version'],m['case']

class Validators(unittest.TestCase):
    def check(self,agent,case,fs,meta,sent=(),ctx=None,rc=None):
        return record.VALIDATORS[(agent,case)](fs,meta.get('exit_code') if rc is None else rc,meta,list(sent),ctx or {})
    def test_every_committed_case_is_valid(self):
        cases=list(committed_cases());self.assertGreaterEqual(len(cases),18)
        for agent,version,case in cases:
            with self.subTest(case=f'{agent}/{version}/{case}'):
                fs,meta,sent=load_case(agent,version,case)
                self.assertEqual(self.check(agent,case,fs,meta,sent,context(agent,version)),[])
    def test_every_case_has_a_validator(self):
        for agent,_,case in committed_cases():self.assertIn((agent,case),record.VALIDATORS)
    # M0R3-01: the four outcomes the old checks accepted.
    def test_claude_success_after_sigint_rejected(self):
        fs,meta,_=load_case('claude','2.1.317','sigint-cancel')
        bad=[f|{'subtype':'success','is_error':False} if f.get('type')=='result' else f for f in fs]
        problems=self.check('claude','sigint-cancel',bad,meta);self.assertTrue(any('reports success' in p for p in problems),problems)
        early=copy.deepcopy(meta);early['sigint_after_frame_index']=len(fs)
        self.assertTrue(any('before SIGINT' in p for p in self.check('claude','sigint-cancel',fs,early)))
        self.assertTrue(self.check('claude','sigint-cancel',fs,meta|{'killed_by_recorder':True}))
    def test_codex_echoed_user_token_without_assistant_output_rejected(self):
        fs,meta,sent=load_case('codex','0.166.0','app-server-turn')
        # Drop the agent's message items; the userMessage item still echoes the prompt containing the token.
        bad=[f for f in fs if not (f.get('method','').startswith('item/') and f.get('params',{}).get('item',{}).get('type')=='agentMessage')
             and f.get('method')!='item/agentMessage/delta']
        self.assertIn(record.TOKEN,json.dumps(bad))
        self.assertTrue(any('agentMessage' in p for p in self.check('codex','app-server-turn',bad,meta,sent)))
    def test_codex_failed_steer_completion_rejected(self):
        fs,meta,sent=load_case('codex','0.166.0','app-server-steer')
        failed=[copy.deepcopy(f) for f in fs]
        for f in failed:
            if f.get('method')=='turn/completed':f['params']['turn']['status']='failed'
        self.assertTrue(any("expected 'completed'" in p for p in self.check('codex','app-server-steer',failed,meta,sent)))
        ignored=[f for f in fs if not (f.get('method')=='item/completed' and f['params']['item'].get('type')=='agentMessage')]
        self.assertTrue(any('STEERED' in p for p in self.check('codex','app-server-steer',ignored,meta,sent)))
        accepted_stale=[{**f,'result':{}} if 'error' in f and f.get('id')==record.requests(sent,'turn/steer')[0]['id'] else f for f in fs]
        accepted_stale=[{k:v for k,v in f.items() if k!='error'} for f in accepted_stale]
        self.assertTrue(any('stale' in p for p in self.check('codex','app-server-steer',accepted_stale,meta,sent)))
    def test_pi_error_followed_by_settled_rejected(self):
        fs,meta,sent=load_case('pi','1.5.0','rpc-prompt')
        errored=[copy.deepcopy(f) for f in fs]
        for f in errored:
            if f.get('type')=='message_end' and f['message'].get('role')=='assistant':f['message']['stopReason']='error'
        self.assertTrue(any('stop reasons error' in p for p in self.check('pi','rpc-prompt',errored,meta,sent)))
        rejected=[f|{'success':False} if f.get('id')=='p1' else f for f in fs]
        self.assertTrue(any('not accepted' in p for p in self.check('pi','rpc-prompt',rejected,meta,sent)))
        no_reply=[f for f in fs if not (f.get('type')=='message_end' and f['message'].get('role')=='assistant')]
        self.assertTrue(any(record.TOKEN in p for p in self.check('pi','rpc-prompt',no_reply,meta,sent)))
    def test_error_cases_need_the_claimed_error(self):
        # M0R4-04: a generic failure must not pass as the specific one the case claims.
        for case,rid in [('rpc-no-provider','p1'),('rpc-invalid-model','m1')]:
            fs,meta,sent=load_case('pi','1.5.0',case)
            generic=[f|{'error':'Internal error'} if f.get('id')==rid and f.get('type')=='response' else f for f in fs]
            with self.subTest(case=case):self.assertTrue(self.check('pi',case,generic,meta,sent))
        fs,meta,sent=load_case('codex','0.166.0','app-server-steer');stale=record.requests(sent,'turn/steer')[0]['id']
        wrong=[{**f,'error':{'code':-32601,'message':'Method not found'}} if f.get('id')==stale and 'error' in f else f for f in fs]
        self.assertTrue(any('stale' in p for p in self.check('codex','app-server-steer',wrong,meta,sent)))
        fs,meta,_=load_case('codex','0.166.0','exec-turn-failed')
        other=[{**f,'error':{'message':'stream disconnected before completion'}} if f.get('type')=='turn.failed' else f for f in fs]
        self.assertTrue(self.check('codex','exec-turn-failed',other,meta))
    def test_pi_turn_recovered_by_native_retry_accepted(self):
        # M0R4-05: Pi retries a failed request itself; judge the final answer and the retry outcome.
        fs,meta,sent=load_case('pi','1.5.0','rpc-prompt')
        i=next(n for n,f in enumerate(fs) if f.get('type')=='message_end' and f['message'].get('role')=='assistant')
        failed={'type':'message_end','message':{'role':'assistant','content':[],'stopReason':'error','errorMessage':'429'}}
        retried=fs[:i]+[failed,{'type':'auto_retry_start','attempt':1},{'type':'auto_retry_end','success':True}]+fs[i:]
        self.assertEqual(self.check('pi','rpc-prompt',retried,meta,sent),[])
        exhausted=fs[:i]+[failed,{'type':'auto_retry_start','attempt':1},{'type':'auto_retry_end','success':False}]+fs[i:]
        self.assertTrue(any('retry' in p for p in self.check('pi','rpc-prompt',exhausted,meta,sent)))
        unrecovered=fs[:i]+[failed]+fs[i:]
        self.assertTrue(self.check('pi','rpc-prompt',unrecovered,meta,sent))
    def test_other_negative_outcomes(self):
        fs,meta,_=load_case('codex','0.166.0','exec')
        self.assertTrue(self.check('codex','exec',fs+[{'type':'turn.failed','error':{'message':'x'}}],meta))
        self.assertTrue(self.check('codex','exec',fs,meta,rc=1))
        fs,meta,_=load_case('claude','2.1.317','resume')
        self.assertTrue(any('session ids' in p for p in self.check('claude','resume',fs,meta,ctx={'session_id':'another-session'})))
        fs,meta,sent=load_case('codex','0.166.0','app-server-resume')
        self.assertTrue(any('turn/completed' in p for p in self.check('codex','app-server-resume',fs,meta,sent,{'thread_id':'another-thread'})))
        fs,meta,_=load_case('codex','0.166.0','exec-unauthenticated')
        self.assertTrue(self.check('codex','exec-unauthenticated',fs+[{'type':'turn.completed'}],meta))
        fs,meta,sent=load_case('pi','1.5.0','rpc-abort')
        self.assertTrue(self.check('pi','rpc-abort',[f for f in fs if f.get('type')!='agent_settled'],meta,sent))

SLEEPER=r"""import os,signal,sys,time
mode=sys.argv[1]
if mode=='ignore-term':signal.signal(signal.SIGTERM,signal.SIG_IGN)
if mode=='escape':
    if os.fork()==0:
        os.setsid();open(sys.argv[2],'w').write(str(os.getpid()));time.sleep(30);os._exit(0)  # leaves the group, keeps stdout open
    sys.exit(0)
if mode=='exit-on-eof':sys.stdin.read();print('bye');sys.exit(0)
if mode=='count-stdin':print(len(sys.stdin.buffer.read()));sys.exit(0)
if mode=='chatty':
    sys.stderr.write('e'*1_000_000);sys.stderr.flush();print('done');sys.exit(0)
time.sleep(60)
"""

class BoundedStop(unittest.TestCase):
    """M0R3-03: stopping a child never waits without bound."""
    def setUp(self):
        t=tempfile.TemporaryDirectory();self.addCleanup(t.cleanup);self.dir=Path(t.name)
        self.script=self.dir/'child.py';self.script.write_text(SLEEPER)
    def proc(self,mode):
        p=record.Proc([sys.executable,str(self.script),mode,str(self.dir/'escaped.pid')],self.dir,dict(os.environ))
        self.addCleanup(lambda:p.signal(signal.SIGKILL))
        return p
    def timed_stop(self,p,**kw):
        t=time.monotonic();how=p.stop(**kw);return how,time.monotonic()-t
    def test_exits_on_stdin_close(self):
        p=self.proc('exit-on-eof');how,_=self.timed_stop(p,grace=10);self.assertEqual(how,'exited');self.assertEqual(p.out,b'bye\n');self.assertEqual(p.returncode,0)
    def test_term_escalation(self):
        p=self.proc('sleep');how,took=self.timed_stop(p,grace=0.5,term_grace=5);self.assertEqual(how,'SIGTERM');self.assertLess(took,5);self.assertEqual(p.returncode,-signal.SIGTERM)
    def test_kill_when_term_ignored(self):
        p=self.proc('ignore-term');time.sleep(0.3)
        how,took=self.timed_stop(p,grace=0.5,term_grace=0.5,kill_grace=5);self.assertEqual(how,'SIGKILL');self.assertLess(took,5);self.assertEqual(p.returncode,-signal.SIGKILL)
    def test_descendant_outside_group_killed(self):
        # M0R4-07: as a child subreaper the recorder adopts and kills descendants that left the process group.
        driver=self.dir/'driver.py'
        driver.write_text(DRIVER_PREAMBLE+'''
record.enable_subreaper()
p=record.Proc([sys.executable,sys.argv[2],'escape',sys.argv[3]],Path(sys.argv[3]).parent,dict(os.environ))
t=time.monotonic();how=p.stop(grace=0.5,term_grace=0.5,kill_grace=0.5)
pid=int(Path(sys.argv[3]).read_text())
try:os.kill(pid,0);alive=True
except ProcessLookupError:alive=False
print(json.dumps({'how':how,'took':time.monotonic()-t,'alive':alive}))
''')
        r=subprocess.run([sys.executable,str(driver),str(ROOT),str(self.script),str(self.dir/'escaped.pid')],capture_output=True,text=True,timeout=60)
        self.assertEqual(r.returncode,0,r.stderr);res=json.loads(r.stdout)
        self.assertFalse(res['alive'],res);self.assertIn('orphans-killed',res['how']);self.assertLess(res['took'],5)
    def test_unclean_stop_invalidates_case(self):
        t=tempfile.TemporaryDirectory();self.addCleanup(t.cleanup)
        rec=record.Recording('pi','1.5.0',Path(t.name),Path(t.name)/'failed');d=FIXTURES/'pi/1.5.0/rpc-invalid-model'
        rec.save('rpc-invalid-model',['pi'],(d/'stdin.txt').read_bytes(),(d/'stdout.jsonl').read_bytes(),b'',0,'x',stop='SIGKILL+orphans-killed')
        self.assertTrue(any('process cleanup' in p for p in rec.problems['rpc-invalid-model']))
    def test_stderr_drained_while_waiting(self):
        p=self.proc('chatty');how,_=self.timed_stop(p,grace=10);self.assertEqual(how,'exited');self.assertEqual(len(p.err),1_000_000);self.assertEqual(p.out,b'done\n')
    def test_stdin_backpressure_is_bounded(self):
        # M0R4-06: a child that never reads stdin cannot block the recorder on a large input.
        t=time.monotonic();out,err,rc,how=record.run([sys.executable,str(self.script),'sleep','-'],b'x'*(1<<20),self.dir,timeout=0.2)
        self.assertLess(time.monotonic()-t,5);self.assertTrue(how.startswith('SIGTERM'),how)
    def test_large_stdin_fully_delivered(self):
        out,err,rc,how=record.run([sys.executable,str(self.script),'count-stdin','-'],b'y'*(3<<20),self.dir,timeout=30)
        self.assertEqual((out,how,rc),(str(3<<20).encode()+b'\n','exited',0))
    def test_run_reports_timeout_escalation(self):
        out,err,rc,how=record.run([sys.executable,str(self.script),'ignore-term','-'],b'',self.dir,timeout=0.5)
        self.assertTrue(how.startswith('SIGTERM') or how.startswith('SIGKILL'),how)

DRIVER_PREAMBLE='''import importlib.util,json,os,sys,time
from pathlib import Path
spec=importlib.util.spec_from_file_location('record',Path(sys.argv[1])/'tests/fixtures/protocol/record.py')
record=importlib.util.module_from_spec(spec);spec.loader.exec_module(record)
'''

class Interrupted(unittest.TestCase):
    """M0R4-01: SIGINT or SIGTERM to the recorder stops every child before its directories are removed."""
    def interrupt(self,sig):
        t=tempfile.TemporaryDirectory();self.addCleanup(t.cleanup);d=Path(t.name);root=d/'protocol';root.mkdir()
        driver=d/'driver.py';info=d/'child.json'
        driver.write_text(DRIVER_PREAMBLE+'''
root=Path(sys.argv[2]);info=Path(sys.argv[3])
def body(rec):
    p=record.Proc(['sleep','60'],record.workdir())
    info.write_text(json.dumps({'pid':p.p.pid,'cwd':str(record.TEMP_DIRS[-1])}))
    time.sleep(60)
record.AGENTS['pi']=body;record.INSTALLED_VERSION['pi']=lambda:'1.5.0';record.binaries=lambda name:{};record.REPO=root.parent
record.record('pi',root)
''')
        p=subprocess.Popen([sys.executable,str(driver),str(ROOT),str(root),str(info)],stdout=subprocess.PIPE,stderr=subprocess.PIPE)
        self.addCleanup(lambda:p.poll() is None and p.kill())
        deadline=time.monotonic()+30
        while not info.exists() and time.monotonic()<deadline:time.sleep(0.05)
        child=json.loads(info.read_text());p.send_signal(sig);out,err=p.communicate(timeout=60)
        try:os.kill(child['pid'],0);alive=True
        except ProcessLookupError:alive=False
        if alive:os.kill(child['pid'],signal.SIGKILL)
        self.assertFalse(alive,err.decode()[-2000:]);self.assertFalse(Path(child['cwd']).exists())
        self.assertNotEqual(p.returncode,0);self.assertFalse(any((root/'pi').glob('.recording-*')))
    def test_sigint(self):self.interrupt(signal.SIGINT)
    def test_sigterm(self):self.interrupt(signal.SIGTERM)

class StagedPublication(unittest.TestCase):
    """M0R3-02: a version directory is replaced only by a complete, valid recording."""
    def setUp(self):
        t=tempfile.TemporaryDirectory();self.addCleanup(t.cleanup);self.root=Path(t.name)/'protocol';self.root.mkdir()
        self.failed=Path(t.name)/'failed'
        self.published=self.root/'pi/1.5.0';(self.published/'rpc-prompt').mkdir(parents=True)
        (self.published/'rpc-prompt/meta.json').write_text(json.dumps({'valid':True}));(self.published/'old-only.txt').write_text('old')
        self.case=FIXTURES/'pi/1.5.0/rpc-invalid-model'
    def recording(self):return record.Recording('pi','1.5.0',self.root,self.failed)
    def save_invalid_model(self,rec,valid=True):
        d=self.case;out=(d/'stdout.jsonl').read_bytes()
        if not valid:out=out.replace(b'"success":false',b'"success":true');assert b'"success":true' in out
        return rec.save('rpc-invalid-model',['pi'],(d/'stdin.txt').read_bytes(),out,b'',0,'test')
    def test_valid_recording_replaces_whole_directory(self):
        rec=self.recording();self.save_invalid_model(rec);rec.publish({'cli_version':'1.5.0','auth':'X'})
        self.assertEqual(sorted(p.name for p in self.published.iterdir()),['provenance.json','rpc-invalid-model'])
        meta=json.loads((self.published/'rpc-invalid-model/meta.json').read_text());self.assertTrue(meta['valid']);self.assertEqual(meta['problems'],[])
        self.assertEqual([p.name for p in (self.root/'pi').iterdir()],['1.5.0'])
    def test_invalid_case_keeps_published_fixtures(self):
        rec=self.recording();self.save_invalid_model(rec,valid=False)
        with self.assertRaises(record.RecordingFailed):rec.publish({})
        where=rec.abandon()
        self.assertEqual((self.published/'old-only.txt').read_text(),'old');self.assertFalse((self.published/'rpc-invalid-model').exists())
        self.assertFalse(json.loads((where/'rpc-invalid-model/meta.json').read_text())['valid']);self.assertEqual([p.name for p in (self.root/'pi').iterdir()],['1.5.0'])
    def test_blocked_case_keeps_published_fixtures(self):
        rec=self.recording();self.save_invalid_model(rec);rec.fail('auth','BLOCKED_AUTH')
        with self.assertRaises(record.RecordingFailed):rec.publish({})
        rec.abandon();self.assertTrue((self.published/'old-only.txt').exists())
    def test_record_failure_leaves_fixtures_and_manifest_entry(self):
        (self.root/'compat-manifest.json').write_text(json.dumps({'agents':{'pi':{'1.5.0':{'cli_version':'1.5.0','auth':'OLD','help_sha256':'h','entry_sha256':'e'}}}}))
        def body(rec):self.save_invalid_model(rec,valid=False)
        with patch.dict(record.AGENTS,{'pi':body}),patch.dict(record.INSTALLED_VERSION,{'pi':lambda:'1.5.0'}),patch.object(record,'binaries',lambda name:{'entry_sha256':'NEW'}),\
             patch.object(record,'REPO',self.root.parent):
            with self.assertRaises(record.RecordingFailed) as cm:record.record('pi',self.root)
        self.assertIn('published fixtures unchanged',str(cm.exception));self.assertTrue((self.published/'old-only.txt').exists())
        record.write_manifest(self.root)
        entry=json.loads((self.root/'compat-manifest.json').read_text())['agents']['pi']['1.5.0']
        self.assertEqual((entry['auth'],entry['entry_sha256']),('OLD','e'))
    def test_binary_change_during_recording_fails(self):
        hashes=iter([{'entry_sha256':'A'},{'entry_sha256':'B'}])
        def body(rec):self.save_invalid_model(rec);(rec.dir/'help.txt').write_bytes(b'help')
        with patch.dict(record.AGENTS,{'pi':body}),patch.dict(record.INSTALLED_VERSION,{'pi':lambda:'1.5.0'}),patch.object(record,'binaries',lambda name:next(hashes)),\
             patch.object(record,'REPO',self.root.parent):
            with self.assertRaises(record.RecordingFailed) as cm:record.record('pi',self.root)
        self.assertIn('binary changed',str(cm.exception));self.assertTrue((self.published/'old-only.txt').exists())
    def test_successful_record_writes_provenance_from_recording_time(self):
        def body(rec):self.save_invalid_model(rec);(rec.dir/'help.txt').write_bytes(b'help');rec.auth='AUTHENTICATED (live turn recorded)'
        with patch.dict(record.AGENTS,{'pi':body}),patch.dict(record.INSTALLED_VERSION,{'pi':lambda:'1.5.0'}),patch.object(record,'binaries',lambda name:{'entry_sha256':'A'}):
            record.record('pi',self.root)
        with patch.object(record,'binaries',side_effect=AssertionError('manifest must not re-measure binaries')):record.write_manifest(self.root)
        entry=json.loads((self.root/'compat-manifest.json').read_text())['agents']['pi']['1.5.0']
        self.assertEqual((entry['entry_sha256'],entry['help_sha256'],entry['auth']),('A',record.sha256(b'help'),'AUTHENTICATED (live turn recorded)'))
        self.assertEqual(list(entry['cases']),['rpc-invalid-model'])
    def test_record_removes_its_temporary_directories(self):
        made=[]
        def body(rec):made.append(record.workdir());made.append(record.tempdir('hoh-codex-home-'));rec.fail('auth','BLOCKED_AUTH')
        with patch.dict(record.AGENTS,{'pi':body}),patch.dict(record.INSTALLED_VERSION,{'pi':lambda:'1.5.0'}),patch.object(record,'binaries',lambda name:{}),\
             patch.object(record,'REPO',self.root.parent):
            with self.assertRaises(record.RecordingFailed):record.record('pi',self.root)
        self.assertEqual(len(made),2);self.assertFalse(any(d.exists() for d in made));self.assertEqual(record.TEMP_DIRS,[])
    def test_recover_after_kill_mid_publish(self):
        parent=self.root/'pi';self.published.rename(parent/'.replaced-1.5.0-123');(parent/'.recording-1.5.0-abc').mkdir()
        record.recover(parent);self.assertEqual([p.name for p in parent.iterdir()],['1.5.0']);self.assertTrue((self.published/'old-only.txt').exists())
        (parent/'.replaced-1.5.0-456').mkdir();record.recover(parent);self.assertEqual([p.name for p in parent.iterdir()],['1.5.0'])
    def test_manifest_recovers_interrupted_publish(self):
        # M0R4-02: a publish interrupted between its renames must not drop the version from the manifest.
        (self.published/'provenance.json').write_text(json.dumps({'cli_version':'1.5.0','auth':'A'}))
        self.published.rename(self.root/'pi/.replaced-1.5.0-999');record.write_manifest(self.root)
        self.assertIn('1.5.0',json.loads((self.root/'compat-manifest.json').read_text())['agents']['pi']);self.assertTrue(self.published.is_dir())
    def test_publish_exchanges_directories(self):
        a=self.root/'a';b=self.root/'b';a.mkdir();b.mkdir();(a/'x').write_text('a');(b/'x').write_text('b')
        record.exchange(a,b);self.assertEqual(((a/'x').read_text(),(b/'x').read_text()),('b','a'))
    def test_corrupt_previous_manifest_is_rebuilt(self):
        # M0R4-03: a truncated manifest from an interrupted write does not block rebuilding it.
        (self.published/'provenance.json').write_text(json.dumps({'cli_version':'1.5.0','auth':'A','entry_sha256':'e'}))
        (self.root/'compat-manifest.json').write_text('{"agents": {"pi": {')
        record.write_manifest(self.root)
        self.assertEqual(json.loads((self.root/'compat-manifest.json').read_text())['agents']['pi']['1.5.0']['entry_sha256'],'e')
        self.assertEqual(sorted(p.name for p in self.root.iterdir()),['compat-manifest.json','pi'])
    def test_manifest_ignores_staging_directories(self):
        (self.root/'pi/.recording-1.5.0-x/rpc-prompt').mkdir(parents=True);record.write_manifest(self.root)
        self.assertEqual(list(json.loads((self.root/'compat-manifest.json').read_text())['agents']['pi']),['1.5.0'])

if __name__=='__main__':unittest.main()
