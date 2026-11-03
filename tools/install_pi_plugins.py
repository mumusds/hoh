#!/usr/bin/env python3
"""Install locked project-local Pi packages, not a second workflow engine.

No settings keys are invented. After install the AI must read the pinned package
schema, configure compaction/memory ownership, and run the required live matrix.
"""
from __future__ import annotations
import argparse, shutil, subprocess, sys
from pathlib import Path
from common import ROOT,atomic_json,load_json,utcnow

def main(argv=None):
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--dry-run',action='store_true');a=p.parse_args(argv)
    conf=load_json(ROOT/'config/plugins.json');lock_path=ROOT/'.hoh-dev/plugin-lock.json'
    lock=load_json(lock_path) if lock_path.exists() else {'schema_version':'1.0','plugins':{}}
    if a.dry_run:
        for item in conf['plugins']:
            print(f'pi install --local npm:{item["package"]}@'+lock['plugins'].get(item['id'],{}).get('version','<resolve-and-lock>'))
        return 0
    if not shutil.which('pi') or not shutil.which('npm'):raise RuntimeError('Pi and npm must be available')
    help_text=subprocess.run(['pi','install','--help'],capture_output=True,text=True,timeout=20,check=True).stdout
    if '--local' not in help_text:raise RuntimeError('Installed Pi does not advertise local package installation')
    pi_version=subprocess.run(['pi','--version'],capture_output=True,text=True,timeout=20,check=True).stdout.strip()
    lock['pi_version']=pi_version
    for item in conf['plugins']:
        old=lock['plugins'].get(item['id'],{})
        version=old.get('version') or subprocess.run(['npm','view',item['package'],'version'],capture_output=True,text=True,timeout=60,check=True).stdout.strip()
        if not version or '\n' in version:raise RuntimeError('Invalid package version result')
        entry={'package':item['package'],'version':version,'source':item['source'],'state':'RESOLVED','compatibility':'NOT_TESTED'}
        lock['plugins'][item['id']]=entry;atomic_json(lock_path,lock)
        subprocess.run(['pi','install','--local',f'npm:{item["package"]}@{version}'],cwd=ROOT,timeout=600,check=True)
        entry.update(state='INSTALLED',installed_at=utcnow());atomic_json(lock_path,lock)
    print('Pi packages installed. Compatibility, headless UI hooks and memory ownership still require live tests.')
    return 0
if __name__=='__main__':
    try:sys.exit(main())
    except (RuntimeError,OSError,subprocess.SubprocessError) as exc:print(str(exc),file=sys.stderr);sys.exit(3)
