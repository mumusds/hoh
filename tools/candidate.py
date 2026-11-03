#!/usr/bin/env python3
"""Freeze/verify a committed bootstrap candidate without modifying source.

Use inside the candidate checkout. Evidence belongs in ignored .hoh-dev/.
The final Rust runtime owns its own candidates; this helps its development.
"""
from __future__ import annotations
import argparse,hashlib,json,subprocess,sys
from pathlib import Path
from common import ROOT,atomic_json,load_json,ref_for,safe_path,sha256

def git(root:Path,*args:str)->str:
    return subprocess.run(['git',*args],cwd=root,stdout=subprocess.PIPE,stderr=subprocess.PIPE,check=True).stdout.decode('utf-8','strict').rstrip('\n')

def identity(data:dict)->str:
    payload={k:v for k,v in data.items() if k!='candidate_id'}
    return 'CAND-'+hashlib.sha256(json.dumps(payload,sort_keys=True,separators=(',',':'),ensure_ascii=False).encode()).hexdigest()

def verify(root:Path,data:dict)->None:
    if data['candidate_id']!=identity(data):raise ValueError('candidate identity mismatch')
    if git(root,'rev-parse','HEAD')!=data['git_commit'] or git(root,'rev-parse','HEAD^{tree}')!=data['git_tree']:raise ValueError('candidate checkout changed')
    if git(root,'status','--porcelain','--untracked-files=all'):raise ValueError('candidate checkout is dirty (put evidence in ignored .hoh-dev)')
    names=git(root,'ls-files','-z').split('\0')
    names=[n for n in names if n]
    refs=data['source_files'];mapped={r['path']:r for r in refs}
    if len(mapped)!=len(refs) or set(names)!=set(mapped):raise ValueError('candidate source inventory mismatch')
    for r in refs+[data[k] for k in ['spec','environment','test_plan']]:
        p=safe_path(root,r['path'])
        if not p.is_file() or p.is_symlink() or sha256(p)!=r['sha256']:raise ValueError('candidate artifact changed: '+r['path'])
    for key in ['spec','environment','test_plan']:
        if data[key]['sha256']!=data[key+'_sha256']:raise ValueError('candidate '+key+' hash mismatch')

def freeze(root:Path,spec:Path,environment:Path,test_plan:Path)->dict:
    if git(root,'status','--porcelain','--untracked-files=all'):raise ValueError('Commit intended source changes first; ignore development evidence.')
    names=[n for n in git(root,'ls-files','-z').split('\0') if n]
    files=[]
    for name in names:
        p=root/name
        if not p.is_file() or p.is_symlink():raise ValueError('Bootstrap candidate supports regular tracked files only: '+name)
        files.append(ref_for(p,root))
    if not files:raise ValueError('empty candidate source inventory')
    data={'schema_version':'1.0','git_commit':git(root,'rev-parse','HEAD'),'git_tree':git(root,'rev-parse','HEAD^{tree}'),'source_files':sorted(files,key=lambda x:x['path'])}
    for k,p in [('spec',spec),('environment',environment),('test_plan',test_plan)]:
        r=ref_for(p,root);data[k]=r;data[k+'_sha256']=r['sha256']
    data['candidate_id']=identity(data);verify(root,data);return data

def main(argv=None):
    p=argparse.ArgumentParser(description=__doc__);sub=p.add_subparsers(dest='action',required=True)
    f=sub.add_parser('freeze');f.add_argument('--spec',type=Path,required=True);f.add_argument('--environment',type=Path,required=True);f.add_argument('--test-plan',type=Path,required=True);f.add_argument('--out',type=Path,required=True)
    v=sub.add_parser('verify');v.add_argument('manifest',type=Path)
    a=p.parse_args(argv)
    try:
        if a.action=='verify':verify(ROOT,load_json(a.manifest));print('Candidate source, environment and test plan verified.')
        else:
            out=a.out.resolve()
            if not out.is_relative_to(ROOT/'.hoh-dev'):raise ValueError('candidate metadata must be under ignored .hoh-dev/')
            if out.exists():raise ValueError('candidate manifest is immutable; choose a new output')
            data=freeze(ROOT,a.spec.resolve(),a.environment.resolve(),a.test_plan.resolve());atomic_json(out,data);print(json.dumps(ref_for(out),ensure_ascii=False))
    except (ValueError,OSError,KeyError,subprocess.SubprocessError) as exc:print(str(exc),file=sys.stderr);return 1
    return 0
if __name__=='__main__':sys.exit(main())
