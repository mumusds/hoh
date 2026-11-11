"""Checks independent-qa and independent-review, scripts/product/independent.sh (docs/adr/ADR-027.md decision 11), with
fake native CLIs in a fixture ROOT of their own: run_check runs the real script, which runs the real tools/agent_task.py.
Fake agents only: NEVER live evidence, and nothing here writes into the real .hoh-dev/."""
import hashlib,json,os,shutil,subprocess,sys,tempfile,unittest
from pathlib import Path
from helpers import ROOT,load_json

FAKE=r"""#!/usr/bin/env python3
import json,os,sys,uuid
name=os.path.basename(sys.argv[0]);args=sys.argv[1:]
if '--help' in args:
 print('--json --dangerously-bypass-approvals-and-sandbox --skip-git-repo-check --output-format --verbose --dangerously-skip-permissions --mode --session-dir --approve --model');sys.exit(0)
if '--version' in args:print(name+' FAKE 0.0.0');sys.exit(0)
data=sys.stdin.read()
model=args[args.index('--model')+1] if '--model' in args else None
key=name+(':'+model if model else '')
behaviour=json.loads(os.environ['FAKE_PLAN']).get(key,'pass')
role=next((l.split(':',1)[1].strip() for l in data.splitlines() if l.startswith('Role:')),'reviewer')
with open(os.environ['FAKE_TRACE'],'a') as t:t.write(json.dumps({'agent':key,'behaviour':behaviour,'cwd':os.getcwd(),'prompt':data})+'\n')
sid=name+'-'+uuid.uuid4().hex[:12]
findings=[{'id':'F-1','severity':'high','summary':'a fake high finding'}] if behaviour=='high' else []
status={'fail':'FAIL'}.get(behaviour,'PASS')
report='Fake '+role+' result.\n```hoh-report\n'+json.dumps({'role':role,'status':status,'scope':['fake scope'],'findings':findings})+'\n```'
text='Fake result without a report.' if behaviour=='noreport' else report
if behaviour=='started78':
 print(json.dumps({'type':'thread.started','thread_id':sid} if name=='codex' else {'type':'system','subtype':'init','session_id':sid}));sys.exit(78)
errors={'capacity':'Selected model is at capacity. Please try a different model.','auth':'Invalid API key · Fix external API key',
        'quota':'Claude AI usage limit reached|1700000000','nokey':'No API key found for anthropic','crash':'unexpected internal error'}
if name=='codex':
 print(json.dumps({'type':'thread.started','thread_id':sid}))
 if behaviour in errors:
  print(json.dumps({'type':'error','message':errors[behaviour]}));print(json.dumps({'type':'turn.failed','error':{'message':errors[behaviour]}}));sys.exit(1)
 print(json.dumps({'type':'item.completed','item':{'type':'agent_message','text':text}}));print(json.dumps({'type':'turn.completed','usage':{}}))
elif name=='claude':
 print(json.dumps({'type':'system','subtype':'init','session_id':sid,'model':'fake-model'}))
 if behaviour in errors:print(json.dumps({'type':'result','subtype':'success','is_error':True,'result':errors[behaviour],'session_id':sid}));sys.exit(1)
 print(json.dumps({'type':'result','subtype':'success','is_error':False,'result':text,'session_id':sid}))
else:
 print(json.dumps({'type':'session','id':sid}))
 if behaviour in errors:
  print(json.dumps({'type':'message_end','message':{'role':'assistant','content':[],'stopReason':'error','errorMessage':errors[behaviour]}}));print(json.dumps({'type':'agent_end'}));sys.exit(1)
 print(json.dumps({'type':'message_end','message':{'role':'assistant','content':[{'type':'text','text':text}],'stopReason':'stop'}}));print(json.dumps({'type':'agent_end'}))
"""

FILES=['tools/agent_task.py','tools/common.py','tools/run_check.py','tools/candidate.py','tools/install_pi_plugins.py','tools/render_prompt.py',
       'scripts/product/independent.sh','scripts/product/independent.py','scripts/product/traceability.py',
       'resources/prompts/qa.md','resources/prompts/reviewer.md']+[f'contracts/{n}.schema.json' for n in
       ('independent-report','live-session','execution','test-result','soak-event','candidate','check-plan','native-attempt')]
ARTIFACTS=['live-sessions.json','independent-report.json','native-attempt.json','raw-session.jsonl']

def sha(path):return hashlib.sha256(Path(path).read_bytes()).hexdigest()

class Independent(unittest.TestCase):
    """The real independent.sh through run_check in a fixture ROOT, the native CLIs faked on a PATH without the real ones."""
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory(prefix='hoh-independent-');self.addCleanup(self.temp.cleanup)
        base=Path(self.temp.name);r=self.root=base/'repo';self.bin=base/'bin';self.bin.mkdir();self.trace=base/'trace.jsonl'
        for rel in FILES:(r/rel).parent.mkdir(parents=True,exist_ok=True);shutil.copy2(ROOT/rel,r/rel)
        (r/'contracts/requirements.json').write_text(json.dumps({'requirements':[{'id':'REQ-901','release_targets':['v0.1'],'release_class':'required',
                                                                                'acceptance':[{'id':'AC-901-01','test_id':'TEST-901-01'},{'id':'AC-901-02','test_id':'TEST-901-02'}]}]}))
        (r/'contracts/acceptance-scenarios.json').write_text(json.dumps({'scenarios':[{'id':'TEST-901-01','check_id':'independent-review'},
                                                                                     {'id':'TEST-901-02','check_id':'traceability'}]}))
        # A stand-in traceability check: one passing line for its TEST.
        (r/'trace.sh').write_text('printf \'%s\\n\' \'{"schema_version": "1.0", "test_id": "TEST-901-02", "name": "test_901_02_x", "status": "PASS"}\' > "$HOH_EVIDENCE_DIR/test-results.jsonl"\n')
        checks={f'independent-{c}':{'mode':'live','argv':['bash','scripts/product/independent.sh',role,'v0.1'],'artifacts':ARTIFACTS,'timeout_seconds':120}
                for c,role in (('qa','qa'),('review','reviewer'))}
        checks['traceability']={'mode':'product','argv':['bash','trace.sh'],'artifacts':['test-results.jsonl'],'timeout_seconds':60}
        (r/'plan.json').write_text(json.dumps({'schema_version':'1.0','plan_id':'independent-fixture','description':'Synthetic fixture plan.','checks':checks},indent=1))
        (r/'.gitignore').write_text('.hoh-dev/\n__pycache__/\n')
        git=lambda *a:subprocess.run(['git','-c','user.name=t','-c','user.email=t@invalid.example',*a],cwd=r,check=True,env=self.env())
        git('init','-q');git('add','.');git('commit','-qm','fixture')
        (r/'.hoh-dev').mkdir();(r/'.hoh-dev/environment.json').write_text('{"mode":"SYNTHETIC_INDEPENDENT_FIXTURE"}\n')
        out=subprocess.run([sys.executable,'tools/candidate.py','freeze','--spec','contracts/requirements.json','--environment','.hoh-dev/environment.json',
                            '--test-plan','plan.json','--out','.hoh-dev/candidates/c.json'],cwd=r,capture_output=True,text=True,env=self.env())
        self.assertEqual(out.returncode,0,out.stderr);self.candidate=load_json(r/'.hoh-dev/candidates/c.json')
    def env(self,plan=None):
        keep={k:v for k,v in os.environ.items() if not k.startswith(('HOH_','CLAUDE','CODEX','AI_AGENT','FAKE_'))}
        return keep|{'PATH':f'{self.bin}:/usr/bin:/bin','PYTHONDONTWRITEBYTECODE':'1','FAKE_PLAN':json.dumps(plan or {}),'FAKE_TRACE':str(self.trace),
                     'GIT_CONFIG_GLOBAL':'/dev/null','GIT_CONFIG_NOSYSTEM':'1'}
    def agents(self,*names):
        for n in names:p=self.bin/n;p.write_text(FAKE);p.chmod(0o755)
    def check(self,check,plan):
        out=self.root/f'.hoh-dev/evidence/{check}'
        r=subprocess.run([sys.executable,'tools/run_check.py','--check-id',check,'--candidate-file','.hoh-dev/candidates/c.json','--out',str(out)],
                         cwd=self.root,capture_output=True,text=True,env=self.env(plan),timeout=300)
        record=load_json(out/'execution.json');self.assertEqual(r.returncode,record['exit_code'],r.stderr)
        return record,out/'artifacts',(out/'stderr.log').read_text()
    def calls(self):return [json.loads(l) for l in self.trace.read_text().splitlines()] if self.trace.exists() else []
    def assert_report(self,record,artifacts,role,status='PASS'):
        """The four declared artifacts, bound as the gate binds them: the report's session listed in live-sessions.json,
        its attempt record and raw stream the copies' bytes, all inside the check's directory."""
        self.assertEqual(record['missing_artifacts'],[])
        report=load_json(artifacts/'independent-report.json');sessions=load_json(artifacts/'live-sessions.json');attempt=load_json(artifacts/'native-attempt.json')
        self.assertEqual((report['role'],report['status'],report['candidate_id']),(role,status,self.candidate['candidate_id']))
        self.assertIn(report['session_id'],[s['session_id'] for s in sessions['sessions']])
        self.assertEqual((sessions['check_id'],sessions['harness_candidate_id']),(record['check_id'],self.candidate['candidate_id']))
        self.assertEqual(report['native_attempt']['sha256'],sha(artifacts/'native-attempt.json'))
        self.assertEqual(report['raw_session']['sha256'],sha(artifacts/'raw-session.jsonl'))
        self.assertEqual(attempt['stdout_sha256'],sha(artifacts/'raw-session.jsonl'))
        self.assertIn(report['session_id'],attempt['session_ids'])
        for ref in (report['native_attempt'],report['raw_session']):
            self.assertTrue((self.root/ref['path']).resolve().is_relative_to(artifacts.resolve()),ref)
            self.assertEqual(sha(self.root/ref['path']),ref['sha256'])
        return report
    def test_a_passing_review_is_exit_0_with_its_four_artifacts(self):
        self.agents('codex','claude');record,artifacts,_=self.check('independent-review',{'codex':'pass'})
        self.assertEqual(record['exit_code'],0);self.assert_report(record,artifacts,'reviewer')
        calls=self.calls();self.assertEqual([c['agent'] for c in calls],['codex'])
        # The prompt: the reviewer's role contract, the candidate, principle one's judgement, the observations directory.
        prompt=calls[0]['prompt'];self.assertIn('[HOH_DELEGATED_TASK]',prompt);self.assertIn(self.candidate['candidate_id'],prompt)
        # Observations beside the record's directory, never in it (M2T10R-52).
        record_dir=artifacts.parent;observations=record_dir.parent/f'{record_dir.name}-observations'
        self.assertIn(str(observations),prompt);self.assertTrue(observations.is_dir());self.assertIn(str(artifacts/'judgement.json'),prompt)
        self.assertEqual(Path(calls[0]['cwd']).resolve(),self.root.resolve())
        judgement=load_json(artifacts/'judgement.json');self.assertEqual(judgement['candidate_id'],self.candidate['candidate_id'])
        self.assertIn('TEST-901-01',judgement['tests'])
    def test_the_judgement_counts_an_earlier_traceability_run(self):
        # M2T10R-51: the independent check judges as itself, so a traceability run that passed is counted, not taken
        # for its own missing self-test.
        self.agents('codex');record,_,_=self.check('traceability',{});self.assertEqual(record['exit_code'],0)
        record,artifacts,_=self.check('independent-review',{'codex':'pass'});self.assertEqual(record['exit_code'],0)
        self.assertEqual(load_json(artifacts/'judgement.json')['tests']['TEST-901-02']['status'],'PASS')
    def test_a_capacity_error_falls_back_to_the_next_model_then_agent(self):
        self.agents('codex','claude')
        record,artifacts,_=self.check('independent-review',{'codex':'capacity','codex:gpt-6.1-sol':'pass'})
        self.assertEqual(record['exit_code'],0);self.assert_report(record,artifacts,'reviewer')
        self.assertEqual([c['agent'] for c in self.calls()],['codex','codex:gpt-6.1-sol'])
    def test_a_written_failure_stops_the_search(self):
        self.agents('codex','claude');record,artifacts,_=self.check('independent-review',{'codex':'fail'})
        self.assertEqual(record['exit_code'],1);self.assert_report(record,artifacts,'reviewer','FAIL')
        self.assertEqual([c['agent'] for c in self.calls()],['codex'])
    def test_a_pass_with_a_high_finding_is_exit_1(self):
        self.agents('codex','claude');record,artifacts,_=self.check('independent-review',{'codex':'high'})
        self.assertEqual(record['exit_code'],1);report=self.assert_report(record,artifacts,'reviewer')
        self.assertEqual([f['severity'] for f in report['findings']],['high'])
    def test_every_attempt_blocked_outside_is_exit_3(self):
        self.agents('codex','claude')
        record,_,stderr=self.check('independent-review',{'codex':'capacity','codex:gpt-6.1-sol':'capacity','claude':'auth'})
        self.assertEqual(record['exit_code'],3,stderr)
        self.assertEqual(set(record['missing_artifacts']),set(ARTIFACTS))
        self.assertEqual([c['agent'] for c in self.calls()],['codex','codex:gpt-6.1-sol','claude'])
    def test_qa_runs_pi_then_claude(self):
        self.agents('pi','claude');record,artifacts,_=self.check('independent-qa',{'pi':'nokey','claude':'pass'})
        self.assertEqual(record['exit_code'],0);self.assert_report(record,artifacts,'qa')
        self.assertEqual([c['agent'] for c in self.calls()],['pi','claude'])
    def test_a_missing_cli_and_a_quota_failure_are_blocked(self):
        self.agents('claude');record,_,stderr=self.check('independent-qa',{'claude':'quota'})
        self.assertEqual(record['exit_code'],3,stderr);self.assertEqual([c['agent'] for c in self.calls()],['claude'])
    def test_a_session_without_a_report_is_not_an_external_block(self):
        self.agents('pi','claude');record,_,stderr=self.check('independent-qa',{'pi':'noreport','claude':'auth'})
        self.assertEqual(record['exit_code'],1,stderr);self.assertEqual([c['agent'] for c in self.calls()],['pi','claude'])
    def test_a_cli_that_started_and_exited_78_is_not_an_external_block(self):
        # M2T10R-53: agent_task passes a started CLI's own exit code through; only with no attempt record did it end
        # before the CLI started.
        self.agents('codex','claude')
        record,_,stderr=self.check('independent-review',{'codex':'started78','codex:gpt-6.1-sol':'started78','claude':'started78'})
        self.assertEqual(record['exit_code'],1,stderr)
    def test_an_unknown_failure_is_not_an_external_block(self):
        self.agents('pi','claude');record,_,_=self.check('independent-qa',{'pi':'crash','claude':'crash'})
        self.assertEqual(record['exit_code'],1)

if __name__=='__main__':unittest.main()
