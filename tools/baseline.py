#!/usr/bin/env python3
"""Verify or re-lock the contract baseline. Re-locking requires an APPROVED change request.

This makes baseline changes explicit and reviewable in Git history; it is not
tamper-proof against someone who can rewrite every file in the VM.
"""
from __future__ import annotations
import argparse,json,subprocess,sys
from pathlib import Path
from common import ROOT,atomic_json,load_json,safe_path,sha256
from release_gate import LOCK_REQUIRED,GateError,check_records,parse_cr,verify_baseline

def committed_crs(root:Path)->set[str]:
    """CR ids recorded in the lock at HEAD; an amend may only touch a CR that is not yet committed."""
    r=subprocess.run(['git','show','HEAD:contracts/baseline-lock.json'],cwd=root,capture_output=True)
    if r.returncode!=0:return set()
    try:return {c['id'] for c in json.loads(r.stdout).get('change_requests',[])}
    except ValueError:return set()

def relock(root:Path,cr_path:Path,baseline_id:str|None,amend:bool=False,drop:list[str]|None=None,add:list[str]|None=None)->dict:
    """New baseline revision for an APPROVED CR. amend=True re-hashes for the latest, still uncommitted CR."""
    old_path=root/'contracts/baseline-lock.json';old=load_json(old_path)
    rel=str(cr_path.resolve().relative_to(root.resolve()));info=parse_cr(root/rel)
    applied=old.get('change_requests',[])
    if amend:
        if not applied or applied[-1]['id']!=info['id']:raise ValueError('--amend only re-locks the most recent CR')
        if info['id'] in committed_crs(root):raise ValueError(info['id']+' is already committed; record further changes in a new CR')
        applied=applied[:-1];revision=old['revision'];previous=old['previous']
    else:
        if any(c['id']==info['id'] for c in applied):raise ValueError(info['id']+' already applied; use --amend while it is uncommitted')
        revision=old.get('revision',1)+1;previous={'baseline_id':old['baseline_id'],'lock_sha256':sha256(old_path)}
    new_id=baseline_id or old['baseline_id']
    policy=load_json(root/'contracts/release-policy.json')
    if policy['baseline_id']!=new_id:raise ValueError(f'release-policy.json baseline_id is {policy["baseline_id"]}; update it to {new_id} first')
    for extra in add or []:
        if not safe_path(root,extra).is_file():raise ValueError(f'--add {extra}: not a file in the project')
    files=sorted((set(old['files'])|set(LOCK_REQUIRED)|set(add or []))-set(drop or []))
    if set(LOCK_REQUIRED)-set(files):raise ValueError('cannot drop gate-defining contracts from the lock')
    lock={'schema_version':'1.1','baseline_id':new_id,'revision':revision,
          'change_policy':old.get('change_policy','Formal CR + new baseline; never lower required gates to pass'),
          'previous':previous,'change_requests':applied+[{'id':info['id'],'path':rel,'sha256':sha256(root/rel)}],
          'files':{f:sha256(safe_path(root,f)) for f in files}}
    atomic_json(old_path,lock);return lock

def main(argv=None)->int:
    p=argparse.ArgumentParser(description=__doc__);sub=p.add_subparsers(dest='action',required=True)
    sub.add_parser('verify')
    r=sub.add_parser('relock');r.add_argument('--cr',type=Path,required=True,help='APPROVED change request record');r.add_argument('--baseline-id')
    r.add_argument('--amend',action='store_true',help='Re-hash for the most recent CR (before it is committed).')
    r.add_argument('--drop',action='append',default=[],help='Remove a non-gate file from the lock (record why in the CR).')
    r.add_argument('--add',action='append',default=[],help='Add a contract file to the lock (record why in the CR).')
    a=p.parse_args(argv)
    try:
        if a.action=='relock':
            lock=relock(ROOT,a.cr,a.baseline_id,a.amend,a.drop,a.add);print(json.dumps({'baseline_id':lock['baseline_id'],'revision':lock['revision'],'files':len(lock['files'])},ensure_ascii=False))
        lock=verify_baseline(ROOT);errors=check_records(ROOT,lock)
        if errors:raise ValueError('; '.join(errors))
        print(f'Baseline {lock["baseline_id"]} revision {lock.get("revision",1)} verified ({len(lock["files"])} files).')
    except (ValueError,KeyError,OSError,GateError) as exc:print(str(exc),file=sys.stderr);return 1
    return 0
if __name__=='__main__':sys.exit(main())
