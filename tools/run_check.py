#!/usr/bin/env python3
"""Run an actual command against a frozen candidate, recording raw evidence."""
from __future__ import annotations
import argparse,json,sys,uuid
from pathlib import Path
from common import ROOT,atomic_json,execute,load_json,ref_for,utcnow
from candidate import verify

def main(argv=None):
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--check-id',required=True);p.add_argument('--candidate-file',type=Path,required=True)
    p.add_argument('--mode',choices=['bundle','mock','product','live'],required=True)
    p.add_argument('--cwd',type=Path,default=ROOT);p.add_argument('--out',type=Path,required=True);p.add_argument('--timeout',type=float,default=3600)
    p.add_argument('command',nargs=argparse.REMAINDER)
    a=p.parse_args(argv);cmd=a.command[1:] if a.command[:1]==['--'] else a.command
    if not cmd or a.timeout<=0:p.error('command and positive timeout required')
    out=a.out.resolve()
    if not out.is_relative_to(ROOT/'.hoh-dev'):p.error('evidence must be inside ignored .hoh-dev/')
    candidate=load_json(a.candidate_file)
    try:verify(ROOT,candidate)
    except Exception as e:print('Candidate invalid before execution: '+str(e),file=sys.stderr);return 1
    out.mkdir(parents=True,exist_ok=False);started=utcnow();interrupted=False
    try:rc,timed,duration=execute(cmd,a.cwd.resolve(),out/'stdout.log',out/'stderr.log',a.timeout)
    except OSError as exc:
        (out/'stdout.log').write_bytes(b'');(out/'stderr.log').write_text(str(exc));rc,timed,duration=127,False,0
    except KeyboardInterrupt:rc,timed,duration=130,False,0;interrupted=True
    unchanged=True
    try:verify(ROOT,candidate)
    except Exception as exc:unchanged=False;print('Candidate changed: '+str(exc),file=sys.stderr)
    record={'schema_version':'1.0','record_type':'command_execution','record_id':str(uuid.uuid4()),'check_id':a.check_id,'candidate_id':candidate['candidate_id'],'spec_sha256':candidate['spec_sha256'],'candidate_manifest':ref_for(a.candidate_file),'source_unchanged':unchanged,'environment_sha256':candidate['environment_sha256'],'test_plan_sha256':candidate['test_plan_sha256'],'mode':a.mode,'command':cmd,'cwd':str(a.cwd.resolve()),'started_at':started,'finished_at':utcnow(),'duration_seconds':duration,'exit_code':rc,'timed_out':timed,'stdout':ref_for(out/'stdout.log'),'stderr':ref_for(out/'stderr.log')}
    atomic_json(out/'execution.json',record);print(json.dumps(ref_for(out/'execution.json'),ensure_ascii=False))
    return 130 if interrupted else (0 if rc==0 and unchanged else (rc if 1<=rc<=125 else 1))
if __name__=='__main__':sys.exit(main())
