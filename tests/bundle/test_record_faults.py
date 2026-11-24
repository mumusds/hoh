"""Provider-fault fixtures (M2T07R-256, REQ-042): the fake provider, the sealed-namespace entry, the case validators and
the committed recordings under tests/fixtures/protocol/_provider-faults."""
import copy,errno,importlib.util,json,unittest,urllib.error,urllib.request
from helpers import ROOT

spec=importlib.util.spec_from_file_location('record_faults',ROOT/'tests/fixtures/protocol/record_faults.py')
faults=importlib.util.module_from_spec(spec);spec.loader.exec_module(faults)
FAULTS=ROOT/'tests/fixtures/protocol/_provider-faults'

def load(agent:str,case:str,version:str='*'):
    d=next(FAULTS.glob(f'{agent}/{version}/{case}'))
    return faults.record.frames((d/'stdout.jsonl').read_bytes()),json.loads((d/'meta.json').read_text()),faults.record.sent_messages((d/'stdin.txt').read_bytes())

def check(agent,case,fs,meta,sent=()):
    return faults.VALIDATORS[(agent,case)](fs,meta.get('exit_code'),meta,list(sent),{})

class Recordings(unittest.TestCase):
    def test_every_declared_case_is_committed_and_valid(self):
        # Every version directory's cases, each judged (an agent may keep an older version with fewer cases).
        recorded={(m.parts[-4],m.parts[-3],m.parts[-2]) for m in FAULTS.glob('*/*/*/meta.json')}
        committed={(agent,case) for agent,_,case in recorded}
        self.assertEqual(committed,set(faults.VALIDATORS))
        for agent,version,case in sorted(recorded):
            with self.subTest(case=f'{agent}/{version}/{case}'):
                fs,meta,sent=load(agent,case,version)
                self.assertEqual(meta['mode'],'fake-provider')
                self.assertTrue(meta['valid'])
                self.assertEqual(check(agent,case,fs,meta,sent),[])
    def test_every_version_directory_states_its_sealed_namespace(self):
        versions=sorted(p for p in FAULTS.glob('*/*') if p.is_dir())
        self.assertEqual({p.parent.name for p in versions},{'claude','codex','pi'})
        for v in versions:
            prov=json.loads((v/'provenance.json').read_text())
            self.assertEqual(prov['cli_version'],v.name)
            self.assertEqual(prov['namespace']['interfaces'],['lo'])
            self.assertEqual(prov['namespace']['route_out'],'ENETUNREACH')
    def test_the_live_recorder_and_the_manifest_never_see_them(self):
        # The live recorder's cases are <agent>/<version>/<case>; the manifest skips directories starting with '_'.
        live=ROOT/'tests/fixtures/protocol'
        self.assertFalse([m for m in live.glob('*/*/*/meta.json') if m.parts[-4].startswith('_')])
        manifest=json.loads((live/'compat-manifest.json').read_text())
        self.assertFalse([a for a in manifest['agents'] if a.startswith('_')])

class Validators(unittest.TestCase):
    def test_a_usage_limit_needs_its_reset_on_the_event_and_the_error(self):
        fs,meta,sent=load('claude','fault-usage-limit')
        bare=[{k:v for k,v in f.items() if k!='api_error_params'} for f in fs]
        self.assertTrue(any('api_error_params' in p for p in check('claude','fault-usage-limit',bare,meta,sent)))
        allowed=[f|{'rate_limit_info':f['rate_limit_info']|{'status':'allowed'}} if f.get('type')=='rate_limit_event' else f for f in fs]
        self.assertTrue(any('rate_limit_event' in p for p in check('claude','fault-usage-limit',allowed,meta,sent)))
    def test_a_long_retry_after_must_not_be_retried_by_claude(self):
        fs,meta,sent=load('claude','fault-429-retry-after-long')
        retry={'type':'system','subtype':'api_retry','attempt':1,'max_retries':10,'retry_delay_ms':120000,'error_status':429,'error':'rate_limit'}
        self.assertTrue(any('retried' in p for p in check('claude','fault-429-retry-after-long',[fs[0],retry,*fs[1:]],meta,sent)))
    def test_a_short_retry_after_is_waited_by_claude_itself(self):
        fs,meta,sent=load('claude','fault-429-retry-after-short')
        quick=[f|{'retry_delay_ms':500} if f.get('subtype')=='api_retry' else f for f in fs]
        self.assertTrue(any('Retry-After' in p for p in check('claude','fault-429-retry-after-short',quick,meta,sent)))
    def test_a_codex_usage_limit_needs_the_exhausted_window(self):
        fs,meta,sent=load('codex','app-server-fault-usage-limit')
        none=[f for f in fs if f.get('method')!='account/rateLimits/updated']
        self.assertTrue(any('account/rateLimits/updated' in p for p in check('codex','app-server-fault-usage-limit',none,meta,sent)))
    def test_exec_carries_nothing_structured(self):
        fs,meta,sent=load('codex','exec-fault-usage-limit')
        typed=[f|{'error':f['error']|{'resets_at':1}} if f.get('type')=='turn.failed' else f for f in fs]
        self.assertTrue(any('structured' in p for p in check('codex','exec-fault-usage-limit',typed,meta,sent)))
    def test_pi_waits_its_own_backoff_not_the_retry_after(self):
        fs,meta,sent=load('pi','rpc-fault-429-retry-after')
        server=[f|{'delayMs':3000} if f.get('type')=='auto_retry_start' else f for f in fs]
        self.assertTrue(any('delayMs' in p for p in check('pi','rpc-fault-429-retry-after',server,meta,sent)))
    def test_pi_in_turn_errors_need_their_settled_end(self):
        # PI16-01, PI16-B1: each recorded in-turn error of every pi version whose agent_settled HoH trusts (1.5.0 and
        # 1.6.0) ends with one agent_settled, not aborted, as its last frame.
        cases=('rpc-fault-429-retry-after','rpc-fault-503-retry-after','rpc-fault-usage-limit','rpc-fault-billing-no-retry')
        for version,case in [(v,c) for v in ('1.5.0','1.6.0') for c in cases]:
            with self.subTest(version=version,case=case):
                fs,meta,sent=load('pi',case,version)
                self.assertEqual(check('pi',case,fs,meta,sent),[])
                unsettled=[f for f in fs if f.get('type')!='agent_settled']
                self.assertTrue(any('no agent_settled' in p for p in check('pi',case,unsettled,meta,sent)))
                early=[*unsettled[:-1],fs[-1],unsettled[-1]]
                self.assertTrue(any('last frame' in p for p in check('pi',case,early,meta,sent)))
                aborted=[f|{'aborted':True} if f.get('type')=='agent_settled' else f for f in fs]
                self.assertTrue(any('agent_settled' in p for p in check('pi',case,aborted,meta,sent)))
    def test_a_pi_billing_error_is_not_retried(self):
        retry={'type':'auto_retry_start','attempt':1,'maxAttempts':3,'delayMs':2000,'errorMessage':'400'}
        for version in ('1.5.0','1.6.0'):
            with self.subTest(version=version):
                fs,meta,sent=load('pi','rpc-fault-billing-no-retry',version)
                self.assertTrue(any('retried' in p for p in check('pi','rpc-fault-billing-no-retry',[*fs[:-1],retry,fs[-1]],meta,sent)))
    def test_a_request_log_that_does_not_match_fails(self):
        fs,meta,sent=load('codex','app-server-fault-429-retry-after')
        other=copy.deepcopy(meta);other['provider']['requests']=other['provider']['requests'][:1]
        self.assertTrue(check('codex','app-server-fault-429-retry-after',fs,other,sent))
    def test_codex_503_needs_its_thirty_requests(self):
        # RA-250 (M2T07R-256 review round 3): the recorded 503 case made 30 requests (5 stream retries of 6 each); a
        # recording that stopped after 6 must not pass as it.
        fs,meta,sent=load('codex','app-server-fault-503-retry-after')
        self.assertEqual(check('codex','app-server-fault-503-retry-after',fs,meta,sent),[])
        six=copy.deepcopy(meta);six['provider']['requests']=six['provider']['requests'][:6]
        self.assertTrue(any('30' in p for p in check('codex','app-server-fault-503-retry-after',fs,six,sent)))

class FakeProvider(unittest.TestCase):
    def test_answers_its_scenario_in_turn_and_logs_no_key(self):
        scenario={'routes':[{'method':'POST','path_prefix':'/v1/x','responses':[
                      {'status':429,'headers':{'retry-after':'7','x-reset':'{now+60}'},'body':{'resets_at':'{now+60}'}},
                      {'status':503,'headers':{},'body':{'error':'busy'}}]}],
                  'default':{'status':404,'body':{'error':'no route'}}}
        p=faults.FakeProvider(scenario)
        try:
            def post(path):
                req=urllib.request.Request(p.url+path,data=b'{"model":"m"}',method='POST',headers={'x-api-key':'secret','authorization':'Bearer secret'})
                try:
                    with urllib.request.urlopen(req,timeout=10) as r:return r.status,dict(r.headers),json.loads(r.read())
                except urllib.error.HTTPError as e:
                    with e:return e.code,dict(e.headers),json.loads(e.read())
            s1,h1,b1=post('/v1/x');s2,_,_=post('/v1/x');s3,_,_=post('/v1/x');s4,_,_=post('/other')
        finally:
            p.close()
        self.assertEqual([s1,s2,s3,s4],[429,503,503,404])
        self.assertEqual(h1['retry-after'],'7')
        self.assertEqual(int(h1['x-reset']),b1['resets_at'])
        self.assertAlmostEqual(b1['resets_at'],p.log[0]['at']+60,delta=2)
        self.assertEqual([e['status'] for e in p.log],[429,503,503,404])
        self.assertEqual(p.log[0]['model'],'m')
        self.assertNotIn('secret',json.dumps(p.log))

class Namespace(unittest.TestCase):
    def test_only_loopback_and_no_route_out_is_sealed(self):
        self.assertEqual(faults.sealed_problems(['lo'],[errno.ENETUNREACH,errno.ENETUNREACH]),[])
        self.assertTrue(faults.sealed_problems(['lo','eth0'],[errno.ENETUNREACH,errno.ENETUNREACH]))
        self.assertTrue(faults.sealed_problems(['lo'],[0,errno.ENETUNREACH]))
        self.assertTrue(faults.sealed_problems(['lo'],[errno.ENETUNREACH,errno.ETIMEDOUT]))
    def test_the_entry_reexecutes_itself_in_a_new_network_namespace_with_an_allow_listed_environment(self):
        env={'PATH':'/bin','LANG':'C.UTF-8','ANTHROPIC_API_KEY':'k','CODEX_HOME':'/home/x/.codex','HOME':'/home/x','XDG_RUNTIME_DIR':'/run/user/1'}
        cmd=faults.outer_command(['--agents','pi'],env,1000,1000,'/usr/bin/python3')
        self.assertEqual(cmd[:5],['sudo','-n','unshare','-n','--'])
        self.assertIn('ip link set lo up',' '.join(cmd))
        self.assertIn('--reuid=1000',' '.join(cmd))
        self.assertEqual(cmd[-3:],['--inside','--agents','pi'])
        passed=[a for a in cmd if '=' in a and not a.startswith('-')]
        self.assertIn('PATH=/bin',passed);self.assertIn('XDG_RUNTIME_DIR=/run/user/1',passed)
        for name in ('ANTHROPIC_API_KEY','CODEX_HOME','HOME'):
            self.assertFalse([a for a in passed if a.startswith(name+'=')],name)

if __name__=='__main__':unittest.main()
