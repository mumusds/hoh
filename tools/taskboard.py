#!/usr/bin/env python3
"""Small locked development ledger, not the HoH product task database.

Plan: contracts/development-tasks.json. State: .hoh-dev/task-state.json.
Rules enforced here: atomic claim with owner, legal transitions only, at most
one running writer task (single writer), implementation/verification tasks
only for requirements whose spec_maturity is "detailed" (readiness), and
reopening a done task blocks every dependent that already started.
"""
from __future__ import annotations
import argparse, fcntl, json, sys
from pathlib import Path
from common import ROOT,atomic_json,load_json,utcnow

TRANSITIONS={'pending':{'blocked'},'running':{'done','blocked','pending'},'blocked':{'pending'},'done':{'pending'}}
NEEDS_DETAILED_SPEC={'implement','verify','release'}

class LedgerError(ValueError):pass

def dependents(tasks:list[dict],task_id:str)->set[str]:
    found=set();frontier=[task_id]
    while frontier:
        cur=frontier.pop()
        for t in tasks:
            if cur in t['depends_on'] and t['id'] not in found:found.add(t['id']);frontier.append(t['id'])
    return found

def readiness_problems(task:dict,maturity:dict[str,str])->list[str]:
    if task.get('kind') not in NEEDS_DETAILED_SPEC:return []
    return [r for r in task['requirements'] if maturity.get(r)!='detailed']

def main(argv=None):
    p=argparse.ArgumentParser(description=__doc__,formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument('action',choices=['list','next','claim','set'])
    p.add_argument('task_id',nargs='?');p.add_argument('status',nargs='?',choices=sorted(TRANSITIONS))
    p.add_argument('--owner',default='',help='Session/agent identity; required for claim and for leaving running.')
    p.add_argument('--note',default='');a=p.parse_args(argv)
    source=load_json(ROOT/'contracts/development-tasks.json')['tasks'];index={t['id']:t for t in source}
    maturity={r['id']:r.get('spec_maturity','outline') for r in load_json(ROOT/'contracts/requirements.json')['requirements']}
    statefile=ROOT/'.hoh-dev/task-state.json';statefile.parent.mkdir(exist_ok=True)
    with (statefile.parent/'task-state.lock').open('a+') as lock:
        fcntl.flock(lock,fcntl.LOCK_EX)
        data=load_json(statefile) if statefile.exists() else {'tasks':{},'history':[]}
        def status(i):return data['tasks'].get(i,{}).get('status','pending')
        def record(task_id,new,note,owner=''):
            entry={'task_id':task_id,'status':new,'owner':owner,'note':note,'at':utcnow()}
            data['tasks'][task_id]=entry;data['history'].append(entry)
        def ready(t):return status(t['id'])=='pending' and all(status(d)=='done' for d in t['depends_on']) and not readiness_problems(t,maturity)
        try:
            if a.action in ('claim','set') and a.task_id not in index:raise LedgerError('unknown task_id')
            if a.action=='claim':
                t=index[a.task_id]
                if not a.owner.strip():raise LedgerError('claim requires --owner')
                if status(t['id'])!='pending':raise LedgerError(f'task is {status(t["id"])}, not pending')
                if any(status(d)!='done' for d in t['depends_on']):raise LedgerError('dependencies not done')
                missing=readiness_problems(t,maturity)
                if missing:raise LedgerError('requirements still outline (run the milestone spec task first): '+', '.join(missing))
                if t.get('writer'):
                    busy=[i for i in index if i!=t['id'] and index[i].get('writer') and status(i)=='running']
                    if busy:raise LedgerError('single writer: writer task already running: '+', '.join(busy))
                record(t['id'],'running',a.note,a.owner);atomic_json(statefile,data)
            elif a.action=='set':
                if not a.status:raise LedgerError('status required')
                cur=status(a.task_id);new=a.status
                if new=='running':raise LedgerError('use "claim" to start a task')
                if new not in TRANSITIONS[cur]:raise LedgerError(f'illegal transition {cur} -> {new}')
                if cur=='running' and a.owner!=data['tasks'][a.task_id].get('owner'):raise LedgerError('only the claiming --owner may leave running')
                if new in ('done','blocked') or cur=='done':
                    if not a.note.strip():raise LedgerError(f'{new} requires a note pointing to real test/evidence or the blocker')
                record(a.task_id,new,a.note,a.owner)
                if cur=='done':
                    for d in sorted(dependents(source,a.task_id)):
                        if status(d) in ('running','done'):record(d,'blocked',f'dependency {a.task_id} reopened: {a.note}')
                atomic_json(statefile,data)
            elif a.action=='next':
                candidates=[t for t in source if ready(t)]
                print(json.dumps(candidates[0] if candidates else {'message':'No ready pending task; inspect running/blocked tasks, outline requirements and actual evidence.'},ensure_ascii=False,indent=2))
            else:
                for t in source:
                    s=data['tasks'].get(t['id'],{});owner=f" @{s['owner']}" if s.get('owner') and status(t['id'])=='running' else ''
                    flag='' if not readiness_problems(t,maturity) else ' (needs detailed spec)'
                    print(f"{t['id']} [{status(t['id'])}{owner}] {t.get('kind','?')}{'/writer' if t.get('writer') else ''} {t['title']}{flag}")
        except LedgerError as exc:p.error(str(exc))
    return 0
if __name__=='__main__':sys.exit(main())
