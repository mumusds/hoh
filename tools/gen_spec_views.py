#!/usr/bin/env python3
"""Generate docs/02-functional-spec.md and docs/18-traceability.md from the machine contracts.

The JSON contracts are authoritative; these Markdown files are generated views.
--check exits 1 if a committed view differs from what the contracts generate.
"""
from __future__ import annotations
import argparse,sys
from pathlib import Path
from common import ROOT,load_json

HEADER='> 生成视图：由 `tools/gen_spec_views.py` 从 `contracts/requirements.json`、`contracts/acceptance-scenarios.json` 与 `contracts/development-tasks.json` 生成，请勿手工编辑。需求变更走 CR（见 [规格治理](21-spec-governance.md)）。\n'

def cell(text:str)->str:return text.replace('|','\\|').replace('\n',' ')

def functional_spec(req:list,scen:dict)->str:
    out=['# 功能规格\n',HEADER,
         '成熟度：`detailed` 已细化为可直接实施和判定的验收；`outline` 只有概要验收，所在里程碑的 spec 任务必须先把它细化为 detailed（Readiness Gate，`tools/taskboard.py` 会拒绝领取依赖 outline 需求的实现任务）。\n',
         '发布类：`required` 必须 PASS；`degradable` 可在有诊断证据和上游引用时记为 DEGRADED，发布状态随之标明 DEGRADED（CR-001）。\n']
    for r in req:
        out.append(f'## {r["id"]} · {r["title"]}\n')
        refs=('，参考：'+'、'.join(f'`{d}`' for d in r.get('doc_refs',[]))) if r.get('doc_refs') else ''
        out.append(f'{r["milestone"]} · {r["priority"]} · 发布类 {r["release_class"]} · 目标 {", ".join(r["release_targets"])} · 成熟度 {r["spec_maturity"]}{refs}\n')
        out.append(r['summary']+'\n')
        if r.get('behavior'):out.append('**必须行为**\n\n'+''.join(f'- {b}\n' for b in r['behavior']))
        if r.get('failure_modes'):out.append('**失败与边界**\n\n'+''.join(f'- {b}\n' for b in r['failure_modes']))
        out.append('**验收**\n')
        for ac in r['acceptance']:
            s=scen[ac['test_id']]
            out.append(f'- **{ac["id"]}**（{ac["test_id"]} · 检查 `{s["check_id"]}`）Given {ac["given"]}；When {ac["when"]}；Then {ac["then"]}')
            if s.get('pass_criteria'):out.append('  - 判定：'+'；'.join(s['pass_criteria']))
        out.append('')
        if r.get('decisions'):out.append('**已确认的设计选择**（项目所有者 2026-11-03 确认，CR-002；“初值”指可经 CR 调整的基线值）\n\n'+''.join(f'- {q}\n' for q in r['decisions']))
    return '\n'.join(out).rstrip()+'\n'

def traceability(req:list,scen:dict,tasks:list)->str:
    by_req={};owner={}
    for t in tasks:
        for r in t['requirements']:by_req.setdefault(r,[]).append(t['id'])
    for t in tasks:
        if t['kind']!='implement':continue
        for r in req:
            if r['id'] in t['requirements']:
                for ac in r['acceptance']:
                    if 'acceptance' not in t or ac['id'] in t['acceptance']:owner.setdefault(ac['id'],[]).append(t['id'])
    out=['# 需求—验收—任务追踪\n',HEADER,'|需求|功能|阶段|发布类|目标|成熟度|验收（测试·检查·实现任务）|任务|','|---|---|---|---|---|---|---|---|']
    for r in req:
        acs='<br>'.join(f'{ac["id"]}（{ac["test_id"]}·{scen[ac["test_id"]]["check_id"]}·{"/".join(owner.get(ac["id"],["?"]))}）' for ac in r['acceptance'])
        out.append(f'|{r["id"]}|{cell(r["title"])}|{r["milestone"]}|{r["release_class"]}|{", ".join(r["release_targets"])}|{r["spec_maturity"]}|{acs}|{", ".join(by_req.get(r["id"],[]))}|')
    n_ac=sum(len(r['acceptance']) for r in req);detailed=sum(r['spec_maturity']=='detailed' for r in req)
    out.append(f'\n合计：{len(req)} 项需求（{detailed} 项 detailed），{n_ac} 条验收。v0.1 范围：{sum("v0.1" in r["release_targets"] for r in req)} 项需求。\n')
    return '\n'.join(out)

def render(root:Path)->dict[str,str]:
    req=load_json(root/'contracts/requirements.json')['requirements']
    scen={s['id']:s for s in load_json(root/'contracts/acceptance-scenarios.json')['scenarios']}
    tasks=load_json(root/'contracts/development-tasks.json')['tasks']
    return {'docs/02-functional-spec.md':functional_spec(req,scen),'docs/18-traceability.md':traceability(req,scen,tasks)}

def stale(root:Path)->list[str]:
    return [p for p,text in render(root).items() if not (root/p).is_file() or (root/p).read_text(encoding='utf-8')!=text]

def main(argv=None)->int:
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--check',action='store_true');a=p.parse_args(argv)
    if a.check:
        bad=stale(ROOT)
        if bad:print('Generated views are stale; run python3 tools/gen_spec_views.py: '+', '.join(bad),file=sys.stderr);return 1
        print('Generated spec views are current.');return 0
    for path,text in render(ROOT).items():(ROOT/path).write_text(text,encoding='utf-8');print('wrote '+path)
    return 0
if __name__=='__main__':sys.exit(main())
