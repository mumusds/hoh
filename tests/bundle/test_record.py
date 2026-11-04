"""Protocol fixture recorder (M0-T06): case validators, bounded process stop, staged publication, manifest provenance."""
import copy,importlib.util,json,os,shutil,signal,subprocess,sys,tempfile,time,unittest
from pathlib import Path
from unittest.mock import patch
from helpers import ROOT

spec=importlib.util.spec_from_file_location('record',ROOT/'tests/fixtures/protocol/record.py')
record=importlib.util.module_from_spec(spec);spec.loader.exec_module(record)
FIXTURES=ROOT/'tests/fixtures/protocol'
# Children run in transient systemd user scopes (cgroups). Outside CI a machine without a user manager skips the
# process tests; in CI (where the workflow starts one) they must run, so a missing manager fails them instead.
SCOPES=record.scopes_available()
needs_scopes=unittest.skipUnless(SCOPES is None or os.environ.get('CI'),f'systemd user scopes unavailable: {SCOPES}')

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

SLEEPER=r"""import json,os,signal,sys,time
mode=sys.argv[1]
if mode=='ignore-term':signal.signal(signal.SIGTERM,signal.SIG_IGN)
if mode=='escape':
    if os.fork()==0:
        os.setsid();open(sys.argv[2],'w').write(str(os.getpid()));time.sleep(30);os._exit(0)  # leaves the group, keeps stdout open
    sys.exit(0)
if mode=='exit-on-eof':sys.stdin.read();print('bye');sys.exit(0)
if mode=='count-stdin':print(len(sys.stdin.buffer.read()));sys.exit(0)
if mode=='close-then-read':
    os.close(1);os.close(2);n=len(sys.stdin.buffer.read());open(sys.argv[2],'w').write(str(n));sys.exit(0)
if mode=='linger':
    if os.fork()==0:
        os.setsid();fd=os.open(os.devnull,os.O_RDWR)
        for n in (0,1,2):os.dup2(fd,n)
        time.sleep(1);os._exit(0)  # like the last `sleep 2` of a cancelled tool: ends on its own shortly
    sys.exit(0)
if mode=='relay':
    if os.fork()==0:
        os.setsid();fd=os.open(os.devnull,os.O_RDWR)
        for n in (0,1,2):os.dup2(fd,n)
        time.sleep(0.5)
        if os.fork()==0:time.sleep(1.0);os._exit(0)  # B, born during the grace period, ends on its own
        os._exit(0)  # A hands over to B
    sys.exit(0)
if mode=='hidden':
    if os.fork()==0:
        import ctypes;ctypes.CDLL(None).prctl(4,0,0,0,0)  # PR_SET_DUMPABLE=0: /proc/<pid>/environ unreadable
        os.setsid();fd=os.open(os.devnull,os.O_RDWR)
        for n in (0,1,2):os.dup2(fd,n)
        open(sys.argv[2],'w').write(str(os.getpid()));time.sleep(30);os._exit(0)
    time.sleep(0.3);sys.exit(0)
if mode=='dispositions':
    print(json.dumps({s:str(signal.getsignal(getattr(signal,s))) for s in ('SIGINT','SIGTERM','SIGHUP')}));sys.exit(0)
if mode=='spawner':
    if os.fork()==0:
        os.setsid();fd=os.open(os.devnull,os.O_RDWR)
        for n in (0,1,2):os.dup2(fd,n)
        end=time.time()+10
        while time.time()<end:  # keeps forking new descendants, also while it is being killed
            if os.fork()==0:open(os.path.join(sys.argv[2],str(os.getpid())),'w').close();time.sleep(30);os._exit(0)
            time.sleep(0.02)
        os._exit(0)
    time.sleep(0.3);sys.exit(0)
if mode=='relay-fast':
    if os.fork()==0:
        os.setsid();fd=os.open(os.devnull,os.O_RDWR)
        for n in (0,1,2):os.dup2(fd,n)
        if os.fork()==0:time.sleep(0.8);os._exit(0)  # B takes over at once
        os._exit(0)
    sys.exit(0)
if mode=='version-probe':
    if os.fork()==0:
        os.setsid();fd=os.open(os.devnull,os.O_RDWR)
        for n in (0,1,2):os.dup2(fd,n)
        open(sys.argv[2],'w').write(str(os.getpid()));time.sleep(30);os._exit(0)
    print('9.9.9');sys.stdout.flush();sys.exit(0) if len(sys.argv)<4 else time.sleep(30)
if mode=='subcgroup':
    own='/sys/fs/cgroup'+open('/proc/self/cgroup').read().strip().rsplit(':',1)[-1]
    worker=os.path.join(own,'worker');os.mkdir(worker)
    if os.fork()==0:
        open(os.path.join(worker,'cgroup.procs'),'w').write(str(os.getpid()))  # into a child cgroup of the scope
        fd=os.open(os.devnull,os.O_RDWR)
        for n in (0,1,2):os.dup2(fd,n)
        open(sys.argv[2],'w').write(str(os.getpid()));time.sleep(30);os._exit(0)
    time.sleep(0.3);sys.exit(0)
if mode=='unlistable':
    own='/sys/fs/cgroup'+open('/proc/self/cgroup').read().strip().rsplit(':',1)[-1]
    if os.fork()==0:
        go=sys.argv[2]+'.go'
        while not os.path.exists(go):time.sleep(0.01)  # only once the recorder has finished building the Proc
        os.chmod(own,0o111)  # the scope directory can still be traversed but no longer listed
        fd=os.open(os.devnull,os.O_RDWR)
        for n in (0,1,2):os.dup2(fd,n)
        open(sys.argv[2],'w').write(str(os.getpid()));time.sleep(30);os._exit(0)
    time.sleep(0.3);sys.exit(0)
if mode=='escape-quiet':
    if os.fork()==0:
        os.setsid();fd=os.open(os.devnull,os.O_RDWR)
        for n in (0,1,2):os.dup2(fd,n)
        open(sys.argv[2],'w').write(str(os.getpid()));time.sleep(30);os._exit(0)  # leaves the group and closes its stdio
    time.sleep(0.3);sys.exit(0)
if mode=='chatty':
    sys.stderr.write('e'*1_000_000);sys.stderr.flush();print('done');sys.exit(0)
time.sleep(60)
"""

@needs_scopes
class BoundedStop(unittest.TestCase):
    """M0R3-03: stopping a child never waits without bound."""
    def setUp(self):
        t=tempfile.TemporaryDirectory();self.addCleanup(t.cleanup);self.dir=Path(t.name)
        self.addCleanup(record.stop_all)  # also covers a Proc whose construction raised (M0R13-01); runs before the cleanup above
        self.script=self.dir/'child.py';self.script.write_text(SLEEPER)
    def proc(self,mode):
        p=record.Proc([sys.executable,str(self.script),mode,str(self.dir/'escaped.pid')],self.dir,dict(os.environ))
        self.addCleanup(lambda:p.signal(signal.SIGKILL))
        self.addCleanup(lambda:(self.dir/'escaped.pid').exists() and record.alive(int((self.dir/'escaped.pid').read_text())) and os.kill(int((self.dir/'escaped.pid').read_text()),signal.SIGKILL))
        return p
    def timed_stop(self,p,**kw):
        t=time.monotonic();how=p.stop(**kw);return how,time.monotonic()-t
    def test_child_that_exits_right_after_entering_its_scope(self):
        # Live run: `git init` entered its scope and exited before a second read of cgroup.procs; that is not a failure.
        real=record.members;seen={'entered':False}
        def once(cg):
            got=real(cg)
            if seen['entered']:return []
            if got:seen['entered']=True
            return got
        with patch.object(record,'members',side_effect=once):p=self.proc('sleep')
        self.assertTrue(seen['entered']);p.terminate()
    def test_exits_on_stdin_close(self):
        p=self.proc('exit-on-eof');how,_=self.timed_stop(p,grace=10);self.assertEqual(how,'exited');self.assertEqual(p.out,b'bye\n');self.assertEqual(p.returncode,0)
    def test_term_escalation(self):
        p=self.proc('sleep');how,took=self.timed_stop(p,grace=0.5,term_grace=5);self.assertEqual(how,'SIGTERM');self.assertLess(took,5);self.assertEqual(p.returncode,-signal.SIGTERM)
    def test_kill_when_term_ignored(self):
        p=self.proc('ignore-term');time.sleep(0.3)
        how,took=self.timed_stop(p,grace=0.5,term_grace=0.5,kill_grace=5);self.assertEqual(how,'SIGKILL');self.assertLess(took,5);self.assertEqual(p.returncode,-signal.SIGKILL)
    def escaped_alive(self)->bool:
        pid=int((self.dir/'escaped.pid').read_text())
        return record.alive(pid)
    def test_descendant_outside_group_killed(self):
        # M0R4-07: descendants that left the process group and hold a pipe are found by the Proc's token and killed.
        p=self.proc('escape');how,took=self.timed_stop(p,grace=0.5,term_grace=0.5,kill_grace=2)
        self.assertIn('descendants-outlived',how);self.assertLess(took,6);self.assertFalse(self.escaped_alive())
    def test_quiet_escaped_descendant_killed_after_clean_exit(self):
        # M0R5-02: the child exits normally but leaves a setsid descendant with closed stdio.
        p=self.proc('escape-quiet');how,_=self.timed_stop(p,grace=5)
        self.assertEqual(how,'exited+descendants-outlived');self.assertFalse(self.escaped_alive())
    def test_descendant_ending_on_its_own_is_only_recorded(self):
        # A descendant that exits within the grace period by itself is reported, not killed and not a failure.
        p=self.proc('linger');how=p.stop(grace=5)
        self.assertEqual(how,'exited');self.assertEqual(len(how.lingered),1);self.assertEqual(how.descendants,[])
        t=tempfile.TemporaryDirectory();self.addCleanup(t.cleanup)
        rec=record.Recording('pi','1.5.0',Path(t.name),Path(t.name)/'failed');d=FIXTURES/'pi/1.5.0/rpc-invalid-model'
        rec.save('rpc-invalid-model',['pi'],(d/'stdin.txt').read_bytes(),(d/'stdout.jsonl').read_bytes(),b'',0,'x',stop=how)
        self.assertEqual(rec.problems,{});meta=json.loads((rec.dir/'rpc-invalid-model/meta.json').read_text())
        self.assertEqual(len(meta['descendants_lingered']),1)
    def test_grace_covers_descendants_born_during_it(self):
        # M0R6-03: a tree that hands over to a new descendant and ends within the grace period is not killed.
        p=self.proc('relay');how=p.stop(grace=5,descendant_grace=5)
        self.assertEqual(how,'exited');self.assertEqual(how.descendants,[]);self.assertGreaterEqual(len(how.lingered),2)
    def test_nondumpable_descendant_killed(self):
        # M0R6-02: membership of the child's cgroup does not depend on reading /proc/<pid>/environ.
        p=self.proc('hidden');how=p.stop(grace=5,descendant_grace=1)
        self.assertEqual(how,'exited+descendants-outlived');self.assertFalse(self.escaped_alive())
        t=tempfile.TemporaryDirectory();self.addCleanup(t.cleanup)
        rec=record.Recording('pi','1.5.0',Path(t.name),Path(t.name)/'failed');d=FIXTURES/'pi/1.5.0/rpc-invalid-model'
        rec.save('rpc-invalid-model',['pi'],(d/'stdin.txt').read_bytes(),(d/'stdout.jsonl').read_bytes(),b'',0,'x',stop=how)
        self.assertTrue(any('process cleanup' in p for p in rec.problems['rpc-invalid-model']))
    def test_descendants_forked_during_the_kill_are_killed(self):
        # M0R7-01: a tree that keeps forking cannot outrun the cleanup (cgroup.kill is atomic for the whole cgroup).
        spawned=self.dir/'spawned';spawned.mkdir()
        p=record.Proc([sys.executable,str(self.script),'spawner',str(spawned)],self.dir,dict(os.environ))
        how=p.stop(grace=5,descendant_grace=0.5);time.sleep(0.2)
        pids=[int(x.name) for x in spawned.iterdir()];self.assertGreater(len(pids),3)
        self.assertEqual(how,'exited+descendants-outlived');self.assertEqual([x for x in pids if record.alive(x)],[])
        self.assertFalse(record.populated(p.cgroup));self.assertNotIn(p,record.LIVE)
    def test_fast_handover_within_grace_is_not_killed(self):
        # M0R7-03: a descendant that hands over to a new one at once still counts as one tree ending on its own.
        p=self.proc('relay-fast');how=p.stop(grace=5,descendant_grace=3)
        self.assertEqual(how,'exited');self.assertEqual(how.descendants,[])
    def test_outlived_is_what_the_deadline_snapshot_lists(self):
        # M0R10-02/03: the verdict is one observation at the end of the grace period, never an attribution of who
        # ended a process. The kernel's members of the scope at that moment are the descendants that outlived it.
        real=record.members;snaps=[]
        def spy(cg):
            got=real(cg);snaps.append(list(got));return got
        p=self.proc('escape-quiet')
        with patch.object(record,'members',side_effect=spy):how=p.stop(grace=5,descendant_grace=0.2)
        self.assertEqual(how,'exited+descendants-outlived');self.assertEqual(len(how.descendants),1);self.assertFalse(self.escaped_alive())
        self.assertTrue(any(snaps))
    def test_populated_flag_alone_does_not_mark_outlived(self):
        # M0R10-03: a scope that reads populated but lists no descendant at the deadline (the last one just exited)
        # is cleaned up without a verdict against the case.
        p=self.proc('exit-on-eof');real=record.populated;calls={'n':0};until=[None]
        def stale(cg):  # reads populated for 1.5 s after the deadline, then the truth
            calls['n']+=1
            if until[0] is None:until[0]=time.monotonic()+1.5
            return True if time.monotonic()<until[0] else real(cg)
        with patch.object(record,'populated',side_effect=stale),patch.object(record,'members',side_effect=lambda cg:[]):
            how=p.stop(grace=5,descendant_grace=0)
        self.assertEqual(how,'exited');self.assertEqual(how.descendants,[]);self.assertGreaterEqual(calls['n'],2)
    def test_backstop_without_cgroup_kill_kills_members(self):
        # M0R9-01 / M0R10-01: cgroup.kill is written and fails, so the per-member fallback must run and empty the scope.
        real_write=record.write_control;real_kill=record.kill_pid;used={'cgroup.kill':0,'kill_pid':0}
        def no_kill(cg,name,value):
            if name=='cgroup.kill':used['cgroup.kill']+=1;raise FileNotFoundError(2,'No such file or directory')
            return real_write(cg,name,value)
        def spy_kill(pid):
            used['kill_pid']+=1;return real_kill(pid)
        p=self.proc('escape-quiet')
        with patch.object(record,'write_control',side_effect=no_kill),patch.object(record,'kill_pid',side_effect=spy_kill):
            how=p.stop(grace=5,descendant_grace=0.2)
        self.assertGreaterEqual(used['cgroup.kill'],1);self.assertGreaterEqual(used['kill_pid'],1)
        self.assertEqual(how,'exited+descendants-outlived');self.assertFalse(self.escaped_alive())
        self.assertFalse(record.populated(p.cgroup));self.assertNotIn(p,record.LIVE)
    def test_unkillable_scope_keeps_the_child_registered(self):
        def no_kill(cg,name,value):raise FileNotFoundError(2,'No such file or directory')
        p=self.proc('escape-quiet')
        with patch.object(record,'write_control',side_effect=no_kill),patch.object(record,'kill_pid',return_value=False),\
             patch.object(record,'CONFIRM_SECONDS',0.5):
            with self.assertRaises(RuntimeError):p.stop(grace=5,descendant_grace=0.2,kill_grace=0.5)
        self.assertIn(p,record.LIVE);self.assertTrue(self.escaped_alive());p.terminate();self.assertFalse(self.escaped_alive())
    def test_terminate_survives_cleanup_errors(self):
        # M0R7-02: an error inside terminate() must not skip the kill, the wait or the confirmation.
        p=self.proc('ignore-term');time.sleep(0.3)
        with patch.object(p,'drain',side_effect=OSError(5,'Input/output error')):p.terminate()
        self.assertIsNotNone(p.p.returncode);self.assertFalse(record.populated(p.cgroup));self.assertNotIn(p,record.LIVE)
    def test_unconfirmed_child_stays_registered(self):
        p=self.proc('ignore-term');time.sleep(0.3)
        with patch.object(p,'kill_tree'),patch.object(p,'signal'),patch.object(record,'CONFIRM_SECONDS',0.5):
            with self.assertRaises(RuntimeError):p.terminate()
        self.assertIn(p,record.LIVE);p.terminate();self.assertNotIn(p,record.LIVE)
    def test_child_gets_default_signal_dispositions(self):
        # A recorder started with SIGINT ignored (background job) must not pass that on: Claude's cancel needs SIGINT.
        old=signal.signal(signal.SIGINT,signal.SIG_IGN);self.addCleanup(signal.signal,signal.SIGINT,old)
        out,err,rc,how=record.run([sys.executable,str(self.script),'dispositions','-'],b'',self.dir,timeout=10)
        self.assertEqual(json.loads(out),{'SIGINT':str(signal.default_int_handler),'SIGTERM':str(signal.SIG_DFL),'SIGHUP':str(signal.SIG_DFL)})
    def test_descendant_in_a_child_cgroup_outlives(self):
        # M0R11-01: the deadline observation covers the whole scope subtree, like cgroup.events and cgroup.kill.
        p=self.proc('subcgroup');how=p.stop(grace=5,descendant_grace=0.3)
        self.assertEqual(how,'exited+descendants-outlived');self.assertEqual(len(how.descendants),1);self.assertFalse(self.escaped_alive())
        self.assertFalse(record.populated(p.cgroup))
    def test_unlistable_scope_fails_instead_of_looking_empty(self):
        # M0R12-01: an error listing the scope tree is not an empty member list; the stop fails, the scope is still cleaned.
        p=self.proc('unlistable');(self.dir/'escaped.pid.go').touch()
        deadline=time.monotonic()+10
        while time.monotonic()<deadline and os.stat(p.cgroup).st_mode&0o777!=0o111:time.sleep(0.01)
        with self.assertRaises(PermissionError):p.stop(grace=5,descendant_grace=0.3)
        self.assertFalse(self.escaped_alive());self.assertFalse(record.populated(p.cgroup));self.assertNotIn(p,record.LIVE)
    def test_unrelated_children_survive(self):
        # M0R5-06: only processes started by the stopped Proc (carrying its token) are killed.
        other=subprocess.Popen(['sleep','30']);self.addCleanup(lambda:(other.kill(),other.wait()))
        p=self.proc('escape-quiet');p.stop(grace=5);record.stop_all()
        self.assertIsNone(other.poll())
    def test_output_closed_input_still_delivered(self):
        # M0R5-04: pending stdin is still written after the child closed stdout and stderr.
        p=record.Proc([sys.executable,str(self.script),'close-then-read',str(self.dir/'n')],self.dir,dict(os.environ))
        p.write(b'z'*(3<<20));how=p.stop(grace=10)
        self.assertEqual((how,(self.dir/'n').read_text()),('exited',str(3<<20)))
    def test_stop_all_survives_a_failing_terminate(self):
        a=self.proc('sleep');b=self.proc('sleep')
        with patch.object(a,'terminate',side_effect=RuntimeError('boom')):record.stop_all()
        self.assertIsNotNone(b.p.poll());self.assertIn(a,record.LIVE);a.terminate()
    def test_unclean_stop_invalidates_case(self):
        t=tempfile.TemporaryDirectory();self.addCleanup(t.cleanup)
        rec=record.Recording('pi','1.5.0',Path(t.name),Path(t.name)/'failed');d=FIXTURES/'pi/1.5.0/rpc-invalid-model'
        rec.save('rpc-invalid-model',['pi'],(d/'stdin.txt').read_bytes(),(d/'stdout.jsonl').read_bytes(),b'',0,'x',stop='exited+descendants-outlived')
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
# argv[1] is a sandbox repository (see sandbox()), so record.HERE and every default path are temporary.
spec=importlib.util.spec_from_file_location('record',Path(sys.argv[1])/'tests/fixtures/protocol/record.py')
record=importlib.util.module_from_spec(spec);spec.loader.exec_module(record)
'''

def sandbox(d:Path)->Path:
    """A throw-away repository layout holding a copy of record.py: drivers can never touch the real fixtures,
    whichever version of the recorder they run."""
    repo=d/'repo';(repo/'tests/fixtures/protocol').mkdir(parents=True)
    shutil.copy(FIXTURES/'record.py',repo/'tests/fixtures/protocol/record.py');(repo/'tools').symlink_to(ROOT/'tools')
    return repo

@needs_scopes
class Interrupted(unittest.TestCase):
    """M0R4-01: SIGINT or SIGTERM to the recorder stops every child before its directories are removed."""
    def interrupt(self,sig):
        t=tempfile.TemporaryDirectory();self.addCleanup(t.cleanup);d=Path(t.name);repo=sandbox(d);root=repo/'tests/fixtures/protocol'
        driver=d/'driver.py';info=d/'child.json'
        driver.write_text(DRIVER_PREAMBLE+'''
root=Path(sys.argv[2]);info=Path(sys.argv[3])
def body(rec):
    p=record.Proc(['sleep','60'],record.workdir())
    info.write_text(json.dumps({'pid':p.p.pid,'cwd':str(record.TEMP_DIRS[-1])}))
    time.sleep(60)
record.AGENTS['pi']=body;record.INSTALLED_VERSION['pi']=lambda:'1.5.0';record.binaries=lambda name:{}
record.record('pi',root)
''')
        p=subprocess.Popen([sys.executable,str(driver),str(repo),str(root),str(info)],stdout=subprocess.PIPE,stderr=subprocess.PIPE)
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
    def stop_interrupted(self,sig):
        # M0R6-01: a signal while stop() waits must still end with the child terminated before its cwd goes.
        t=tempfile.TemporaryDirectory();self.addCleanup(t.cleanup);d=Path(t.name);repo=sandbox(d);root=repo/'tests/fixtures/protocol';info=d/'child.json'
        driver=d/'driver.py';driver.write_text(DRIVER_PREAMBLE+'''
root=Path(sys.argv[2]);info=Path(sys.argv[3])
def body(rec):
    cwd=record.workdir();p=record.Proc(['sleep','60'],cwd)
    info.write_text(json.dumps({'pid':p.p.pid,'cwd':str(cwd)}))
    p.stop(grace=30)
record.AGENTS['pi']=body;record.INSTALLED_VERSION['pi']=lambda:'1.5.0';record.binaries=lambda name:{}
record.record('pi',root)
''')
        p=subprocess.Popen([sys.executable,str(driver),str(repo),str(root),str(info)],stdout=subprocess.PIPE,stderr=subprocess.PIPE)
        self.addCleanup(lambda:p.poll() is None and p.kill())
        deadline=time.monotonic()+30
        while not info.exists() and time.monotonic()<deadline:time.sleep(0.05)
        time.sleep(0.5);child=json.loads(info.read_text());p.send_signal(sig);out,err=p.communicate(timeout=60)
        still=record.alive(child['pid'])
        if still:os.kill(child['pid'],signal.SIGKILL)
        self.assertFalse(still,err.decode()[-2000:]);self.assertFalse(Path(child['cwd']).exists())
    def test_sigint_during_stop(self):self.stop_interrupted(signal.SIGINT)
    def test_sigterm_during_stop(self):self.stop_interrupted(signal.SIGTERM)
    def test_sigterm_between_agents_still_rebuilds_manifest(self):
        # M0R6-04: SIGTERM while the next agent's version is probed must still rebuild the manifest.
        t=tempfile.TemporaryDirectory();self.addCleanup(t.cleanup);d=Path(t.name);repo=sandbox(d);root=repo/'tests/fixtures/protocol'
        driver=d/'driver.py';driver.write_text(DRIVER_PREAMBLE+'''
import signal
root=Path(sys.argv[2]);src=Path(sys.argv[3])/'tests/fixtures/protocol/claude/2.1.317/max-turns-error'
def claude(rec):
    rec.save('max-turns-error',['claude'],(src/'stdin.txt').read_bytes(),(src/'stdout.jsonl').read_bytes(),b'',1,'x');(rec.dir/'help.txt').write_bytes(b'h')
def pi_version():
    os.kill(os.getpid(),signal.SIGTERM);time.sleep(5);return '1.5.0'
record.AGENTS.update(claude=claude);record.INSTALLED_VERSION.update(claude=lambda:'2.1.317',pi=pi_version)
record.binaries=lambda name:{};sys.argv=['record.py','--agents','claude,pi']
sys.exit(record.main())
''')
        r=subprocess.run([sys.executable,str(driver),str(repo),str(root),str(ROOT)],capture_output=True,text=True,timeout=60)
        self.assertEqual(r.returncode,128+signal.SIGTERM,r.stderr)
        entry=json.loads((root/'compat-manifest.json').read_text())['agents']['claude']['2.1.317']
        published=root/'claude/2.1.317/max-turns-error/stdout.jsonl'
        self.assertEqual(entry['cases']['max-turns-error']['files']['stdout.jsonl'],record.sha256(published.read_bytes()))
    def test_sigterm_during_version_probe_kills_its_descendants(self):
        # M0R7-04: version/help/schema/auth probes run in scopes too, so an interrupt kills their whole tree.
        t=tempfile.TemporaryDirectory();self.addCleanup(t.cleanup);d=Path(t.name);repo=sandbox(d);root=repo/'tests/fixtures/protocol'
        pidfile=d/'probe.pid';script=d/'child.py';script.write_text(SLEEPER)
        driver=d/'driver.py';driver.write_text(DRIVER_PREAMBLE+'''
probe=[sys.executable,sys.argv[3],'version-probe',sys.argv[4],'block']
record.INSTALLED_VERSION['pi']=lambda:record.first_line(probe)
sys.argv=['record.py','--agents','pi'];sys.exit(record.main())
''')
        p=subprocess.Popen([sys.executable,str(driver),str(repo),str(root),str(script),str(pidfile)],stdout=subprocess.PIPE,stderr=subprocess.PIPE)
        self.addCleanup(lambda:p.poll() is None and p.kill())
        deadline=time.monotonic()+30
        while not pidfile.exists() and time.monotonic()<deadline:time.sleep(0.05)
        time.sleep(0.3);pid=int(pidfile.read_text());p.send_signal(signal.SIGTERM);out,err=p.communicate(timeout=60)
        still=record.alive(pid)
        if still:os.kill(pid,signal.SIGKILL)
        self.assertEqual(p.returncode,128+signal.SIGTERM,err.decode()[-2000:]);self.assertFalse(still)
    def test_sigterm_while_constructing_the_version_probe(self):
        # M0R8-01: the version probe runs before the recording; a signal while its Proc is built must still stop it.
        t=tempfile.TemporaryDirectory();self.addCleanup(t.cleanup);d=Path(t.name);repo=sandbox(d);root=repo/'tests/fixtures/protocol'
        pidfile=d/'probe.pid';script=d/'child.py';script.write_text(SLEEPER)
        driver=d/'driver.py';driver.write_text(DRIVER_PREAMBLE+'''
import signal
probe=[sys.executable,sys.argv[3],'version-probe',sys.argv[4],'block'];real=os.set_blocking
def set_blocking(fd,flag):
    os.kill(os.getpid(),signal.SIGTERM);real(fd,flag)
def version():
    os.set_blocking=set_blocking
    try:return record.first_line(probe)
    finally:os.set_blocking=real
record.INSTALLED_VERSION['pi']=version
sys.argv=['record.py','--agents','pi'];sys.exit(record.main())
''')
        p=subprocess.run([sys.executable,str(driver),str(repo),str(root),str(script),str(pidfile)],capture_output=True,text=True,timeout=90)
        # Only scopes running this test's own child count (and are stopped): concurrent bundle runs have their own.
        scopes=subprocess.run(['systemctl','--user','list-units','hoh-recorder-*','--no-legend','--plain','--full'],capture_output=True,text=True).stdout
        mine=[line for line in scopes.splitlines() if str(script) in line]
        leaked=pidfile.exists() and record.alive(int(pidfile.read_text()))
        if leaked:os.kill(int(pidfile.read_text()),signal.SIGKILL)
        for line in mine:subprocess.run(['systemctl','--user','stop',line.split()[0]],capture_output=True)
        self.assertEqual(p.returncode,128+signal.SIGTERM,p.stderr[-2000:]);self.assertEqual(mine,[]);self.assertFalse(leaked)
    def test_version_probe_leaves_nothing_behind(self):
        t=tempfile.TemporaryDirectory();self.addCleanup(t.cleanup);d=Path(t.name);script=d/'child.py';script.write_text(SLEEPER)
        self.assertEqual(record.first_line([sys.executable,str(script),'version-probe',str(d/'probe.pid')]),'9.9.9')
        pid=int((d/'probe.pid').read_text());still=record.alive(pid)
        if still:os.kill(pid,signal.SIGKILL)
        self.assertFalse(still)
    def test_signal_while_constructing_a_child(self):
        # M0R5-01: SIGTERM arriving inside Proc() is handled only once the child is registered, so it is still stopped.
        t=tempfile.TemporaryDirectory();self.addCleanup(t.cleanup);d=Path(t.name);repo=sandbox(d);root=repo/'tests/fixtures/protocol';info=d/'child.json'
        driver=d/'driver.py';driver.write_text(DRIVER_PREAMBLE+'''
import signal
root=Path(sys.argv[2]);info=Path(sys.argv[3]);real=os.set_blocking
def set_blocking(fd,flag):
    os.kill(os.getpid(),signal.SIGTERM);real(fd,flag)
def body(rec):
    cwd=record.workdir();os.set_blocking=set_blocking
    try:record.Proc(['sleep','60'],cwd)
    finally:
        os.set_blocking=real
        info.write_text(json.dumps({'pids':[p.p.pid for p in record.LIVE],'cwd':str(cwd)}))
record.AGENTS['pi']=body;record.INSTALLED_VERSION['pi']=lambda:'1.5.0';record.binaries=lambda name:{}
record.record('pi',root)
''')
        r=subprocess.run([sys.executable,str(driver),str(repo),str(root),str(info)],capture_output=True,text=True,timeout=60)
        child=json.loads(info.read_text());self.assertEqual(len(child['pids']),1,r.stderr)
        self.assertFalse(record.alive(child['pids'][0]),r.stderr[-2000:]);self.assertFalse(Path(child['cwd']).exists())
        self.assertIn('Terminated',r.stderr);self.assertFalse(any((root/'pi').glob('.recording-*')))
    def test_interrupt_still_rebuilds_manifest(self):
        # M0R5-03: an agent published before the interrupt must appear in the manifest with its new hashes.
        t=tempfile.TemporaryDirectory();self.addCleanup(t.cleanup);d=Path(t.name);repo=sandbox(d);root=repo/'tests/fixtures/protocol'
        driver=d/'driver.py';driver.write_text(DRIVER_PREAMBLE+'''
root=Path(sys.argv[2]);src=Path(sys.argv[3])/'tests/fixtures/protocol/claude/2.1.317/max-turns-error'
def claude(rec):
    rec.save('max-turns-error',['claude'],(src/'stdin.txt').read_bytes(),(src/'stdout.jsonl').read_bytes(),b'',1,'x');(rec.dir/'help.txt').write_bytes(b'h')
def pi(rec):raise KeyboardInterrupt
record.AGENTS.update(claude=claude,pi=pi);record.INSTALLED_VERSION.update(claude=lambda:'2.1.317',pi=lambda:'1.5.0')
record.binaries=lambda name:{};sys.argv=['record.py','--agents','claude,pi']
sys.exit(record.main())
''')
        r=subprocess.run([sys.executable,str(driver),str(repo),str(root),str(ROOT)],capture_output=True,text=True,timeout=60)
        self.assertEqual(r.returncode,130,r.stderr)
        entry=json.loads((root/'compat-manifest.json').read_text())['agents']['claude']['2.1.317']
        published=root/'claude/2.1.317/max-turns-error/stdout.jsonl'
        self.assertEqual(entry['cases']['max-turns-error']['files']['stdout.jsonl'],record.sha256(published.read_bytes()))

@needs_scopes
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
    def test_record_keeps_directories_of_a_child_it_could_not_stop(self):
        # M0R7-02: a cwd is removed only once nothing registered can still be using it.
        made={}
        def body(rec):
            made['cwd']=record.workdir();made['p']=record.Proc(['sleep','30'],made['cwd']);raise RuntimeError('case failed')
        with patch.dict(record.AGENTS,{'pi':body}),patch.dict(record.INSTALLED_VERSION,{'pi':lambda:'1.5.0'}),patch.object(record,'binaries',lambda name:{}),\
             patch.object(record,'REPO',self.root.parent),patch.object(record.Proc,'terminate',side_effect=RuntimeError('cannot confirm')):
            with self.assertRaises(record.RecordingFailed):record.record('pi',self.root)
        self.assertTrue(made['cwd'].exists());self.assertIn(made['p'],record.LIVE)
        made['p'].terminate();shutil.rmtree(made['cwd']);record.TEMP_DIRS.clear()
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
    def test_failed_exchange_keeps_the_new_recording(self):
        # M0R5-05: a publish that fails before the swap keeps the new recording as failure evidence.
        rec=self.recording();self.save_invalid_model(rec)
        with patch.object(record,'exchange',side_effect=PermissionError(13,'Permission denied')):
            with self.assertRaises(PermissionError):rec.publish({'cli_version':'1.5.0'})
        where=rec.abandon();self.assertTrue(str(where).startswith(str(self.failed)),where)
        self.assertTrue((where/'rpc-invalid-model/meta.json').is_file());self.assertTrue((where/'provenance.json').is_file())
        self.assertTrue((self.published/'old-only.txt').exists())
    def test_manifest_ignores_staging_directories(self):
        (self.root/'pi/.recording-1.5.0-x/rpc-prompt').mkdir(parents=True);record.write_manifest(self.root)
        self.assertEqual(list(json.loads((self.root/'compat-manifest.json').read_text())['agents']['pi']),['1.5.0'])

if __name__=='__main__':unittest.main()
