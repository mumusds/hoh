#!/usr/bin/env python3
"""Fail-closed evidence consistency gate, NOT proof of software correctness.

All evidence paths are relative to the project root. Source records must come
from actual tests; access to the whole VM still permits intentional forgery.
The Rust product must implement equivalent or stricter invariants.
"""
from __future__ import annotations
import argparse, json, sys
from datetime import datetime
from pathlib import Path
from jsonschema import Draft202012Validator, FormatChecker
from common import ROOT, load_json, safe_path, sha256
from candidate import verify as verify_candidate
import subprocess

class GateError(ValueError): pass

def need(ok: bool, message: str) -> None:
    if not ok: raise GateError(message)

def validate_schema(root: Path, schema_name: str, data: dict) -> None:
    validator=Draft202012Validator(load_json(root/'contracts'/schema_name),format_checker=FormatChecker())
    errors=sorted(validator.iter_errors(data),key=lambda e: str(list(e.path)))
    if errors:
        e=errors[0];raise GateError(f'{schema_name} {list(e.path)}: {e.message}')

def verify_ref(root: Path, ref: dict, nonempty: bool=False) -> Path:
    path=safe_path(root,ref['path'])
    need(path.is_file(),f'missing artifact: {ref["path"]}')
    need(sha256(path)==ref['sha256'],f'artifact hash mismatch: {ref["path"]}')
    if nonempty: need(path.stat().st_size>0,f'empty artifact: {ref["path"]}')
    return path

def verify_baseline(root: Path) -> dict:
    lock=load_json(root/'contracts/baseline-lock.json')
    need(lock.get('baseline_id')=='HOH-BASELINE-1.0','baseline identity mismatch')
    need(bool(lock.get('files')),'baseline lock is empty')
    required=['CONTRACT.md','contracts/requirements.json','contracts/release-policy.json','contracts/release.schema.json','contracts/execution.schema.json','contracts/acceptance-observation.schema.json','contracts/independent-report.schema.json','contracts/candidate.schema.json']
    need(set(required)<=set(lock['files']),'baseline lock misses required contracts')
    for rel,digest in lock['files'].items():
        need(sha256(safe_path(root,rel))==digest,f'baseline contract changed: {rel}; formal CR required')
    return lock

def when(value: str) -> datetime:
    d=datetime.fromisoformat(value.replace('Z','+00:00'))
    need(d.tzinfo is not None,'timestamp must include timezone')
    return d

def unique_index(items: list, key: str, label: str) -> dict:
    index={v[key]:v for v in items}
    need(len(index)==len(items),f'duplicate {label}')
    return index

def validate_manifest(root: Path, manifest: dict) -> dict:
    root=root.resolve();verify_baseline(root)
    validate_schema(root,'release.schema.json',manifest)
    policy=load_json(root/'contracts/release-policy.json')
    requirements=load_json(root/'contracts/requirements.json')['requirements']
    need(manifest['requirements_sha256']==sha256(root/'contracts/requirements.json'),'requirements hash mismatch')
    need(manifest['baseline_id']==policy['baseline_id'],'wrong baseline')
    candidate=manifest['candidate_id'];spec=manifest['spec_sha256']
    candidate_data=load_json(verify_ref(root,manifest['candidate_manifest'],True))
    validate_schema(root,'candidate.schema.json',candidate_data)
    verify_candidate(root,candidate_data)
    need(candidate_data['candidate_id']==candidate and candidate_data['spec_sha256']==spec,'release candidate identity mismatch')
    checks=unique_index(manifest['checks'],'id','check id')
    need(set(policy['required_checks'])<=set(checks),'missing required checks')
    executions={}
    for check_id,check in checks.items():
        need(check['status']=='PASS',f'{check_id}: not PASS')
        execution=load_json(verify_ref(root,check['execution'],True))
        validate_schema(root,'execution.schema.json',execution)
        need(execution['candidate_manifest']==manifest['candidate_manifest'],'check uses different candidate manifest')
        need(execution['source_unchanged'] is True,f'{check_id}: source changed during verification')
        need(all(execution[k]==candidate_data[k] for k in ['environment_sha256','test_plan_sha256']),f'{check_id}: environment/test plan changed')
        need(execution['check_id']==check_id,f'{check_id}: wrong check binding')
        need(execution['candidate_id']==candidate and execution['spec_sha256']==spec,f'{check_id}: stale candidate/spec')
        need(execution['exit_code']==0 and not execution['timed_out'],f'{check_id}: command failed/timed out')
        need(execution['mode'] in ['live','product'],f'{check_id}: mock/bundle cannot certify a product release')
        expected=policy['required_modes'].get(check_id)
        if expected:need(execution['mode']==expected,f'{check_id}: needs {expected} execution')
        elapsed=(when(execution['finished_at'])-when(execution['started_at'])).total_seconds()
        need(elapsed>=0 and abs(elapsed-execution['duration_seconds'])<=max(5,elapsed*.01),f'{check_id}: inconsistent duration')
        verify_ref(root,execution['stdout']);verify_ref(root,execution['stderr'])
        executions[check_id]=execution
    need(len({e['record_id'] for e in executions.values()})==len(executions),'reused execution record id')
    accepted=unique_index(manifest['acceptance'],'id','AC id')
    expected_ac={ac['id']:ac for r in requirements if r['required_release'] for ac in r['acceptance']}
    need(set(accepted)==set(expected_ac),'AC list must match all required acceptance criteria exactly')
    for ac_id,entry in accepted.items():
        need(entry['status']=='PASS',f'{ac_id}: not PASS')
        need(set(entry['check_ids'])<=set(checks),f'{ac_id}: unknown check reference')
        observation=load_json(verify_ref(root,entry['observation'],True))
        validate_schema(root,'acceptance-observation.schema.json',observation)
        need(observation['ac_id']==ac_id and observation['test_id']==expected_ac[ac_id]['test_id'],f'{ac_id}: wrong AC/test binding')
        need(observation['candidate_id']==candidate and observation['spec_sha256']==spec,f'{ac_id}: stale observation')
        need(observation['status']=='PASS' and set(observation['check_ids'])==set(entry['check_ids']),f'{ac_id}: inconsistent observation')
        for ref in observation['raw_evidence']:verify_ref(root,ref,True)
    roles=unique_index(manifest['independent_reports'],'role','independent role')
    need(set(roles)=={'qa','reviewer'},'independent QA and reviewer required')
    session_ids={manifest['developer_session_id']}
    for role,entry in roles.items():
        need(entry['session_id'] not in session_ids,'developer/QA/reviewer sessions must be distinct')
        session_ids.add(entry['session_id'])
        need(entry['candidate_id']==candidate and entry['spec_sha256']==spec and entry['status']=='PASS',f'{role}: stale/failed report')
        actual=load_json(verify_ref(root,entry['artifact'],True))
        validate_schema(root,'independent-report.schema.json',actual)
        need(all(actual[k]==entry[k] for k in ['role','session_id','candidate_id','spec_sha256','status']),f'{role}: report binding mismatch')
        verify_ref(root,actual['raw_session'],True)
    blocking=[i['id'] for i in manifest['open_issues'] if i['severity'] in policy['required_zero_issue_severities']]
    need(not blocking,'blocking issues remain: '+','.join(blocking))
    soak=executions['soak'];need(soak['duration_seconds']>=policy['minimum_soak_seconds'],'soak duration below required threshold')
    timeline_path=verify_ref(root,manifest['soak']['timeline'],True)
    ids=set();times=[]
    with timeline_path.open('rb') as f:
        for line in f:
            if not line.strip():continue
            event=json.loads(line)
            if event.get('kind')=='iteration.accepted' and event.get('mode')=='live':
                need(bool(event.get('iteration_id')),'soak iteration missing identity')
                need(event['iteration_id'] not in ids,'duplicate accepted soak iteration')
                ids.add(event['iteration_id']);times.append(when(event['occurred_at']))
    need(len(ids)==manifest['soak']['live_iterations'],'soak iteration count does not match raw timeline')
    need(len(ids)>=policy['minimum_live_iterations'],'insufficient live iterations')
    need(all(when(soak['started_at'])<=t<=when(soak['finished_at']) for t in times),'soak events outside execution window')
    artifact=verify_ref(root,manifest['release_artifact'],True)
    return {'status':'PASS','gate':'evidence-consistency','candidate_id':candidate,'required_acceptance':len(expected_ac),'checks':len(checks),'release_sha256':sha256(artifact),'notice':'This verifies evidence consistency, not the truthfulness of arbitrary forged records or absence of undiscovered defects.'}

def main(argv=None) -> int:
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--manifest',type=Path,required=True)
    a=p.parse_args(argv)
    try:result=validate_manifest(ROOT,load_json(a.manifest))
    except (ValueError,KeyError,TypeError,OSError,subprocess.SubprocessError) as exc:
        print(json.dumps({'status':'FAIL','error':str(exc)},ensure_ascii=False),file=sys.stderr);return 1
    print(json.dumps(result,ensure_ascii=False,indent=2));return 0
if __name__=='__main__':sys.exit(main())
