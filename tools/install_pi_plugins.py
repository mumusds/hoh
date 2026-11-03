#!/usr/bin/env python3
"""Install locked project-local Pi packages, not a second workflow engine.

No settings keys are invented. After install the AI must read the pinned package
schema, configure compaction/memory ownership, and run the live matrix.
Plugins are release_class=degradable (CR-001): a plugin that cannot be installed
is recorded as DEGRADED with its error; other plugins and bootstrap continue.
Exit code 3 means at least one plugin is DEGRADED.
"""
from __future__ import annotations
import argparse, json, shutil, subprocess, sys
from pathlib import Path
from common import ROOT,atomic_json,flag_supported,load_json,utcnow

def installed_integrity(package:str)->str|None:
    """Integrity npm recorded for the tarball it actually installed (Pi installs project packages under .pi/npm)."""
    lock=ROOT/'.pi/npm/package-lock.json'
    if not lock.is_file():return None
    entry=load_json(lock).get('packages',{}).get(f'node_modules/{package}',{})
    return entry.get('integrity')

def npm_spec(source:str)->tuple[str,str|None]|None:
    """('name', 'version' or None) for an `npm:name[@version]` package source, else None."""
    if not source.startswith('npm:'):return None
    spec=source[4:];at=spec.rfind('@')
    return (spec[:at],spec[at+1:]) if at>0 else (spec,None)

def verify_installed(project:Path)->dict:
    """Check the project packages Pi would load from <project>/.pi/npm (M0R3-05). Each one that is installed must
    match the version in .pi/settings.json and the integrity tools/install_pi_plugins.py pinned in the plugin lock.
    A configured package that is not installed is only `absent`: started with PI_OFFLINE=1, Pi skips it instead of
    installing it outside this guard (plugins are degradable). Non-npm sources cannot be verified."""
    result={'verified':[],'absent':[],'problems':[]};settings=project/'.pi/settings.json'
    if not settings.is_file():return result
    lock_path=project/'.hoh-dev/plugin-lock.json';npm_lock=project/'.pi/npm/package-lock.json'
    pinned={e.get('package'):e for e in (load_json(lock_path).get('plugins',{}).values() if lock_path.is_file() else [])}
    installed=load_json(npm_lock).get('packages',{}) if npm_lock.is_file() else {}
    for item in load_json(settings).get('packages',[]):
        source=item if isinstance(item,str) else item.get('source','') if isinstance(item,dict) else ''
        spec=npm_spec(source)
        if spec is None:result['problems'].append(f'{source or item!r}: not an npm source; its integrity cannot be verified');continue
        name,version=spec
        if not (project/'.pi/npm/node_modules'/name).exists():result['absent'].append(name);continue
        entry=installed.get(f'node_modules/{name}',{});lock=pinned.get(name)
        if lock is None or lock.get('state')!='INSTALLED':result['problems'].append(f'{name}: installed but the plugin lock has no INSTALLED entry');continue
        if entry.get('version')!=(version or lock.get('version')) or lock.get('version')!=entry.get('version'):
            result['problems'].append(f'{name}: installed version {entry.get("version")} differs from settings {version} or lock {lock.get("version")}');continue
        if not entry.get('integrity') or entry.get('integrity')!=lock.get('integrity'):
            result['problems'].append(f'{name}: installed integrity {entry.get("integrity")} differs from the pinned {lock.get("integrity")}');continue
        result['verified'].append(name)
    return result

def run(cmd:list[str],timeout:int)->str:
    return subprocess.run(cmd,cwd=ROOT,capture_output=True,text=True,timeout=timeout,check=True).stdout.strip()

def main(argv=None):
    p=argparse.ArgumentParser(description=__doc__,formatter_class=argparse.RawDescriptionHelpFormatter);p.add_argument('--dry-run',action='store_true')
    p.add_argument('--verify',action='store_true',help='only check that installed project packages match the lock (exit 1 if not)');a=p.parse_args(argv)
    if a.verify:
        r=verify_installed(ROOT);print(json.dumps(r,indent=2))
        for problem in r['problems']:print('unverified Pi package: '+problem,file=sys.stderr)
        return 1 if r['problems'] else 0
    conf=load_json(ROOT/'config/plugins.json');lock_path=ROOT/'.hoh-dev/plugin-lock.json'
    lock=load_json(lock_path) if lock_path.exists() else {'schema_version':'1.1','plugins':{}}
    if a.dry_run:
        for item in conf['plugins']:
            print(f'pi install --local [--approve] npm:{item["package"]}@'+(item.get('version') or lock['plugins'].get(item['id'],{}).get('version','<resolve-and-lock>')))
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
            version=item.get('version') or old.get('version') or run(['npm','view',item['package'],'version'],60)
            if not version or '\n' in version:raise RuntimeError('invalid npm version result')
            integrity=run(['npm','view',f'{item["package"]}@{version}','dist.integrity'],60)
            if not integrity.startswith('sha512-'):raise RuntimeError('npm registry returned no sha512 integrity')
            if item.get('integrity') and integrity!=item['integrity']:raise RuntimeError(f'registry integrity {integrity} differs from the pinned {item["integrity"]}')
            entry.update(version=version,integrity=integrity,state='RESOLVED')
            lock['plugins'][item['id']]=entry;atomic_json(lock_path,lock)
            run(['pi','install','--local',*approve,f'npm:{item["package"]}@{version}'],600)
            installed=installed_integrity(item['package'])
            if installed is None:raise RuntimeError('installed package not found in .pi/npm/package-lock.json')
            if installed!=integrity:raise RuntimeError(f'installed tarball integrity {installed} differs from {integrity}')
            entry.update(state='INSTALLED',installed_at=utcnow(),installed_integrity=installed)
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
