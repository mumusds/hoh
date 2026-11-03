#!/usr/bin/env python3
"""Install locked project-local Pi packages, not a second workflow engine.

No settings keys are invented. After install the AI must read the pinned package
schema, configure compaction/memory ownership, and run the live matrix.
Plugins are release_class=degradable (CR-001): a plugin that cannot be installed
is recorded as DEGRADED with its error; other plugins and bootstrap continue.
Exit code 3 means at least one plugin is DEGRADED.
"""
from __future__ import annotations
import argparse, shutil, subprocess, sys
from common import ROOT,atomic_json,flag_supported,load_json,utcnow

def run(cmd:list[str],timeout:int)->str:
    return subprocess.run(cmd,cwd=ROOT,capture_output=True,text=True,timeout=timeout,check=True).stdout.strip()

def main(argv=None):
    p=argparse.ArgumentParser(description=__doc__,formatter_class=argparse.RawDescriptionHelpFormatter);p.add_argument('--dry-run',action='store_true');a=p.parse_args(argv)
    conf=load_json(ROOT/'config/plugins.json');lock_path=ROOT/'.hoh-dev/plugin-lock.json'
    lock=load_json(lock_path) if lock_path.exists() else {'schema_version':'1.1','plugins':{}}
    if a.dry_run:
        for item in conf['plugins']:
            print(f'pi install --local [--approve] npm:{item["package"]}@'+lock['plugins'].get(item['id'],{}).get('version','<resolve-and-lock>'))
        return 0
    try:
        if not shutil.which('pi') or not shutil.which('npm'):raise RuntimeError('Pi and npm must be available')
        help_text=run(['pi','install','--help'],20)
        if not flag_supported(help_text,'--local'):raise RuntimeError('Installed Pi does not advertise local package installation')
    except (RuntimeError,OSError,subprocess.SubprocessError) as exc:
        # Prerequisite failure degrades every plugin explicitly instead of leaving no record.
        for item in conf['plugins']:
            lock['plugins'][item['id']]={'package':item['package'],'source':item['source'],'state':'DEGRADED','compatibility':'NOT_TESTED','error':'prerequisite: '+str(exc),'failed_at':utcnow()}
        atomic_json(lock_path,lock);print('Pi plugins DEGRADED (prerequisite missing): '+str(exc),file=sys.stderr);return 3
    # Pi >=1.x refuses to modify an untrusted project's local package config without --approve.
    approve=['--approve'] if flag_supported(help_text,'--approve') else []
    lock['pi_version']=run(['pi','--version'],20);lock['schema_version']='1.1'
    degraded=[]
    for item in conf['plugins']:
        old=lock['plugins'].get(item['id'],{})
        entry={'package':item['package'],'source':item['source'],'compatibility':'NOT_TESTED'}
        try:
            version=old.get('version') or run(['npm','view',item['package'],'version'],60)
            if not version or '\n' in version:raise RuntimeError('invalid npm version result')
            integrity=old.get('integrity') or run(['npm','view',f'{item["package"]}@{version}','dist.integrity'],60)
            if not integrity.startswith('sha512-'):raise RuntimeError('npm registry returned no sha512 integrity')
            entry.update(version=version,integrity=integrity,state='RESOLVED')
            lock['plugins'][item['id']]=entry;atomic_json(lock_path,lock)
            run(['pi','install','--local',*approve,f'npm:{item["package"]}@{version}'],600)
            entry.update(state='INSTALLED',installed_at=utcnow())
        except (RuntimeError,OSError,subprocess.SubprocessError) as exc:
            detail=(getattr(exc,'stderr',None) or str(exc)).strip()[-2000:]
            entry.update(state='DEGRADED',error=detail,failed_at=utcnow());degraded.append(item['id'])
            print(f'DEGRADED {item["id"]}: {detail}',file=sys.stderr)
        lock['plugins'][item['id']]=entry;atomic_json(lock_path,lock)
    if degraded:
        print('Pi plugins DEGRADED: '+', '.join(degraded)+'. Development can continue; release reports must show DEGRADED (see docs/15).',file=sys.stderr);return 3
    print('Pi packages installed. Compatibility, headless UI hooks and memory ownership still require live tests.')
    return 0
if __name__=='__main__':
    try:sys.exit(main())
    except (RuntimeError,OSError,subprocess.SubprocessError) as exc:print(str(exc),file=sys.stderr);sys.exit(3)
