#!/usr/bin/env python3
"""Validate this development kit without running or accepting the future product."""
from __future__ import annotations
import argparse,ast,json,re,sqlite3,subprocess,sys,tomllib
from pathlib import Path
from jsonschema import Draft202012Validator
from common import ROOT,atomic_json,is_excluded,load_json,utcnow
from reference_model import Task,topo
from release_gate import verify_baseline
from baseline import check_records
from gen_spec_views import stale as stale_views

# CR-001: only these requirements may be released as DEGRADED (third-party Pi plugins).
DEGRADABLE={'REQ-048','REQ-049','REQ-050'}
TASK_KINDS={'setup','spec','implement','verify','release'}
TRIVIAL_COMMANDS={'true',':','echo','printf','exit','yes'}
SHELLS={'bash','sh','dash','zsh','ksh'}

ENV_OPTIONS_WITH_ARG={'-u','--unset','-C','--chdir'}
INLINE_LETTERS={'shell':'c','python':'c','node':'ep','perl':'eE','ruby':'e'}

def unwrap_env(args:list[str])->list[str]|None:
    """Return the command wrapped by `env [options] [NAME=value ...]`; None for env -S/--split-string."""
    while args and Path(args[0]).name=='env':
        i=1
        while i<len(args) and args[i].startswith('-') and args[i]!='-':
            if args[i]=='--':i+=1;break
            if args[i] in ('-S','--split-string') or args[i].startswith('--split-string='):return None
            i+=2 if args[i] in ENV_OPTIONS_WITH_ARG else 1
        while i<len(args) and '=' in args[i] and not args[i].startswith('-'):i+=1
        args=args[i:]
    return args

def interpreter_kind(name:str)->str|None:
    if name in SHELLS:return 'shell'
    base=name.rstrip('0123456789.')
    return 'python' if base.startswith('python') else base if base in INLINE_LETTERS else None

def plan_command_problem(argv:list[str])->str|None:
    """Checks must run committed, reviewable commands: no always-succeeding builtins, no inline code."""
    args=unwrap_env(list(argv))
    if args is None:return 'env --split-string; commit a script instead'
    if not args:return 'empty command'
    name=Path(args[0]).name
    if name in TRIVIAL_COMMANDS:return f'always-succeeding command {args[0]!r}'
    kind=interpreter_kind(name)
    if kind:
        for a in args[1:]:
            if a in ('--eval','--print','--command') or (a.startswith('-') and not a.startswith('--') and any(ch in INLINE_LETTERS[kind] for ch in a[1:])):
                return f'inline code via {args[0]} {a}; commit a script instead'
            if not a.startswith('-'):break  # first operand is the script path; later args belong to the script
    return None

# The kit owns these paths. Product sources (crates/, web/, tests/ fixtures …) have their own linters,
# so their JSON/TOML/Markdown is never parsed or link-checked here.
KIT_DIRS=('contracts','config','examples','resources','research','reports','docs','.claude','.agents','.pi','.github')
FENCE=re.compile(r'^(```|~~~).*?^\1',re.M|re.S)

def files(root:Path,pattern:str)->list[Path]:
    found=[p for p in root.glob(pattern) if p.is_file()]
    for d in KIT_DIRS:
        if (root/d).is_dir():found+=[p for p in (root/d).rglob(pattern) if p.is_file()]
    return sorted(p for p in set(found) if not is_excluded(p,root))

def ancestors(tasks:dict,task_id:str)->set[str]:
    seen=set();stack=list(tasks[task_id]['depends_on'])
    while stack:
        t=stack.pop()
        if t not in seen and t in tasks:seen.add(t);stack.extend(tasks[t]['depends_on'])
    return seen

def validate(root:Path)->dict:
    errors=[];counts={}
    def check(ok,msg):
        if not ok:errors.append(msg)
    for name in ['README.md','START_HERE.md','CONTRACT.md','AGENTS.md','CLAUDE.md','LAUNCH_PROMPT.md','RESUME_PROMPT.md','goal.md','LICENSE','THIRD_PARTY_NOTICES.md']:
        check((root/name).is_file(),'missing '+name)
    json_files=files(root,'*.json')
    for path in json_files:
        try:load_json(path)
        except (ValueError,OSError) as e:errors.append(f'{path.relative_to(root)}: {e}')
    schemas=sorted((root/'contracts').glob('*.schema.json'))
    for path in schemas:
        try:Draft202012Validator.check_schema(load_json(path))
        except Exception as e:errors.append(f'{path.name}: invalid schema: {e}')
    policy=load_json(root/'contracts/release-policy.json')
    req=load_json(root/'contracts/requirements.json')['requirements'];req_ids={r['id'] for r in req}
    check(len(req)==70,'baseline scope must contain 70 requirements')
    check([x['id'] for x in req]==[f'REQ-{i:03d}' for i in range(1,71)],'requirement identities missing/duplicated')
    check(all(r['required_release'] is True and 'v1' in r['release_targets'] for r in req),'a requirement was removed from v1')
    check({r['id'] for r in req if r['release_class']=='degradable'}==DEGRADABLE,'release_class degradable must be exactly '+', '.join(sorted(DEGRADABLE))+' (CR-001)')
    check(all(r['release_class'] in ('required','degradable') for r in req),'unknown release_class')
    check(all(set(r['release_targets'])<=set(policy['profiles']) for r in req),'release target not in release-policy profiles')
    ac=[]
    for r in req:
        n=r['id'][4:]
        for i,a in enumerate(r['acceptance'],1):
            check(a['id']==f'AC-{n}-{i:02d}' and a['test_id']==f'TEST-{n}-{i:02d}',f'{r["id"]}: acceptance ids must be AC-{n}-NN/TEST-{n}-NN in order')
            check(all(isinstance(a.get(k),str) and a[k].strip() for k in ['given','when','then']),f'{a.get("id")}: given/when/then required')
            ac.append(a)
        check(isinstance(r.get('summary'),str) and r['summary'].strip(),f'{r["id"]}: summary required')
        if r['spec_maturity']=='detailed':
            check(len(r['acceptance'])>=2 and r.get('behavior') and r.get('failure_modes'),f'{r["id"]}: detailed requirement needs behavior, failure_modes and >=2 ACs')
        else:check(r['spec_maturity']=='outline',f'{r["id"]}: spec_maturity must be detailed or outline')
    whens=[a['when'] for a in ac];repeated={w for w in whens if whens.count(w)>3}
    check(not repeated,'boilerplate "when" repeated in more than 3 ACs: '+'; '.join(sorted(repeated))[:200])
    check(len({a['id'] for a in ac})==len(ac),'duplicate AC')
    scenarios=load_json(root/'contracts/acceptance-scenarios.json')['scenarios'];scen={s['id']:s for s in scenarios}
    check(len(scen)==len(scenarios),'duplicate scenario id')
    check(set(scen)=={a['test_id'] for a in ac},'scenario/test mapping mismatch')
    owner={a['id']:r['id'] for r in req for a in r['acceptance']}
    for a in ac:
        s=scen.get(a['test_id'])
        if not s:continue
        check(s['acceptance_id']==a['id'] and s['requirement_id']==owner[a['id']],f'{s["id"]}: wrong AC/requirement binding')
        check(s['check_id'] in policy['checks'],f'{s["id"]}: unknown check {s["check_id"]}')
        check(bool(s.get('steps')) and bool(s.get('pass_criteria')),f'{s["id"]}: steps and pass_criteria required')
    for name,profile in policy['profiles'].items():
        check(set(profile['required_checks'])<=set(policy['checks']),f'profile {name}: unknown checks')
        for r in req:
            if name in r['release_targets']:
                for a in r['acceptance']:
                    s=scen.get(a['test_id'])
                    if s:check(s['check_id'] in profile['required_checks'],f'profile {name}: {a["id"]} needs check {s["check_id"]} which the profile does not run')
    check(all('v1' in r['release_targets'] for r in req if 'v0.1' in r['release_targets']),'v0.1 must be a subset of v1')
    check(all(r['spec_maturity']=='detailed' for r in req if 'v0.1' in r['release_targets']),'v0.1 requirements must be detailed')
    tasks=load_json(root/'contracts/development-tasks.json')['tasks'];tindex={t['id']:t for t in tasks}
    check(len(tindex)==len(tasks),'duplicate task id')
    try:topo([Task(t['id'],tuple(t['depends_on'])) for t in tasks])
    except ValueError as e:errors.append('task DAG: '+str(e))
    for t in tasks:
        check(t.get('kind') in TASK_KINDS and isinstance(t.get('writer'),bool),f'{t["id"]}: kind/writer required')
        check(set(t['requirements'])<=req_ids,f'{t["id"]}: unknown requirement')
        check(all(owner.get(x) in t['requirements'] for x in t.get('acceptance',[])),f'{t["id"]}: acceptance refs must exist and belong to its requirements')
        check(t.get('owner_role')!='qa' or t['kind'] in ('verify','spec'),f'{t["id"]}: QA may own verify/spec tasks only, never implementation')
        check(not t.get('writer') or t['kind'] in ('setup','spec','implement','release'),f'{t["id"]}: verify tasks must not be writers')
    implemented={r for t in tasks if t['kind']=='implement' for r in t['requirements']}
    check(implemented==req_ids,'every requirement needs an implement task: missing '+', '.join(sorted(req_ids-implemented)))
    for r in req:
        if r['spec_maturity']=='outline':
            spec_tasks={t['id'] for t in tasks if t['kind']=='spec' and r['id'] in t['requirements']}
            for t in tasks:
                if t['kind']=='implement' and r['id'] in t['requirements']:
                    check(bool(spec_tasks & ancestors(tindex,t['id'])),f'{t["id"]}: implements outline {r["id"]} without depending on its spec task')
    proto=load_json(root/'contracts/protocol.schema.json')
    for path in ['context-handoff.json','hcp-message.json','command.json']:
        try:Draft202012Validator(proto).validate(load_json(root/'examples'/path))
        except Exception as e:errors.append(f'example {path}: {e}')
    check((root/'tests/check-plan.json').is_file() or not (root/'Cargo.toml').is_file(),'tests/check-plan.json is required once the Cargo workspace exists')
    for rel in ['examples/check-plan.json','tests/check-plan.json']:
        if not (root/rel).is_file():continue
        try:
            plan=load_json(root/rel);Draft202012Validator(load_json(root/'contracts/check-plan.schema.json')).validate(plan)
            check(set(plan['checks'])==set(policy['checks']),f'{rel}: must declare exactly the release-policy checks')
            for cid,entry in plan['checks'].items():
                rule=policy['checks'].get(cid,{})
                problem=plan_command_problem(entry['argv'])
                check(problem is None,f'{rel}: {cid} uses {problem}')
                check(entry['mode']==rule.get('mode'),f'{rel}: {cid} mode {entry["mode"]} but policy needs {rule.get("mode")}')
                if entry['mode']=='live':check(policy['artifacts']['live_sessions'] in entry.get('artifacts',[]),f'{rel}: live check {cid} must declare {policy["artifacts"]["live_sessions"]}')
            check(policy['artifacts']['soak_timeline'] in plan['checks'].get('soak',{}).get('artifacts',[]),f'{rel}: soak must declare {policy["artifacts"]["soak_timeline"]}')
        except Exception as e:errors.append(f'{rel}: {e}')
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
    for p in files(root,'*.toml'):
        try:tomllib.loads(p.read_text())
        except ValueError as e:errors.append(f'{p.relative_to(root)}: {e}')
    for p in sorted((root/'tools').glob('*.py'))+sorted((root/'tests/bundle').glob('*.py')):
        try:ast.parse(p.read_text(),filename=str(p))
        except SyntaxError as e:errors.append(str(e))
    for p in sorted((root/'scripts').rglob('*.sh')):
        r=subprocess.run(['bash','-n',str(p)],capture_output=True,text=True)
        check(r.returncode==0,p.name+': '+r.stderr)
    try:
        c=sqlite3.connect(':memory:');c.executescript((root/'contracts/sql/001_initial.sql').read_text())
        check(c.execute('PRAGMA integrity_check').fetchone()[0]=='ok','SQLite integrity failed')
        check(c.execute('PRAGMA foreign_key_check').fetchall()==[],'SQLite foreign key check failed');c.close()
    except sqlite3.Error as e:errors.append('SQL schema invalid: '+str(e))
    try:
        lock=verify_baseline(root);errors.extend('baseline: '+e for e in check_records(root,lock))
        check(policy['baseline_id']==lock['baseline_id'],'release-policy baseline_id differs from baseline lock')
    except Exception as e:errors.append('baseline: '+str(e))
    try:errors.extend('generated view stale (run tools/gen_spec_views.py): '+p for p in stale_views(root))
    except Exception as e:errors.append('generated views: '+str(e))
    for p in files(root,'*.md'):
        for target in re.findall(r'(?<!!)\[[^\]]+\]\(([^\s)]+)\)',FENCE.sub('',p.read_text())):
            if target.startswith(('https:','http:','mailto:','#')) or '{{' in target:continue
            target=target.split('#',1)[0]
            if target:check((p.parent/target).exists(),f'broken local link {p.relative_to(root)} -> {target}')
    counts.update(requirements=len(req),detailed_requirements=sum(r['spec_maturity']=='detailed' for r in req),acceptance=len(ac),
                  interim_v0_1_requirements=sum('v0.1' in r['release_targets'] for r in req),development_tasks=len(tasks),
                  skills=len(resources),templates=len(profiles['full']),schemas=len(schemas),json_files=len(json_files))
    return {'schema_version':'1.1','at':utcnow(),'scope':'BUNDLE_ONLY','status':'PASS' if not errors else 'FAIL','counts':counts,'errors':errors,'product_accepted':False}

def main(argv=None):
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output',type=Path);a=p.parse_args(argv)
    try:r=validate(ROOT)
    except Exception as e:r={'status':'FAIL','scope':'BUNDLE_ONLY','errors':[f'{type(e).__name__}: {e}'],'product_accepted':False}
    if a.output:atomic_json(a.output,r)
    print(json.dumps(r,ensure_ascii=False,indent=2));return 0 if r['status']=='PASS' else 1
if __name__=='__main__':sys.exit(main())
