"""Synthetic unit fixtures. NEVER exported as real product acceptance evidence."""
from pathlib import Path
import copy,json,os,shlex,shutil,subprocess,sys,tempfile
from datetime import datetime,timedelta,timezone
ROOT=Path(__file__).resolve().parents[2]
sys.path.insert(0,str(ROOT/'tools'))
from common import atomic_json,load_json,ref_for,sha256
from candidate import freeze

def own_python(bin_dir: Path, rest: str) -> str:
    """A PATH for a fixture's subprocesses whose python3 is the interpreter running these tests: the product's scripts
    that a check runs by `python3` then judge under requirements-dev.txt's jsonschema, as a real run_check runs them
    under the project's environment, whatever PATH the tests were started with (M2T10R-750, -800). Writes the shim
    into `bin_dir`; `rest` follows it."""
    shim=bin_dir/'python3';shim.write_text(f'#!/bin/sh\nexec {shlex.quote(sys.executable)} "$@"\n');shim.chmod(0o755)
    return f'{bin_dir}:{rest}'

def json_file(root,path,data):
    p=root/path;atomic_json(p,data);return ref_for(p,root)

def init_repository(root,checks=None):
    """Synthetic Git repo whose test plan is a check plan with the given checks."""
    subprocess.run(['git','init','--quiet',str(root)],check=True)
    subprocess.run(['git','-C',str(root),'config','user.email','test@invalid.example'],check=True)
    subprocess.run(['git','-C',str(root),'config','user.name','HoH bundle fixture'],check=True)
    (root/'.gitignore').write_text('.hoh-dev/\n__pycache__/\n')
    (root/'source.txt').write_text('synthetic source\n',encoding='utf-8')
    (root/'spec.md').write_text('Synthetic specification. Not a real product.\n')
    plan={'schema_version':'1.0','plan_id':'synthetic-fixture','description':'Synthetic test plan. Not real coverage.',
          'checks':checks or {'unit':{'mode':'mock','argv':[sys.executable,'-c','print("MOCK UNIT CHECK ONLY")']}}}
    (root/'plan.json').write_text(json.dumps(plan,indent=2)+'\n')
    subprocess.run(['git','-C',str(root),'add','.'],check=True)
    subprocess.run(['git','-C',str(root),'commit','--quiet','-m','synthetic test fixture'],check=True)
    json_file(root,'.hoh-dev/environment.json',{'mode':'SYNTHETIC_UNIT_FIXTURE'})
    c=freeze(root,root/'spec.md',root/'.hoh-dev/environment.json',root/'plan.json')
    cr=json_file(root,'.hoh-dev/candidate.json',c)
    return c,cr

REPORT_SESSION={'independent-qa':'qa-independent-fixture','independent-review':'reviewer-independent-fixture'}
ROLE_CHECK={'qa':'independent-qa','reviewer':'independent-review'}

class ReleaseFixture:
    """Positive envelope fixture only; no actual live run is claimed."""
    def __init__(self,profile='v1',plan_overrides=None):
        self.temp=tempfile.TemporaryDirectory(prefix='hoh-gate-fixture-');self.root=Path(self.temp.name)
        r=self.root
        lock=load_json(ROOT/'contracts/baseline-lock.json')
        shutil.copytree(ROOT/'contracts',r/'contracts')
        for rel in list(lock['files'])+[c['path'] for c in lock.get('change_requests',[])]:
            (r/rel).parent.mkdir(parents=True,exist_ok=True);shutil.copy(ROOT/rel,r/rel)
        policy=load_json(r/'contracts/release-policy.json');self.policy=policy
        req=load_json(r/'contracts/requirements.json')['requirements']
        scen={s['id']:s for s in load_json(r/'contracts/acceptance-scenarios.json')['scenarios']}
        names=policy['artifacts']
        plan_checks={}
        for key,rule in policy['checks'].items():
            entry={'mode':rule['mode'],'argv':['SYNTHETIC_FIXTURE_NOT_EXECUTED',key]}
            arts=([names['live_sessions']] if rule['mode']=='live' else [])+([names['soak_timeline']] if key=='soak' else [])+[names['test_results']]
            arts+=[names['independent_report']] if key in ROLE_CHECK.values() else []
            arts+=[names['release_bundle']] if key=='build' else []
            if arts:entry['artifacts']=arts
            entry.update((plan_overrides or {}).get(key,{}))
            plan_checks[key]=entry
        self.plan=plan_checks
        c,cr=init_repository(r,plan_checks);self.candidate=c;cid=c['candidate_id']
        prof=policy['profiles'][profile];self.profile=prof
        tests_by_check={}
        for item in req:
            if profile in item['release_targets']:
                for ac in item['acceptance']:tests_by_check.setdefault(scen[ac['test_id']]['check_id'],[]).append(ac['test_id'])
        self.attempts={};self.report_paths={}
        start=datetime(2020,1,1,tzinfo=timezone.utc);duration=policy['minimum_soak_seconds'];self.start=start
        checks=[];self.records={};self.dirs={}
        for key in prof['required_checks']:
            p=f'.hoh-dev/evidence/{key}';self.dirs[key]=p
            out=r/p/'stdout.log';out.parent.mkdir(parents=True,exist_ok=True);out.write_text('SYNTHETIC UNIT FIXTURE ONLY: '+key+'\n')
            err=r/p/'stderr.log';err.write_text('')
            d=duration if key=='soak' else 1
            artifacts=[]
            for name in plan_checks[key].get('artifacts',[]):
                f=r/p/'artifacts'/name;f.parent.mkdir(parents=True,exist_ok=True)
                if name==names['live_sessions']:
                    agents=prof.get('live_agents',{}).get(key,['claude'])
                    sessions=[{'agent':a,'cli_version':'SYNTHETIC','session_id':REPORT_SESSION.get(key,f'synthetic-{key}-{a}'),'resolved_model':'unknown'} for a in agents]
                    for x in sessions:
                        if x['agent']=='pi' and policy['checks'][key].get('requires_plugins'):x['plugins']=[{'package':'synthetic-plugin','version':'0.0.0'}]
                    f.write_text(json.dumps({'schema_version':'1.0','check_id':key,'harness_candidate_id':cid,'sessions':sessions}))
                elif name==names['test_results']:
                    f.write_text(''.join(json.dumps({'schema_version':'1.0','test_id':t,'name':'synthetic_'+t.lower().replace('-','_'),'status':'PASS'})+'\n' for t in tests_by_check.get(key,[])))
                elif name==names['independent_report']:
                    role=next(k for k,v in ROLE_CHECK.items() if v==key);sid=role+'-independent-fixture'
                    raw=r/p/'artifacts/attempts/a1/stdout.jsonl';raw.parent.mkdir(parents=True);raw.write_text('{"synthetic_fixture_only":true}\n')
                    attempt={'schema_version':'1.1','record_type':'bootstrap_native_attempt','role':role,'status':'NATIVE_TURN_COMPLETED','session_ids':[sid],
                             'candidate_id':cid,'stdout_sha256':sha256(raw),'product_accepted':False}
                    self.attempts[role]=attempt
                    report={'schema_version':'1.1','role':role,'session_id':sid,'candidate_id':cid,'spec_sha256':c['spec_sha256'],'status':'PASS','scope':['SYNTHETIC schema test only'],
                            'findings':[],'raw_session':ref_for(raw,r),'native_attempt':json_file(r,f'{p}/artifacts/attempts/a1/attempt.json',attempt)}
                    f.write_text(json.dumps(report));self.report_paths[role]=(key,name)
                elif name==names['release_bundle']:f.write_bytes(b'SYNTHETIC_NOT_A_HOH_RELEASE')
                elif name==names['soak_timeline']:
                    events=[{'schema_version':'1.0','kind':'iteration.accepted','mode':'live','run_id':'fixture-run','iteration_id':f'fixture-{i}','harness_candidate_id':cid,
                             'occurred_at':(start+timedelta(seconds=i*duration/19)).isoformat()} for i in range(20)]
                    f.write_text(''.join(json.dumps(e)+'\n' for e in events))
                else:f.write_text('{"synthetic":true}\n')
                artifacts.append({'name':name}|ref_for(f,r))
            execution={'schema_version':'1.1','record_type':'command_execution','record_id':'fixture-'+key,'producer_attempt_id':'fixture-'+key,'check_id':key,
                       'candidate_id':cid,'spec_sha256':c['spec_sha256'],'candidate_manifest':cr,'source_unchanged':True,
                       'environment_sha256':c['environment_sha256'],'test_plan_sha256':c['test_plan_sha256'],'mode':plan_checks[key]['mode'],
                       'command':plan_checks[key]['argv'],'cwd':'.','started_at':start.isoformat(),'finished_at':(start+timedelta(seconds=d)).isoformat(),
                       'duration_seconds':d,'exit_code':0,'timed_out':False,'stdout':ref_for(out,r),'stderr':ref_for(err,r),
                       'artifacts':artifacts,'missing_artifacts':[],'runner':{'tool':'tools/run_check.py','euid':1000}}
            execution_ref=json_file(r,p+'/execution.json',execution);self.records[key]=execution
            checks.append({'id':key,'status':'PASS','execution':execution_ref})
        accept=[];self.ac_check={}
        for item in req:
            if profile not in item['release_targets']:continue
            for ac in item['acceptance']:
                ck=scen[ac['test_id']]['check_id'];self.ac_check[ac['id']]=ck
                ob={'schema_version':'1.1','ac_id':ac['id'],'test_id':ac['test_id'],'candidate_id':cid,'spec_sha256':c['spec_sha256'],'status':'PASS','check_ids':[ck],
                    'observed':'SYNTHETIC envelope used only to exercise gate invariants, not product coverage.','raw_evidence':[self.records[ck]['stdout']]}
                accept.append({'id':ac['id'],'status':'PASS','check_ids':[ck],'observation':json_file(r,f'.hoh-dev/observations/{ac["id"]}.json',ob)})
        reports=[]
        for role,(key,name) in self.report_paths.items():
            art=next(a for a in self.records[key]['artifacts'] if a['name']==name);report=load_json(r/art['path'])
            reports.append({k:report[k] for k in ['role','session_id','candidate_id','spec_sha256','status']}|{'artifact':{'path':art['path'],'sha256':art['sha256']}})
        bundle=next(a for a in self.records['build']['artifacts'] if a['name']==names['release_bundle'])
        self.manifest={'schema_version':'1.1','baseline_id':policy['baseline_id'],'profile':profile,'requirements_sha256':sha256(r/'contracts/requirements.json'),
                       'candidate_id':cid,'candidate_manifest':cr,'spec_sha256':c['spec_sha256'],'developer_session_id':'developer-fixture',
                       'checks':checks,'acceptance':accept,'independent_reports':reports,'open_issues':[],'release_artifact':{'path':bundle['path'],'sha256':bundle['sha256']}}
        if prof['requires_soak']:
            t=next(a for a in self.records['soak']['artifacts'] if a['name']==names['soak_timeline'])
            self.manifest['soak']={'execution_check_id':'soak','live_iterations':20,'timeline':{'path':t['path'],'sha256':t['sha256']}}
    def check_row(self,key):return next(c for c in self.manifest['checks'] if c['id']==key)
    def edit_execution(self,key,**fields):
        row=self.check_row(key);obj=self.records[key];obj.update(fields);row['execution']=json_file(self.root,row['execution']['path'],obj)
    def rewrite_artifact(self,key,name,text):
        """Replace a produced artifact and re-bind hashes (simulates a self-consistent but altered record)."""
        obj=self.records[key];art=next(a for a in obj['artifacts'] if a['name']==name)
        (self.root/art['path']).write_text(text);art['sha256']=sha256(self.root/art['path'])
        self.edit_execution(key)
        if key=='soak' and name==self.policy['artifacts']['soak_timeline']:self.manifest['soak']['timeline']={'path':art['path'],'sha256':art['sha256']}
    def ac_row(self,ac_id):return next(a for a in self.manifest['acceptance'] if a['id']==ac_id)
    def edit_observation(self,ac_id=None,**fields):
        row=self.ac_row(ac_id) if ac_id else self.manifest['acceptance'][0]
        obj=load_json(self.root/row['observation']['path']);obj.update(fields)
        row['observation']=json_file(self.root,row['observation']['path'],obj)
    def set_ac(self,ac_id,status=None,check_ids=None,**fields):
        row=self.ac_row(ac_id)
        if status:row['status']=status;fields['status']=status
        if check_ids:row['check_ids']=check_ids;fields['check_ids']=check_ids
        self.edit_observation(ac_id,**fields)
    def rebind_report(self,which,report):
        """Rewrite a report and re-bind its hash in the check record (a self-consistent but altered record)."""
        row=next(x for x in self.manifest['independent_reports'] if x['role']==which);key,name=self.report_paths[which]
        self.rewrite_artifact(key,name,json.dumps(report))
        art=next(a for a in self.records[key]['artifacts'] if a['name']==name);row['artifact']={'path':art['path'],'sha256':art['sha256']}
    def edit_attempt(self,which,**fields):
        row=next(x for x in self.manifest['independent_reports'] if x['role']==which)
        report=load_json(self.root/row['artifact']['path']);attempt=self.attempts[which];attempt.update(fields)
        report['native_attempt']=json_file(self.root,report['native_attempt']['path'],attempt)
        self.rebind_report(which,report)
    def edit_report(self,which,**fields):
        row=next(x for x in self.manifest['independent_reports'] if x['role']==which)
        report=load_json(self.root/row['artifact']['path']);report.update(fields);self.rebind_report(which,report)
    def edit_test_results(self,key,lines):
        self.rewrite_artifact(key,self.policy['artifacts']['test_results'],''.join(json.dumps(x)+'\n' for x in lines))
    def degrade_check(self,key):
        row=self.check_row(key);row['status']='DEGRADED'
        row['degradation']={'reason':'Upstream plugin hook API incompatible with installed Pi version','upstream_ref':'example/upstream#1','diagnostic':[self.records[key]['stdout']]}
    def degrade_ac(self,ac_id,key):
        scenario_check=self.ac_check[ac_id];tid='TEST-'+ac_id[3:]
        art=next(a for a in self.records[scenario_check]['artifacts'] if a['name']==self.policy['artifacts']['test_results'])
        lines=[json.loads(l) for l in (self.root/art['path']).read_text().splitlines() if l.strip()]
        for l in lines:
            if l['test_id']==tid:l['status']='DEGRADED'
        self.edit_test_results(scenario_check,lines)
        self.set_ac(ac_id,status='DEGRADED',degradation={'reason':'Upstream plugin hook API incompatible with installed Pi version','upstream_ref':'example/upstream#1','diagnostic':[self.records[key]['stdout']]})
    def close(self):self.temp.cleanup()
