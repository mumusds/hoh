"""Protocol fixture recorder (M0-T06): case validators, bounded process stop, staged publication, manifest provenance."""
import copy,importlib.util,json,os,shutil,signal,sys,tempfile,time,unittest
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
    def test_descendant_outside_group_abandoned(self):
        p=self.proc('escape');how,took=self.timed_stop(p,grace=0.5,term_grace=0.5,kill_grace=0.5)
        self.assertEqual(how,'SIGKILL+abandoned');self.assertLess(took,5);self.assertEqual(p.returncode,0)
        os.kill(int((self.dir/'escaped.pid').read_text()),signal.SIGKILL)
    def test_stderr_drained_while_waiting(self):
        p=self.proc('chatty');how,_=self.timed_stop(p,grace=10);self.assertEqual(how,'exited');self.assertEqual(len(p.err),1_000_000);self.assertEqual(p.out,b'done\n')
    def test_run_reports_timeout_escalation(self):
        out,err,rc,how=record.run([sys.executable,str(self.script),'ignore-term','-'],b'',self.dir,timeout=0.5)
        self.assertTrue(how.startswith('SIGTERM') or how.startswith('SIGKILL'),how)

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
    def test_manifest_ignores_staging_directories(self):
        (self.root/'pi/.recording-1.5.0-x/rpc-prompt').mkdir(parents=True);record.write_manifest(self.root)
        self.assertEqual(list(json.loads((self.root/'compat-manifest.json').read_text())['agents']['pi']),['1.5.0'])

if __name__=='__main__':unittest.main()
