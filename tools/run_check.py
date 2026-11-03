#!/usr/bin/env python3
"""Run one check of the frozen candidate's check plan, recording raw evidence.

The command, mode, cwd and expected artifacts come ONLY from the check plan
(contracts/check-plan.schema.json) that the candidate froze as its test plan.
A command given after `--` is accepted only if it equals the planned argv, so
evidence cannot silently claim a different mode or command than was reviewed.
"""
from __future__ import annotations
import argparse,json,os,sys,uuid
from pathlib import Path
from jsonschema import Draft202012Validator
from common import ROOT,Terminated,atomic_json,execute,install_termination_handlers,load_json,ref_for,restore_handlers,running_as_root,safe_path,utcnow
# Longer than agent_task's own 2 s child grace, so a nested bridge can stop its agent and write its record first.
CHILD_GRACE=15.0
from candidate import verify
PLAN_SCHEMA=Path(__file__).resolve().parents[1]/'contracts/check-plan.schema.json'

def load_plan(root:Path,candidate:dict)->dict:
    path=safe_path(root,candidate['test_plan']['path'])
    try:plan=load_json(path)
    except ValueError as exc:raise ValueError('candidate test plan must be a check-plan JSON file: '+str(exc)) from None
    errors=list(Draft202012Validator(load_json(PLAN_SCHEMA)).iter_errors(plan))
    if errors:raise ValueError('invalid check plan: '+errors[0].message)
    return plan

def boot_id()->str:
    try:return Path('/proc/sys/kernel/random/boot_id').read_text().strip()
    except OSError:return 'unknown'

def main(argv=None):
    p=argparse.ArgumentParser(description=__doc__,formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument('--check-id',required=True);p.add_argument('--candidate-file',type=Path,required=True)
    p.add_argument('--out',type=Path,required=True);p.add_argument('--timeout',type=float,help='Overrides the plan timeout_seconds.')
    p.add_argument('command',nargs=argparse.REMAINDER,help='Optional; must equal the planned argv.')
    a=p.parse_args(argv);cmd=a.command[1:] if a.command[:1]==['--'] else a.command
    if a.timeout is not None and a.timeout<=0:p.error('timeout must be positive')
    out=a.out.resolve()
    if not out.is_relative_to(ROOT/'.hoh-dev'):p.error('evidence must be inside ignored .hoh-dev/')
    if out.exists():p.error('evidence directory already exists; evidence is append-only, choose a new --out')
    try:
        candidate_file=a.candidate_file.resolve()
        if not candidate_file.is_relative_to(ROOT.resolve()):raise ValueError('candidate manifest must be inside the project (normally .hoh-dev/candidates/)')
        candidate=load_json(candidate_file)
        verify(ROOT,candidate);plan=load_plan(ROOT,candidate)
        entry=plan['checks'].get(a.check_id)
        if entry is None:raise ValueError(f'check {a.check_id!r} is not in the frozen check plan')
        if cmd and cmd!=entry['argv']:raise ValueError('command differs from the frozen check plan argv')
        cwd=safe_path(ROOT,entry.get('cwd','.'))
        if not cwd.is_dir():raise ValueError('planned cwd is not a directory')
    except (ValueError,KeyError,OSError) as e:print('Refusing to run check: '+str(e),file=sys.stderr);return 1
    timeout=a.timeout or float(entry.get('timeout_seconds',3600))
    record_id=str(uuid.uuid4());artifact_dir=out/'artifacts';artifact_dir.mkdir(parents=True)
    env=os.environ|{'HOH_EVIDENCE_DIR':str(artifact_dir),'HOH_CHECK_ID':a.check_id,'HOH_CANDIDATE_ID':candidate['candidate_id'],'HOH_SPEC_SHA256':candidate['spec_sha256'],
         'HOH_CANDIDATE_FILE':str(candidate_file),'HOH_CHECK_RECORD_ID':record_id}
    started=utcnow();interrupted=False;previous=install_termination_handlers()
    try:rc,timed,duration=execute(entry['argv'],cwd,out/'stdout.log',out/'stderr.log',timeout,env=env,grace=CHILD_GRACE)
    except OSError as exc:
        (out/'stdout.log').write_bytes(b'');(out/'stderr.log').write_text(str(exc));rc,timed,duration=127,False,0
    except (KeyboardInterrupt,Terminated):rc,timed,duration=130,False,0;interrupted=True
    finally:restore_handlers(previous)
    for name in ('stdout.log','stderr.log'):
        if not (out/name).exists():(out/name).write_bytes(b'')
    finished=utcnow();unchanged=True
    try:verify(ROOT,candidate)
    except Exception as exc:unchanged=False;print('Candidate changed: '+str(exc),file=sys.stderr)
    artifacts=[];missing=[]
    for name in entry.get('artifacts',[]):
        f=artifact_dir/name
        if f.is_file() and not f.is_symlink():artifacts.append({'name':name}|ref_for(f))
        else:missing.append(name)
    record={'schema_version':'1.1','record_type':'command_execution','record_id':record_id,'producer_attempt_id':record_id,
            'check_id':a.check_id,'candidate_id':candidate['candidate_id'],'spec_sha256':candidate['spec_sha256'],'candidate_manifest':ref_for(candidate_file),
            'source_unchanged':unchanged,'environment_sha256':candidate['environment_sha256'],'test_plan_sha256':candidate['test_plan_sha256'],
            'mode':entry['mode'],'command':entry['argv'],'cwd':str(cwd.relative_to(ROOT.resolve())) or '.','started_at':started,'finished_at':finished,
            'duration_seconds':duration,'exit_code':rc,'timed_out':timed,'stdout':ref_for(out/'stdout.log'),'stderr':ref_for(out/'stderr.log'),
            'artifacts':artifacts,'missing_artifacts':missing,'runner':{'tool':'tools/run_check.py','euid':os.geteuid() if hasattr(os,'geteuid') else -1,'boot_id':boot_id()}}
    atomic_json(out/'execution.json',record);print(json.dumps(ref_for(out/'execution.json'),ensure_ascii=False))
    if missing:print('Declared artifacts missing: '+', '.join(missing),file=sys.stderr)
    if running_as_root():print('WARNING: check ran as root; release evidence should come from the non-root development user.',file=sys.stderr)
    if interrupted:return 130
    if rc==0 and unchanged and not missing:return 0
    return rc if 1<=rc<=125 else 1
if __name__=='__main__':sys.exit(main())
