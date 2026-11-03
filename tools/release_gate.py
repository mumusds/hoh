#!/usr/bin/env python3
"""Fail-closed evidence consistency gate, NOT proof of software correctness.

All evidence paths are relative to the project root. Every execution record is
compared with the check plan frozen in the candidate (command, mode, cwd,
artifacts); every acceptance criterion must cite the check its scenario names
and raw evidence produced by that check. Access to the whole VM still permits
intentional forgery; independent QA/review must inspect what the checks cover.
The Rust product must implement equivalent or stricter invariants.
"""
from __future__ import annotations
import argparse, json, re, subprocess, sys
from datetime import datetime
from pathlib import Path
from jsonschema import Draft202012Validator, FormatChecker
from common import ROOT, load_json, safe_path, sha256
from candidate import verify as verify_candidate

class GateError(ValueError): pass

# Contracts that define what "released" means. Changing any of them requires a CR (tools/baseline.py).
LOCK_REQUIRED=['CONTRACT.md','contracts/requirements.json','contracts/acceptance-scenarios.json','contracts/release-policy.json',
    'contracts/release.schema.json','contracts/execution.schema.json','contracts/acceptance-observation.schema.json',
    'contracts/independent-report.schema.json','contracts/candidate.schema.json','contracts/check-plan.schema.json',
    'contracts/live-session.schema.json','contracts/soak-event.schema.json','contracts/native-attempt.schema.json','contracts/test-result.schema.json',
    'docs/10-acceptance.md','tools/release_gate.py','tools/candidate.py','tools/run_check.py','tools/common.py','tools/baseline.py']
REPORT_CHECK={'qa':'independent-qa','reviewer':'independent-review'}
EVIDENCE_TOOLS={'tools/run_check.py','hoh'}
CR_FIELDS={'id':r'^ID[:：]\s*(CR-\d{3,})\s*$','status':r'^状态[:：]\s*(\S+)\s*$','approver':r'^批准人[:：]\s*(.+?)\s*$'}

def need(ok: bool, message: str) -> None:
    if not ok: raise GateError(message)

def schema_errors(root: Path, schema_name: str, data) -> list:
    validator=Draft202012Validator(load_json(root/'contracts'/schema_name),format_checker=FormatChecker())
    return sorted(validator.iter_errors(data),key=lambda e: str(list(e.path)))

def validate_schema(root: Path, schema_name: str, data) -> None:
    errors=schema_errors(root,schema_name,data)
    if errors:
        e=errors[0];raise GateError(f'{schema_name} {list(e.path)}: {e.message}')

def verify_ref(root: Path, ref: dict, nonempty: bool=False) -> Path:
    path=safe_path(root,ref['path'])
    need(path.is_file(),f'missing artifact: {ref["path"]}')
    need(sha256(path)==ref['sha256'],f'artifact hash mismatch: {ref["path"]}')
    if nonempty: need(path.stat().st_size>0,f'empty artifact: {ref["path"]}')
    return path

def parse_cr(path: Path) -> dict:
    text=path.read_text(encoding='utf-8');found={}
    for key,pattern in CR_FIELDS.items():
        m=re.search(pattern,text,re.M)
        if not m:raise ValueError(f'{path}: missing "{pattern}" line')
        found[key]=m.group(1)
    if found['status']!='APPROVED':raise ValueError(f'{path}: CR status is {found["status"]}, not APPROVED')
    return found

def check_records(root: Path, lock: dict) -> list[str]:
    errors=[];seen=set()
    for cr in lock.get('change_requests',[]):
        try:
            info=parse_cr(safe_path(root,cr['path']))
            if info['id']!=cr['id']:errors.append(f'{cr["path"]}: id {info["id"]} != lock entry {cr["id"]}')
            if cr['id'] in seen:errors.append('duplicate CR '+cr['id'])
            seen.add(cr['id'])
        except (ValueError,OSError) as exc:errors.append(str(exc))
    if lock.get('revision',1)>1 and not lock.get('change_requests'):errors.append('baseline revision >1 without change requests')
    return errors

def verify_baseline(root: Path) -> dict:
    lock=load_json(root/'contracts/baseline-lock.json')
    need(isinstance(lock.get('baseline_id'),str) and lock['baseline_id'].startswith('HOH-BASELINE-'),'baseline identity missing')
    need(bool(lock.get('files')),'baseline lock is empty')
    need(set(LOCK_REQUIRED)<=set(lock['files']),'baseline lock misses required contracts: '+', '.join(sorted(set(LOCK_REQUIRED)-set(lock['files']))))
    for rel,digest in lock['files'].items():
        need(sha256(safe_path(root,rel))==digest,f'baseline contract changed: {rel}; formal CR required (tools/baseline.py)')
    for cr in lock.get('change_requests',[]):
        path=safe_path(root,cr['path'])
        need(path.is_file() and sha256(path)==cr['sha256'],f'change request record missing or modified: {cr["path"]}')
    errors=check_records(root,lock)
    need(not errors,'change request records invalid: '+'; '.join(errors))
    return lock

def when(value: str) -> datetime:
    d=datetime.fromisoformat(value.replace('Z','+00:00'))
    need(d.tzinfo is not None,'timestamp must include timezone')
    return d

def unique_index(items: list, key: str, label: str) -> dict:
    index={v[key]:v for v in items}
    need(len(index)==len(items),f'duplicate {label}')
    return index

def inside(root: Path, path: str, directory: Path) -> bool:
    """Resolved containment: '..' segments and symlinks cannot leave the directory."""
    try:return safe_path(root,path).is_relative_to(directory)
    except ValueError:return False

def load_test_results(root: Path, ref: dict) -> dict[str,list[str]]:
    """test_id -> statuses of every executed test case that covers it (CR-003 per-test evidence)."""
    found={}
    with verify_ref(root,ref).open('rb') as f:
        for number,line in enumerate(f,1):
            if not line.strip():continue
            try:item=json.loads(line)
            except ValueError:raise GateError(f'{ref["path"]} line {number}: invalid JSON') from None
            errors=schema_errors(root,'test-result.schema.json',item)
            need(not errors,f'{ref["path"]} line {number}: '+(errors[0].message if errors else ''))
            found.setdefault(item['test_id'],[]).append(item['status'])
    return found

def produced_ref(execution: dict, name: str) -> dict|None:
    ref=next((a for a in execution['artifacts'] if a['name']==name),None)
    return {'path':ref['path'],'sha256':ref['sha256']} if ref else None

def validate_manifest(root: Path, manifest: dict) -> dict:
    root=root.resolve();lock=verify_baseline(root)
    validate_schema(root,'release.schema.json',manifest)
    policy=load_json(root/'contracts/release-policy.json')
    need(manifest['baseline_id']==policy['baseline_id']==lock['baseline_id'],'wrong baseline')
    need(manifest['profile'] in policy['profiles'],'unknown release profile: '+manifest['profile'])
    profile_id=manifest['profile'];profile=policy['profiles'][profile_id]
    requirements=load_json(root/'contracts/requirements.json')['requirements']
    scenarios={s['id']:s for s in load_json(root/'contracts/acceptance-scenarios.json')['scenarios']}
    need(manifest['requirements_sha256']==sha256(root/'contracts/requirements.json'),'requirements hash mismatch')
    candidate=manifest['candidate_id'];spec=manifest['spec_sha256']
    candidate_data=load_json(verify_ref(root,manifest['candidate_manifest'],True))
    validate_schema(root,'candidate.schema.json',candidate_data)
    verify_candidate(root,candidate_data)
    need(candidate_data['candidate_id']==candidate and candidate_data['spec_sha256']==spec,'release candidate identity mismatch')
    need(any(f['path']==candidate_data['test_plan']['path'] for f in candidate_data['source_files']),'check plan must be a committed file of the candidate (reviewable in Git)')
    try:plan=load_json(safe_path(root,candidate_data['test_plan']['path']))
    except ValueError:raise GateError('candidate test plan is not a check-plan JSON file') from None
    validate_schema(root,'check-plan.schema.json',plan)

    checks=unique_index(manifest['checks'],'id','check id')
    need(set(profile['required_checks'])<=set(checks),'missing required checks: '+', '.join(sorted(set(profile['required_checks'])-set(checks))))
    executions={};exec_dirs={};degraded_checks=[]
    for check_id,check in checks.items():
        need(check_id in policy['checks'],f'{check_id}: not a release-policy check')
        rule=policy['checks'][check_id]
        need(check['status']=='PASS' or (check['status']=='DEGRADED' and rule.get('degradable') is True),f'{check_id}: not PASS')
        execution_path=verify_ref(root,check['execution'],True)
        execution=load_json(execution_path)
        validate_schema(root,'execution.schema.json',execution)
        need(check_id in plan['checks'],f'{check_id}: not declared in the frozen check plan')
        planned=plan['checks'][check_id]
        need(planned['mode']==rule['mode'],f'{check_id}: check plan mode {planned["mode"]} but policy needs {rule["mode"]}')
        need(execution['mode']==planned['mode'],f'{check_id}: needs {planned["mode"]} execution')
        need(execution['command']==planned['argv'],f'{check_id}: executed command differs from the frozen check plan')
        need(Path(execution['cwd'])==Path(planned.get('cwd','.')),f'{check_id}: executed in a different cwd than planned')
        need(execution['runner']['tool'] in EVIDENCE_TOOLS and execution['runner']['euid']!=0,f'{check_id}: evidence must come from run_check/hoh as a non-root user')
        need(execution['candidate_manifest']==manifest['candidate_manifest'],'check uses different candidate manifest')
        need(execution['source_unchanged'] is True,f'{check_id}: source changed during verification')
        need(all(execution[k]==candidate_data[k] for k in ['environment_sha256','test_plan_sha256']),f'{check_id}: environment/test plan changed')
        need(execution['check_id']==check_id,f'{check_id}: wrong check binding')
        need(execution['candidate_id']==candidate and execution['spec_sha256']==spec,f'{check_id}: stale candidate/spec')
        need(not execution['timed_out'],f'{check_id}: command timed out')
        need(execution['exit_code']==0 or check['status']=='DEGRADED',f'{check_id}: command failed')
        need(not execution['missing_artifacts'],f'{check_id}: declared artifacts missing')
        need({a['name'] for a in execution['artifacts']}==set(planned.get('artifacts',[])),f'{check_id}: artifacts differ from the check plan')
        elapsed=(when(execution['finished_at'])-when(execution['started_at'])).total_seconds()
        need(elapsed>=0 and abs(elapsed-execution['duration_seconds'])<=max(5,elapsed*.01),f'{check_id}: inconsistent duration')
        directory=execution_path.parent
        for other_id,other in exec_dirs.items():
            need(directory!=other and not directory.is_relative_to(other) and not other.is_relative_to(directory),f'{check_id}: execution directory overlaps {other_id}')
        for ref in [execution['stdout'],execution['stderr']]+execution['artifacts']:
            need(inside(root,ref['path'],directory),f'{check_id}: evidence outside its execution directory')
            verify_ref(root,ref)
        if check['status']=='DEGRADED':
            degraded_checks.append(check_id)
            for ref in check['degradation']['diagnostic']:
                need(inside(root,ref['path'],directory),f'{check_id}: degradation diagnostic is not output of this check')
                verify_ref(root,ref,True)
        if execution['mode']=='live':
            name=policy['artifacts']['live_sessions']
            ref=next((a for a in execution['artifacts'] if a['name']==name),None)
            need(ref is not None,f'{check_id}: live check lacks {name}')
            sessions=load_json(safe_path(root,ref['path']))
            validate_schema(root,'live-session.schema.json',sessions)
            need(sessions['check_id']==check_id and sessions['harness_candidate_id']==candidate,f'{check_id}: live sessions bound to another check/candidate')
            execution['_live_session_ids']={s['session_id'] for s in sessions['sessions']}
            agents={s['agent'] for s in sessions['sessions']}
            required_agents=set(profile.get('live_agents',{}).get(check_id,[]))
            if check['status']=='PASS':
                need(required_agents<=agents,f'{check_id}: live sessions lack required agents: '+', '.join(sorted(required_agents-agents)))
                if rule.get('requires_plugins'):
                    need(all(s.get('plugins') for s in sessions['sessions'] if s['agent']=='pi'),f'{check_id}: every Pi session must list the plugins it loaded')
        results=produced_ref(execution,policy['artifacts']['test_results'])
        if results:execution['_test_results']=load_test_results(root,results)
        executions[check_id]=execution;exec_dirs[check_id]=directory
    need(len({e['record_id'] for e in executions.values()})==len(executions),'reused execution record id')

    accepted=unique_index(manifest['acceptance'],'id','AC id')
    in_scope=[r for r in requirements if profile_id in r['release_targets']]
    expected_ac={ac['id']:(r,ac) for r in in_scope for ac in r['acceptance']}
    need(set(accepted)==set(expected_ac),'AC list must match all acceptance criteria of the profile exactly')
    degraded_ac=[]
    for ac_id,entry in accepted.items():
        req,ac=expected_ac[ac_id];scenario=scenarios.get(ac['test_id'])
        need(scenario is not None and scenario['acceptance_id']==ac_id,f'{ac_id}: no scenario for {ac["test_id"]}')
        degradable=req['release_class']=='degradable'
        need(entry['status']=='PASS' or (entry['status']=='DEGRADED' and degradable),f'{ac_id}: not PASS')
        need(set(entry['check_ids'])<=set(checks),f'{ac_id}: unknown check reference')
        need(scenario['check_id'] in entry['check_ids'],f'{ac_id}: scenario requires check {scenario["check_id"]}')
        cited_degraded=[c for c in entry['check_ids'] if checks[c]['status']=='DEGRADED']
        if entry['status']=='PASS':need(not cited_degraded,f'{ac_id}: cannot PASS on degraded check(s) {", ".join(cited_degraded)}')
        else:
            need(checks[scenario['check_id']]['status']=='DEGRADED',f'{ac_id}: DEGRADED although its scenario check {scenario["check_id"]} passed')
            degraded_ac.append(ac_id)
        observation=load_json(verify_ref(root,entry['observation'],True))
        validate_schema(root,'acceptance-observation.schema.json',observation)
        need(observation['ac_id']==ac_id and observation['test_id']==ac['test_id'],f'{ac_id}: wrong AC/test binding')
        need(observation['candidate_id']==candidate and observation['spec_sha256']==spec,f'{ac_id}: stale observation')
        need(observation['status']==entry['status'] and set(observation['check_ids'])==set(entry['check_ids']),f'{ac_id}: inconsistent observation')
        allowed=[exec_dirs[c] for c in entry['check_ids']]
        refs=observation['raw_evidence']+(observation['degradation']['diagnostic'] if entry['status']=='DEGRADED' else [])
        for ref in refs:
            need(any(inside(root,ref['path'],d) for d in allowed),f'{ac_id}: raw evidence is not output of its cited checks')
            verify_ref(root,ref,True)
        need(any(inside(root,ref['path'],exec_dirs[scenario['check_id']]) for ref in observation['raw_evidence']),f'{ac_id}: no raw evidence from its scenario check {scenario["check_id"]}')
        results=executions[scenario['check_id']].get('_test_results')
        need(results is not None,f'{ac_id}: scenario check {scenario["check_id"]} produced no {policy["artifacts"]["test_results"]}')
        statuses=results.get(ac['test_id'],[])
        need(bool(statuses),f'{ac_id}: no test result for {ac["test_id"]} in check {scenario["check_id"]}')
        allowed_status={'PASS'} if entry['status']=='PASS' else {'PASS','DEGRADED'}
        need(set(statuses)<=allowed_status and (entry['status']=='PASS' or 'DEGRADED' in statuses),
             f'{ac_id}: test results for {ac["test_id"]} are {sorted(set(statuses))}, not {entry["status"]}')

    roles=unique_index(manifest['independent_reports'],'role','independent role')
    need(set(roles)=={'qa','reviewer'},'independent QA and reviewer required')
    session_ids={manifest['developer_session_id']}
    for role,entry in roles.items():
        check_id=REPORT_CHECK[role];need(check_id in executions,f'{role}: {check_id} check missing')
        directory=exec_dirs[check_id]
        need(entry['session_id'] not in session_ids,'developer/QA/reviewer sessions must be distinct')
        session_ids.add(entry['session_id'])
        need(entry['candidate_id']==candidate and entry['spec_sha256']==spec and entry['status']=='PASS',f'{role}: stale/failed report')
        need(entry['artifact']==produced_ref(executions[check_id],policy['artifacts']['independent_report']),
             f'{role}: report is not the {policy["artifacts"]["independent_report"]} produced by the {check_id} check')
        actual=load_json(verify_ref(root,entry['artifact'],True))
        validate_schema(root,'independent-report.schema.json',actual)
        need(all(actual[k]==entry[k] for k in ['role','session_id','candidate_id','spec_sha256','status']),f'{role}: report binding mismatch')
        for ref in [entry['artifact'],actual['raw_session'],actual['native_attempt']]:
            need(inside(root,ref['path'],directory),f'{role}: report material is not output of the {check_id} check')
        raw=verify_ref(root,actual['raw_session'],True)
        attempt=load_json(verify_ref(root,actual['native_attempt'],True))
        validate_schema(root,'native-attempt.schema.json',attempt)
        need(attempt['role']==role,f'{role}: native attempt was run for another role')
        need(attempt['status'] in policy['accepted_attempt_statuses'],f'{role}: native attempt did not complete')
        need(attempt['candidate_id']==candidate,f'{role}: native attempt ran for another candidate')
        need(actual['session_id'] in attempt['session_ids'],f'{role}: report session not observed in the native attempt')
        need(manifest['developer_session_id'] not in attempt['session_ids'],f'{role}: native attempt reused the developer session')
        need(sha256(raw)==attempt['stdout_sha256'],f'{role}: raw session is not the native attempt output')
        need(actual['session_id'] in executions[check_id].get('_live_session_ids',set()),f'{role}: report session not listed in {check_id} live sessions')
        blocking=[f['id'] for f in actual['findings'] if f['severity'] in policy['required_zero_issue_severities']]
        need(not blocking,f'{role}: report has blocking findings: '+', '.join(blocking))
    blocking=[i['id'] for i in manifest['open_issues'] if i['severity'] in policy['required_zero_issue_severities']]
    need(not blocking,'blocking issues remain: '+','.join(blocking))

    if profile['requires_soak']:
        need('soak' in manifest and 'soak' in executions,'soak evidence required by profile')
        soak=executions['soak'];need(checks['soak']['status']=='PASS','soak: not PASS')
        wall=(when(soak['finished_at'])-when(soak['started_at'])).total_seconds()
        need(min(wall,soak['duration_seconds'])>=policy['minimum_soak_seconds'],'soak duration below required threshold')
        name=policy['artifacts']['soak_timeline']
        produced=next((a for a in soak['artifacts'] if a['name']==name),None)
        need(produced is not None,f'soak check did not produce {name}')
        need(manifest['soak']['timeline']=={'path':produced['path'],'sha256':produced['sha256']},'soak timeline is not the one produced by the soak check')
        ids=set();previous=None
        with verify_ref(root,manifest['soak']['timeline'],True).open('rb') as f:
            for number,line in enumerate(f,1):
                if not line.strip():continue
                try:event=json.loads(line)
                except ValueError:raise GateError(f'soak timeline line {number}: invalid JSON') from None
                errors=schema_errors(root,'soak-event.schema.json',event)
                need(not errors,f'soak timeline line {number}: '+(errors[0].message if errors else ''))
                need(event['harness_candidate_id']==candidate,f'soak timeline line {number}: different harness candidate')
                at=when(event['occurred_at']);need(previous is None or at>=previous,f'soak timeline line {number}: out of order');previous=at
                need(when(soak['started_at'])<=at<=when(soak['finished_at']),'soak events outside execution window')
                if event['kind']=='iteration.accepted' and event['mode']=='live':
                    need(event['iteration_id'] not in ids,'duplicate accepted soak iteration')
                    ids.add(event['iteration_id'])
        need(len(ids)==manifest['soak']['live_iterations'],'soak iteration count does not match raw timeline')
        need(len(ids)>=policy['minimum_live_iterations'],'insufficient live iterations')
    need('build' in executions and manifest['release_artifact']==produced_ref(executions['build'],policy['artifacts']['release_bundle']),
         f'release artifact must be the {policy["artifacts"]["release_bundle"]} produced by the build check')
    artifact=verify_ref(root,manifest['release_artifact'],True)
    degraded=bool(degraded_checks or degraded_ac)
    status=profile['release_status_on_degraded_pass' if degraded else 'release_status_on_pass']
    return {'status':'PASS','gate':'evidence-consistency','profile':profile_id,'release_status':status,'baseline_id':lock['baseline_id'],
            'candidate_id':candidate,'required_acceptance':len(expected_ac),'checks':len(checks),
            'degraded':{'checks':sorted(degraded_checks),'acceptance':sorted(degraded_ac)},'release_sha256':sha256(artifact),
            'notice':'This verifies evidence consistency, not the truthfulness of arbitrary forged records or absence of undiscovered defects.'}

def main(argv=None) -> int:
    p=argparse.ArgumentParser(description=__doc__,formatter_class=argparse.RawDescriptionHelpFormatter);p.add_argument('--manifest',type=Path,required=True)
    a=p.parse_args(argv)
    try:result=validate_manifest(ROOT,load_json(a.manifest))
    except (ValueError,KeyError,TypeError,AttributeError,OSError,subprocess.SubprocessError) as exc:
        print(json.dumps({'status':'FAIL','error':str(exc)},ensure_ascii=False),file=sys.stderr);return 1
    print(json.dumps(result,ensure_ascii=False,indent=2));return 0
if __name__=='__main__':sys.exit(main())
