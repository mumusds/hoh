#!/usr/bin/env python3
"""Probe versions/help without claiming auth or executing model requests by default."""
from __future__ import annotations
import argparse,json,os,platform,shutil,subprocess,sys
from pathlib import Path
from common import ROOT,atomic_json,flag_supported,sha256,utcnow
from agent_task import REQUIRED

def probe(name,args=None):
    path=shutil.which(name)
    if not path:return {'available':False,'error':'NOT_INSTALLED'}
    try:
        p=subprocess.run([name]+(args or ['--version']),stdout=subprocess.PIPE,stderr=subprocess.STDOUT,timeout=20)
        return {'available':p.returncode==0,'path':path,'exit_code':p.returncode,'text':p.stdout.decode('utf-8','replace').strip()}
    except (OSError,subprocess.TimeoutExpired) as e:return {'available':False,'path':path,'error':str(e)}

def report():
    result={'schema_version':'1.0','at':utcnow(),'system':platform.platform(),'python':platform.python_version(),'non_root':not hasattr(os,'geteuid') or os.geteuid()!=0,'probes':{},'authentication':'NOT_CHECKED','live_integration':'NOT_RUN','product_acceptance':False}
    for name in ['git','node','npm','cargo','rustc','codex','claude','pi']:result['probes'][name]=probe(name)
    for name in REQUIRED:
        cap=probe(name,['exec','--help'] if name=='codex' else ['--help'])
        text=cap.get('text','');cap['missing_flags']=[f for f in REQUIRED[name] if not flag_supported(text,f)]
        cap['available']=cap['available'] and not cap['missing_flags'];result['probes'][name+'_capabilities']=cap
    node=result['probes']['node']
    try:
        major,minor,*_=map(int,node.get('text','').strip().lstrip('v').split('.'))
        node['supported']=major>22 or (major==22 and minor>=19)
    except ValueError:node['supported']=False
    plugin_lock=ROOT/'.hoh-dev/plugin-lock.json'
    result['plugin_installation_records']=json.loads(plugin_lock.read_text()).get('plugins',{}) if plugin_lock.exists() else {}
    result['plugin_runtime_compatibility']='NOT_VERIFIED_BY_PREFLIGHT'
    result['ready_for_native_smoke']=all(x['available'] for x in result['probes'].values()) and node['supported'] and result['non_root']
    return result

def main(argv=None):
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path);p.add_argument('--live',action='store_true',help='Opt-in: invokes authenticated native models and can incur charges.');a=p.parse_args(argv)
    result=report()
    if a.live and result['ready_for_native_smoke']:
        import tempfile
        # Dedicated scratch repository avoids smoke requests editing the product.
        (ROOT/'.hoh-dev').mkdir(exist_ok=True)
        d=Path(tempfile.mkdtemp(prefix='native-smoke-',dir=ROOT/'.hoh-dev'))
        subprocess.run(['git','init','--quiet',str(d)],check=True)
        prompt=d/'smoke.md';prompt.write_text('Respond with HOH_NATIVE_SMOKE_OK. Do not edit files, call tools, or perform other tasks.\n')
        outcomes={}
        for agent in REQUIRED:
            rc=subprocess.run([sys.executable,str(ROOT/'tools/agent_task.py'),'--agent',agent,'--role','smoke','--prompt-file',str(prompt),'--cwd',str(d),'--output-dir',str(d/agent),'--timeout','180']).returncode
            record=json.loads((d/agent/'attempt.json').read_text()) if (d/agent/'attempt.json').is_file() else {}
            token='HOH_NATIVE_SMOKE_OK' in record.get('assistant_text_tail','')
            outcomes[agent]={'exit_code':rc if rc else (0 if token else 1),'native_turn_completed':rc==0,'expected_reply_observed':token}
        result['native_smoke']=outcomes;result['authentication']='SMOKE_REQUESTS_ATTEMPTED';result['live_integration']='SMOKE_ONLY_NOT_FULL_ACCEPTANCE'
    if a.output:atomic_json(a.output,result)
    print(json.dumps(result,ensure_ascii=False,indent=2))
    return 0 if result['ready_for_native_smoke'] and all(v['exit_code']==0 for v in result.get('native_smoke',{}).values()) else 3
if __name__=='__main__':sys.exit(main())
