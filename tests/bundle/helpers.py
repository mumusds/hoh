"""Synthetic unit fixtures. NEVER exported as real product acceptance evidence."""
from pathlib import Path
import copy,json,os,shutil,subprocess,sys,tempfile
from datetime import datetime,timedelta,timezone
ROOT=Path(__file__).resolve().parents[2]
sys.path.insert(0,str(ROOT/'tools'))
from common import atomic_json,load_json,ref_for,sha256
from candidate import freeze

def json_file(root,path,data):
    p=root/path;atomic_json(p,data);return ref_for(p,root)

def init_repository(root):
    subprocess.run(['git','init','--quiet',str(root)],check=True)
    subprocess.run(['git','-C',str(root),'config','user.email','test@invalid.example'],check=True)
    subprocess.run(['git','-C',str(root),'config','user.name','HoH bundle fixture'],check=True)
    (root/'.gitignore').write_text('.hoh-dev/\n__pycache__/\n')
    (root/'source.txt').write_text('synthetic source\n',encoding='utf-8')
    (root/'spec.md').write_text('Synthetic specification. Not a real product.\n')
    (root/'plan.md').write_text('Synthetic test plan. Not real coverage.\n')
    subprocess.run(['git','-C',str(root),'add','.'],check=True)
    subprocess.run(['git','-C',str(root),'commit','--quiet','-m','synthetic test fixture'],check=True)
    json_file(root,'.hoh-dev/environment.json',{'mode':'SYNTHETIC_UNIT_FIXTURE'})
    c=freeze(root,root/'spec.md',root/'.hoh-dev/environment.json',root/'plan.md')
    cr=json_file(root,'.hoh-dev/candidate.json',c)
    return c,cr

class ReleaseFixture:
    """Positive envelope fixture only; no actual live run is claimed."""
    def __init__(self):
        self.temp=tempfile.TemporaryDirectory(prefix='hoh-gate-fixture-');self.root=Path(self.temp.name)
        r=self.root
        shutil.copytree(ROOT/'contracts',r/'contracts');shutil.copy(ROOT/'CONTRACT.md',r/'CONTRACT.md')
        c,cr=init_repository(r);self.candidate=c
        policy=load_json(r/'contracts/release-policy.json');req=load_json(r/'contracts/requirements.json')['requirements']
        start=datetime(2020,1,1,tzinfo=timezone.utc);duration=policy['minimum_soak_seconds'];end=start+timedelta(seconds=duration)
        checks=[];self.records={}
        for key in policy['required_checks']:
            p=f'.hoh-dev/evidence/{key}'
            out=r/p/'stdout.log';out.parent.mkdir(parents=True,exist_ok=True);out.write_text('SYNTHETIC UNIT FIXTURE ONLY: '+key+'\n')
            err=r/p/'stderr.log';err.write_text('')
            d=duration if key=='soak' else 1
            execution={'schema_version':'1.0','record_type':'command_execution','record_id':'fixture-'+key,'check_id':key,'candidate_id':c['candidate_id'],'spec_sha256':c['spec_sha256'],'candidate_manifest':cr,'source_unchanged':True,'environment_sha256':c['environment_sha256'],'test_plan_sha256':c['test_plan_sha256'],'mode':'live' if key in policy['required_modes'] else 'product','command':['SYNTHETIC_FIXTURE_NOT_EXECUTED',key],'cwd':str(r),'started_at':start.isoformat(),'finished_at':(start+timedelta(seconds=d)).isoformat(),'duration_seconds':d,'exit_code':0,'timed_out':False,'stdout':ref_for(out,r),'stderr':ref_for(err,r)}
            execution_ref=json_file(r,p+'/execution.json',execution);self.records[key]=execution
            checks.append({'id':key,'status':'PASS','execution':execution_ref})
        accept=[]
        for item in req:
            for ac in item['acceptance']:
                ob={'schema_version':'1.0','ac_id':ac['id'],'test_id':ac['test_id'],'candidate_id':c['candidate_id'],'spec_sha256':c['spec_sha256'],'status':'PASS','check_ids':['unit'],'observed':'SYNTHETIC envelope used only to exercise gate invariants, not product coverage.','raw_evidence':[self.records['unit']['stdout']]}
                accept.append({'id':ac['id'],'status':'PASS','check_ids':['unit'],'observation':json_file(r,f'.hoh-dev/observations/{ac["id"]}.json',ob)})
        reports=[]
        for role in ['qa','reviewer']:
            p=r/f'.hoh-dev/{role}-session.jsonl';p.write_text('{"synthetic_fixture_only":true}\n')
            report={'schema_version':'1.0','role':role,'session_id':role+'-independent-fixture','candidate_id':c['candidate_id'],'spec_sha256':c['spec_sha256'],'status':'PASS','scope':['SYNTHETIC schema test only'],'findings':[],'raw_session':ref_for(p,r)}
            reports.append({k:report[k] for k in ['role','session_id','candidate_id','spec_sha256','status']}|{'artifact':json_file(r,f'.hoh-dev/{role}-report.json',report)})
        timeline=r/'.hoh-dev/timeline.jsonl'
        events=[{'kind':'iteration.accepted','mode':'live','iteration_id':f'fixture-{i}','occurred_at':(start+timedelta(seconds=i*duration/19)).isoformat()} for i in range(20)]
        timeline.write_text(''.join(json.dumps(e)+'\n' for e in events))
        artifact=r/'.hoh-dev/synthetic-release.bin';artifact.write_bytes(b'SYNTHETIC_NOT_A_HOH_BINARY')
        self.manifest={'schema_version':'1.0','baseline_id':policy['baseline_id'],'requirements_sha256':sha256(r/'contracts/requirements.json'),'candidate_id':c['candidate_id'],'candidate_manifest':cr,'spec_sha256':c['spec_sha256'],'developer_session_id':'developer-fixture','checks':checks,'acceptance':accept,'independent_reports':reports,'open_issues':[],'soak':{'execution_check_id':'soak','live_iterations':20,'timeline':ref_for(timeline,r)},'release_artifact':ref_for(artifact,r)}
    def edit_execution(self,key,**fields):
        row=next(c for c in self.manifest['checks'] if c['id']==key)
        obj=self.records[key];obj.update(fields);row['execution']=json_file(self.root,row['execution']['path'],obj)
    def edit_observation(self,**fields):
        row=self.manifest['acceptance'][0];obj=load_json(self.root/row['observation']['path']);obj.update(fields)
        row['observation']=json_file(self.root,row['observation']['path'],obj)
    def close(self):self.temp.cleanup()
