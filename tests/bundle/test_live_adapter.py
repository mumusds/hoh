"""scripts/product/adapter-live.sh's qa path (TEST-053-05, docs/adr/ADR-029.md S7) with fake agents and a stand-in for the
ignored live tests: availability and root are judged by the script before any test runs, the qa verdict maps to
TEST-053-05, and nothing but a live run's own verdict with the native sessions of its bindings is ever a PASS. No agent
and no cargo is ever run: every program the script finds is this test's own."""
import json,os,shlex,shutil,subprocess,sys,tempfile,unittest
from pathlib import Path
from helpers import ROOT,own_python

QA_TEST='crates/hoh-app/tests/verification_live.rs::verification_live_candidate_to_qa'
QA_ARGV=['test','-p','hoh-app','--locked','--test','verification_live','--','--ignored','--exact',
         'verification_live_candidate_to_qa','--nocapture']
# The stand-in for `cargo test`: records how it was called and writes the verdict FAKE_VERDICTS gives for the test it
# was asked to run (none when it gives none), as the ignored test would into HOH_LIVE_OUT.
FAKE_CARGO=r"""import json,os,sys
args=sys.argv[1:]
with open(os.environ['CALLS'],'a') as f:f.write(json.dumps({'argv':args,'env':{k:v for k,v in os.environ.items() if k.startswith('HOH_LIVE')}})+'\n')
name=args[args.index('--exact')+1]
agent={'claude_live_turns_resume_and_cancel':'claude','codex_live_turn_and_cancel':'codex','pi_live_turns_resume_and_cancel':'pi',
       'doctor_live_versions_whitelist_and_verified_logins':'doctor','verification_live_candidate_to_qa':'qa'}[name]
v=json.loads(os.environ.get('FAKE_VERDICTS','{}')).get(agent)
if v is not None:open(os.path.join(os.environ['HOH_LIVE_OUT'],agent+'-live-verdict.json'),'w').write(json.dumps(v))
"""
DEFAULT={'developer':'claude','qa':'pi','reviewer':'codex'}

def session(agent,sid):return {'agent':agent,'cli_version':'FAKE-1.0','session_id':sid,'resolved_model':'fake-model'}

def passed(agents=DEFAULT):
    """A PASS verdict as the live test writes one: a native session of each bound agent (the Developer's and the QA's
    distinct even when both are claude)."""
    return {'status':'PASS','detail':'fake','agents':agents,
            'sessions':[session(agents[r],f'native-{r}') for r in ('developer','qa','reviewer')]}

class AdapterLiveQa(unittest.TestCase):
    def setUp(self):
        t=tempfile.TemporaryDirectory(prefix='hoh-adapter-live-');self.addCleanup(t.cleanup);self.root=Path(t.name)
        (self.root/'scripts/product').mkdir(parents=True)
        shutil.copy(ROOT/'scripts/product/adapter-live.sh',self.root/'scripts/product/adapter-live.sh')
        self.bin=self.root/'bin';self.bin.mkdir();self.tools=self.root/'tools';self.tools.mkdir()
        # Only the tools the script uses, and none of the system's PATH: an agent installed there is never found.
        for tool in ['dirname','mkdir','rm','id','mktemp']:
            found=shutil.which(tool,path='/usr/bin:/bin');self.assertIsNotNone(found,tool);os.symlink(found,self.tools/tool)
        (self.root/'cargo.py').write_text(FAKE_CARGO)
        self.script('cargo',f'exec {shlex.quote(sys.executable)} {shlex.quote(str(self.root/"cargo.py"))} "$@"')
        self.ev=self.root/'evidence';self.ev.mkdir();self.calls=self.root/'calls.jsonl'
    def script(self,name,body):
        p=self.bin/name;p.write_text(f'#!/bin/sh\n{body}\n');p.chmod(0o755);return p
    def run_script(self,agents=('claude','codex','pi'),verdicts=None,root=False,env=None):
        for name in ['claude','codex','pi','id']:(self.bin/name).unlink(missing_ok=True)
        for agent in agents:self.script(agent,'exit 0')
        if root:self.script('id','[ "$1" = -u ] && { echo 0; exit 0; }; exit 1')
        # The other live tests' verdicts are given too, so the qa path is the only one under test.
        given={'claude':{'status':'BLOCKED','detail':'fake','sessions':[]},'codex':{'status':'BLOCKED','detail':'fake','sessions':[]},
               'pi':{'status':'BLOCKED','detail':'fake','sessions':[]},'doctor':{'status':'BLOCKED','detail':'fake','sessions':[]}}|(verdicts or {})
        base={'PATH':own_python(self.bin,str(self.tools)),'HOME':str(self.root),'HOH_EVIDENCE_DIR':str(self.ev),
              'CALLS':str(self.calls),'FAKE_VERDICTS':json.dumps(given)}
        p=subprocess.run([shutil.which('bash'),str(self.root/'scripts/product/adapter-live.sh')],capture_output=True,text=True,
                         env=base|(env or {}),timeout=60)
        results={}
        for line in (self.ev/'test-results.jsonl').read_text().splitlines():
            row=json.loads(line);results[row['test_id']]=row
        calls=[json.loads(l) for l in self.calls.read_text().splitlines()] if self.calls.exists() else []
        return p,results,[c for c in calls if 'verification_live' in c['argv']]
    def assertBlocked(self,results,calls,why):
        row=results['TEST-053-05'];self.assertEqual((row['status'],row['name']),('SKIPPED',QA_TEST),row)
        self.assertTrue(row['detail'].startswith('BLOCKED: '),row);self.assertIn(why,row['detail'])
        self.assertEqual(calls,[],'the live test is never run')
    def test_qa_is_blocked_without_claude(self):
        p,results,calls=self.run_script(agents=('codex','pi'))
        self.assertBlocked(results,calls,'claude is not installed');self.assertEqual(p.returncode,3,p.stderr)
    def test_qa_is_blocked_as_root(self):
        p,results,calls=self.run_script(root=True)
        self.assertBlocked(results,calls,'running as root');self.assertEqual(p.returncode,3,p.stderr)
    def test_the_default_qa_and_reviewer_are_needed_unless_v01_is_asked_for(self):
        # AC-053-05's given: Developer claude, QA pi, Reviewer codex; claude in their place only under HOH_LIVE_PROFILE=v0.1.
        for agents,missing in [(('claude',),'pi codex'),(('claude','codex'),'pi'),(('claude','pi'),'codex')]:
            with self.subTest(agents=agents):
                self.calls.unlink(missing_ok=True)
                _,results,calls=self.run_script(agents=agents);self.assertBlocked(results,calls,missing+' not installed')
                self.assertIn('HOH_LIVE_PROFILE=v0.1',results['TEST-053-05']['detail'])
        self.calls.unlink(missing_ok=True)
        _,results,calls=self.run_script(agents=('claude',),env={'HOH_LIVE_PROFILE':'v0.1'},
                                        verdicts={'qa':{'status':'BLOCKED','detail':'not logged in','sessions':[]}})
        self.assertEqual(len(calls),1,calls);self.assertEqual(calls[0]['env']['HOH_LIVE_CLAUDE'],str(self.bin/'claude'))
        self.assertEqual((calls[0]['env']['HOH_LIVE_PI'],calls[0]['env']['HOH_LIVE_CODEX']),('',''))
        self.assertEqual(results['TEST-053-05']['status'],'SKIPPED')
    def test_the_qa_verdict_is_test_053_05(self):
        for verdict,status in [(passed(),'PASS'),({'status':'FAIL','detail':'AC-B PASS','sessions':[]},'FAIL'),
                               ({'status':'BLOCKED','detail':'BLOCKED_AUTH','sessions':[]},'SKIPPED')]:
            with self.subTest(verdict=verdict['status']):
                self.calls.unlink(missing_ok=True);(self.ev/'test-results.jsonl').unlink(missing_ok=True)
                p,results,calls=self.run_script(verdicts={'qa':verdict},env={'HOH_CANDIDATE_ID':'CAND-'+'a'*64})
                self.assertEqual(len(calls),1,calls);self.assertEqual(calls[0]['argv'],QA_ARGV)
                self.assertEqual(calls[0]['env'],{'HOH_LIVE_OUT':str(self.ev),'HOH_LIVE_CLAUDE':str(self.bin/'claude'),
                                                  'HOH_LIVE_PI':str(self.bin/'pi'),'HOH_LIVE_CODEX':str(self.bin/'codex')})
                row=results['TEST-053-05'];self.assertEqual((row['status'],row['name']),(status,QA_TEST),row)
                self.assertTrue(row['detail'].startswith(verdict['status']+': '),row)
                if status=='PASS':
                    live=json.loads((self.ev/'live-sessions.json').read_text())
                    self.assertEqual([s['session_id'] for s in live['sessions']],['native-developer','native-qa','native-reviewer'])
                    self.assertEqual(p.returncode,3,'the other live tests are BLOCKED')
    def test_a_fake_run_never_passes_test_053_05(self):
        v01=dict(DEFAULT,qa='claude',reviewer='claude')
        no_qa_session=passed();no_qa_session['sessions']=[s for s in no_qa_session['sessions'] if s['agent']!='pi']
        one_session=passed(v01);one_session['sessions']=[session('claude','native-1')]
        cases=[
            # A PASS left from an earlier run (or put there) while the live test writes none.
            ('stale verdict',None,{},'no verdict'),
            # A PASS without the native session of a bound agent, or of the Developer and the QA as one session.
            ('no qa session',no_qa_session,{},'live sessions'),
            ('one session',one_session,{'HOH_LIVE_PROFILE':'v0.1'},'live sessions'),
            # claude in place of the default QA and Reviewer without the explicit v0.1 flag.
            ('fallback without v0.1',passed(v01),{},'live sessions'),
            ('no bindings',{k:v for k,v in passed().items() if k!='agents'},{},'live sessions'),
            ('bindings not an object',passed()|{'agents':['claude','pi','codex']},{},'live sessions')]
        for name,verdict,env,why in cases:
            with self.subTest(name):
                self.calls.unlink(missing_ok=True);(self.ev/'test-results.jsonl').unlink(missing_ok=True)
                (self.ev/'qa-live-verdict.json').write_text(json.dumps(passed()))
                p,results,calls=self.run_script(verdicts={} if verdict is None else {'qa':verdict},env=env)
                self.assertEqual(len(calls),1,calls)
                row=results['TEST-053-05'];self.assertEqual(row['status'],'FAIL',row);self.assertIn(why,row['detail'])
                self.assertEqual(p.returncode,1,p.stderr)
        # The same bindings under the flag, with two claude sessions, do pass (v0.1 allows them, AC-053-05).
        self.calls.unlink(missing_ok=True);(self.ev/'test-results.jsonl').unlink(missing_ok=True)
        _,results,_=self.run_script(verdicts={'qa':passed(v01)},env={'HOH_LIVE_PROFILE':'v0.1'})
        self.assertEqual(results['TEST-053-05']['status'],'PASS',results['TEST-053-05'])

if __name__=='__main__':unittest.main()
