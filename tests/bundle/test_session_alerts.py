"""Session alert hooks (scripts/dev/session-alert.sh, session-context.sh): a main session's API failure or end is written
down with a recovery point and shown once at the next session start; delegated sessions are left alone."""
import json,os,shutil,subprocess,tempfile,unittest
from pathlib import Path
from helpers import ROOT

class SessionAlerts(unittest.TestCase):
    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory();self.root=Path(self.tmp.name)
        (self.root/'scripts/dev').mkdir(parents=True)
        for name in ('session-alert.sh','session_alert.py','session-context.sh'):
            shutil.copy(ROOT/'scripts/dev'/name,self.root/'scripts/dev'/name)
        subprocess.run(['git','init','-q','-b','main',str(self.root)],check=True)
        (self.root/'.hoh-dev').mkdir()
        (self.root/'.hoh-dev/task-state.json').write_text(json.dumps({'tasks':{'M1-T07':{'status':'running','owner':'main-dev'},'M1-T09':{'status':'blocked'}}}))
        (self.root/'.hoh-dev/PROGRESS.md').write_text('# x\n## Next\n- freeze the candidate\n- run round 17\n## Environment facts\n- n/a\n')
        (self.root/'.hoh-dev/evidence/m1t07-r4-018').mkdir(parents=True)
        self.env={k:v for k,v in os.environ.items() if k not in ('HOH_BOOTSTRAP_PARENT','HOH_DELEGATED_TASK')}
    def tearDown(self): self.tmp.cleanup()
    def hook(self,name,payload,env=None):
        return subprocess.run(['bash',str(self.root/'scripts/dev'/name)],input=payload,capture_output=True,text=True,env=env or self.env,timeout=30)
    def alerts(self):
        p=self.root/'.hoh-dev/session-alerts.jsonl'
        return [json.loads(l) for l in p.read_text().splitlines()] if p.exists() else []
    def test_an_api_failure_is_written_down_with_where_the_work_stood(self):
        # Background work still running is named: here a process standing for a delegated session.
        (self.root/'tools').mkdir();(self.root/'tools/agent_task.py').write_text('import time\ntime.sleep(30)\n')
        worker=subprocess.Popen(['python3',str(self.root/'tools/agent_task.py')])
        self.addCleanup(lambda:(worker.kill(),worker.wait()))
        out=self.hook('session-alert.sh',json.dumps({'hook_event_name':'StopFailure','session_id':'s1','error':'rate_limit','error_details':'429','last_assistant_message':'API Error: Rate limit reached'}))
        self.assertEqual(out.returncode,0,out.stderr)
        [a]=self.alerts()
        self.assertEqual((a['event'],a['error'],a['error_details'],a['session_id']),('StopFailure','rate_limit','429','s1'))
        self.assertEqual(a['message'],'API Error: Rate limit reached')
        rec=a['recovery']
        self.assertEqual(rec['tasks'],['M1-T07']);self.assertEqual(rec['branch'],'main')
        self.assertEqual(rec['next'],['- freeze the candidate','- run round 17'])
        self.assertTrue(rec['newest_evidence'].endswith('m1t07-r4-018'))
        self.assertTrue(any(str(worker.pid) in b and 'tools/agent_task.py' in b for b in rec['background']),rec['background'])
        self.assertFalse(any(b.split()[2]=='bash' for b in rec['background']),rec['background'])
        # Only a desktop notification (OSC 9 + BEL, printable ASCII between) is printed: Claude Code ignores the rest.
        seq=json.loads(out.stdout)['terminalSequence']
        self.assertTrue(seq.startswith('\x1b]9;') and seq.endswith('\x07'),repr(seq))
        self.assertTrue(all(' '<=c<='~' for c in seq[4:-1]),repr(seq))
        self.assertIn('rate_limit',seq)
    def test_a_session_end_is_written_down_silently(self):
        out=self.hook('session-alert.sh',json.dumps({'hook_event_name':'SessionEnd','reason':'other'}))
        self.assertEqual((out.returncode,out.stdout),(0,''))
        [a]=self.alerts();self.assertEqual((a['event'],a['reason'],a['message']),('SessionEnd','other',None))
    def test_delegated_sessions_and_bad_input_change_nothing_or_fail_nothing(self):
        # A session tools/agent_task.py starts for delegated work carries HOH_DELEGATED_TASK (its role).
        delegated=dict(self.env,HOH_DELEGATED_TASK='reviewer',HOH_BOOTSTRAP_PARENT='attempt-1')
        out=self.hook('session-alert.sh',json.dumps({'hook_event_name':'StopFailure','error':'server_error'}),env=delegated)
        self.assertEqual((out.returncode,out.stdout),(0,''));self.assertEqual(self.alerts(),[])
        self.assertEqual(self.hook('session-context.sh','',env=delegated).stdout,'')
        out=self.hook('session-alert.sh','not json')
        self.assertEqual(out.returncode,0);self.assertEqual(self.alerts()[0]['event'],'')
    def test_a_top_level_session_with_a_bootstrap_parent_is_a_main_session(self):
        # SAR-01: scripts/start.sh (interactive) and agent_task.py --top-level (headless) both set HOH_BOOTSTRAP_PARENT
        # for the top-level session; that alone does not make it delegated.
        for parent in ('interactive-20261106T000000Z-1','0b0d5b0e-attempt'):
            env=dict(self.env,HOH_BOOTSTRAP_PARENT=parent)
            out=self.hook('session-alert.sh',json.dumps({'hook_event_name':'SessionEnd','reason':'other'}),env=env)
            self.assertEqual(out.returncode,0,out.stderr)
            self.assertIn('main development session context',self.hook('session-context.sh','',env=env).stdout.lower())
        self.assertEqual([a['event'] for a in self.alerts()],['SessionEnd','SessionEnd'])
    def test_what_cannot_be_collected_does_not_lose_the_alert(self):
        # SAR-02: the event and its notification are kept whatever the recovery point's sources look like.
        (self.root/'.hoh-dev/evidence/dangling').symlink_to(self.root/'nowhere')
        (self.root/'.hoh-dev/task-state.json').write_text(json.dumps({'tasks':{'M1-T07':'running'}}))
        payloads=[{'hook_event_name':'StopFailure','error':'overloaded','last_assistant_message':{'not':'text'}},
                  {'hook_event_name':'StopFailure','error':'server_error','last_assistant_message':'x'*200_000}]
        for payload in payloads:
            out=self.hook('session-alert.sh',json.dumps(payload))
            self.assertEqual(out.returncode,0,out.stderr)
            self.assertIn(payload['error'],json.loads(out.stdout)['terminalSequence'])
        (self.root/'.hoh-dev/task-state.json').write_text('[1]')
        self.hook('session-alert.sh',json.dumps({'hook_event_name':'SessionEnd','reason':'logout'}))
        got=self.alerts()
        self.assertEqual([(a['event'],a['error'] or a['reason']) for a in got],[('StopFailure','overloaded'),('StopFailure','server_error'),('SessionEnd','logout')])
        self.assertEqual(len(got[1]['message']),300)
        self.assertEqual(got[0]['recovery']['branch'],'main')
        self.assertEqual(got[0]['recovery']['tasks'],'unavailable: AttributeError')
        self.assertEqual(got[0]['recovery']['newest_evidence'],'unavailable: FileNotFoundError')
        # What was unavailable is shown as such at the next start, not split into characters.
        (self.root/'.hoh-dev/task-state.json').write_text('{}')
        (self.root/'.hoh-dev/session-alerts.jsonl').write_text(json.dumps({'event':'SessionEnd','reason':'other','recovery':{'tasks':'unavailable: OSError','background':'unavailable: OSError','branch':'main'}})+'\n')
        shown=self.hook('session-context.sh','').stdout
        self.assertIn('tasks unavailable: OSError;',shown);self.assertIn('was running: unavailable: OSError',shown)
        self.assertNotIn('u, n, a',shown)
    def test_the_notification_keeps_only_printable_ascii(self):
        out=self.hook('session-alert.sh',json.dumps({'hook_event_name':'StopFailure','error':'bad\x1b]52;c;AAAA\x07\u00e9\nend'}))
        seq=json.loads(out.stdout)['terminalSequence']
        self.assertTrue(seq.startswith('\x1b]9;HoH: ') and seq.endswith('\x07'),repr(seq))
        self.assertTrue(all(' '<=c<='~' for c in seq[4:-1]),repr(seq));self.assertIn('bad]52;c;AAAAend',seq)
    def test_the_newest_background_work_is_named_however_much_runs(self):
        # SAR-04/-05: more matching processes than are kept, started before the one that matters, and other
        # interpreter spellings: the newest are kept.
        (self.root/'tools').mkdir();script=self.root/'tools/agent_task.py';script.write_text('import time\ntime.sleep(30)\n')
        decoy=self.root/'mutate7.py';decoy.write_text('import time\ntime.sleep(30)\n')
        procs=[subprocess.Popen(['python3',str(decoy)]) for _ in range(11)]
        procs.append(subprocess.Popen([shutil.which('python3'),'-u',str(script)]))
        for p in procs: self.addCleanup(lambda p=p:(p.kill(),p.wait()))
        self.hook('session-alert.sh',json.dumps({'hook_event_name':'SessionEnd','reason':'other'}))
        seen=self.alerts()[0]['recovery']['background']
        self.assertEqual(len(seen),10,seen)
        self.assertTrue(any(str(procs[-1].pid) in b and 'tools/agent_task.py' in b for b in seen),seen)
        self.assertTrue(any('mutate7.py' in b for b in seen),seen)
    def test_the_next_session_start_shows_each_alert_once(self):
        self.hook('session-alert.sh',json.dumps({'hook_event_name':'StopFailure','error':'overloaded'}))
        self.hook('session-alert.sh',json.dumps({'hook_event_name':'SessionEnd','reason':'prompt_input_exit'}))
        first=self.hook('session-context.sh','')
        self.assertEqual(first.returncode,0,first.stderr)
        self.assertEqual(first.stdout.count('[hoh-session] ALERT'),2,first.stdout)
        self.assertIn('StopFailure: overloaded',first.stdout);self.assertIn('SessionEnd: prompt_input_exit',first.stdout)
        self.assertIn('tasks M1-T07',first.stdout);self.assertIn('2 new alert(s)',first.stdout)
        second=self.hook('session-context.sh','')
        self.assertNotIn('ALERT',second.stdout,second.stdout)
        self.hook('session-alert.sh',json.dumps({'hook_event_name':'StopFailure','error':'unknown'}))
        third=self.hook('session-context.sh','')
        self.assertEqual(third.stdout.count('[hoh-session] ALERT'),1,third.stdout)
        # More new alerts than are shown: the newest five, and a line saying how many older ones were not.
        for n in range(7): self.hook('session-alert.sh',json.dumps({'hook_event_name':'StopFailure','error':f'e{n}'}))
        fourth=self.hook('session-context.sh','').stdout
        self.assertEqual(fourth.count('[hoh-session] ALERT'),5,fourth);self.assertIn('StopFailure: e6',fourth);self.assertNotIn('StopFailure: e1',fourth)
        self.assertIn('2 older new alert(s) not shown',fourth)

if __name__=='__main__': unittest.main()
