#!/usr/bin/env python3
"""Validate this development kit without running or accepting the future product."""
from __future__ import annotations
import argparse,ast,json,re,sqlite3,subprocess,sys,tomllib
from pathlib import Path
from jsonschema import Draft202012Validator
from common import ROOT,atomic_json,load_json,utcnow
from reference_model import Task,topo
from release_gate import verify_baseline

def validate(root:Path)->dict:
    errors=[];counts={}
    def check(ok,msg):
        if not ok:errors.append(msg)
    for name in ['README.md','START_HERE.md','CONTRACT.md','AGENTS.md','CLAUDE.md','LAUNCH_PROMPT.md','RESUME_PROMPT.md','goal.md','LICENSE','THIRD_PARTY_NOTICES.md']:
        check((root/name).is_file(),'missing '+name)
    json_files=list(root.rglob('*.json'))
    json_files=[p for p in json_files if not any(x in p.parts for x in ['.hoh-dev','.git','node_modules','target','.venv'])]
    for path in json_files:
        try:load_json(path)
        except (ValueError,OSError) as e:errors.append(f'{path.relative_to(root)}: {e}')
    schemas=list((root/'contracts').glob('*.schema.json'))
    for path in schemas:
        try:Draft202012Validator.check_schema(load_json(path))
        except Exception as e:errors.append(f'{path.name}: invalid schema: {e}')
    req=load_json(root/'contracts/requirements.json')['requirements']
    check(len(req)==70,'required-release scope must contain 70 baseline requirements')
    check([x['id'] for x in req]==[f'REQ-{i:03d}' for i in range(1,71)],'requirement identities missing/duplicated')
    check(all(r['required_release'] is True for r in req),'a required-release requirement was downgraded')
    ac=[a for r in req for a in r['acceptance']];check(len({a['id'] for a in ac})==len(ac),'duplicate AC')
    check(len({a['test_id'] for a in ac})==len(ac),'duplicate acceptance test mapping')
    tasks=load_json(root/'contracts/development-tasks.json')['tasks']
    try:topo([Task(t['id'],tuple(t['depends_on'])) for t in tasks])
    except ValueError as e:errors.append('task DAG: '+str(e))
    linked={i for t in tasks for i in t['requirements']}
    check(linked=={r['id'] for r in req},'task coverage missing/unknown requirements')
    scenarios=load_json(root/'contracts/acceptance-scenarios.json')['scenarios']
    check({x['id'] for x in scenarios}=={a['test_id'] for a in ac},'scenario/test mapping mismatch')
    proto=load_json(root/'contracts/protocol.schema.json')
    for path in ['context-handoff.json','hcp-message.json','command.json']:
        try:Draft202012Validator(proto).validate(load_json(root/'examples'/path))
        except Exception as e:errors.append(f'example {path}: {e}')
    resources=load_json(root/'resources/skills/registry.json')['skills']
    try:topo([Task(x['id'],tuple(x.get('dependencies',[]))) for x in resources])
    except ValueError as e:errors.append('skill DAG: '+str(e))
    for skill in resources:check((root/skill['entry']).is_file(),'missing skill entry '+skill['id'])
    profiles=load_json(root/'resources/templates/profiles.json')['profiles']
    for ids in profiles.values():
        for i in ids:check((root/f'resources/templates/{i}.md').is_file(),'missing template '+i)
    for p in (root/'resources/prompts').glob('*.md'):
        variables=set(re.findall(r'\{\{([A-Z_]+)\}\}',p.read_text()))
        check(variables=={'PROJECT_CONTEXT','TASK_CONTEXT','EVIDENCE_CONTEXT','GOAL'},f'{p.name}: incorrect four-layer placeholder contract')
    for p in root.rglob('*.toml'):
        if '.hoh-dev' in p.parts:continue
        try:tomllib.loads(p.read_text())
        except ValueError as e:errors.append(str(p)+': '+str(e))
    for p in (root/'tools').glob('*.py'):
        try:ast.parse(p.read_text(),filename=str(p))
        except SyntaxError as e:errors.append(str(e))
    for p in (root/'scripts').glob('*.sh'):
        r=subprocess.run(['bash','-n',str(p)],capture_output=True,text=True)
        check(r.returncode==0,p.name+': '+r.stderr)
    try:
        c=sqlite3.connect(':memory:');c.executescript((root/'contracts/sql/001_initial.sql').read_text())
        check(c.execute('PRAGMA integrity_check').fetchone()[0]=='ok','SQLite integrity failed');c.close()
    except sqlite3.Error as e:errors.append('SQL schema invalid: '+str(e))
    try:verify_baseline(root)
    except Exception as e:errors.append('baseline: '+str(e))
    for p in root.rglob('*.md'):
        if '.hoh-dev' in p.parts:continue
        for target in re.findall(r'(?<!!)\[[^\]]+\]\(([^\s)]+)\)',p.read_text()):
            if target.startswith(('https:','http:','mailto:','#')) or '{{' in target:continue
            target=target.split('#',1)[0]
            if target:check((p.parent/target).exists(),f'broken local link {p.relative_to(root)} -> {target}')
    counts.update(requirements=len(req),acceptance=len(ac),development_tasks=len(tasks),skills=len(resources),templates=len(profiles['full']),schemas=len(schemas),json_files=len(json_files))
    return {'schema_version':'1.0','at':utcnow(),'scope':'BUNDLE_ONLY','status':'PASS' if not errors else 'FAIL','counts':counts,'errors':errors,'product_accepted':False}

def main(argv=None):
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path);a=p.parse_args(argv)
    try:r=validate(ROOT)
    except Exception as e:r={'status':'FAIL','scope':'BUNDLE_ONLY','errors':[str(e)],'product_accepted':False}
    if a.output:atomic_json(a.output,r)
    print(json.dumps(r,ensure_ascii=False,indent=2));return 0 if r['status']=='PASS' else 1
if __name__=='__main__':sys.exit(main())
