#!/usr/bin/env python3
"""Small locked development ledger, not the HoH product task database."""
from __future__ import annotations
import argparse, fcntl, json, sys
from pathlib import Path
from common import ROOT,atomic_json,load_json,utcnow

def main(argv=None):
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('action',choices=['list','next','set']);p.add_argument('task_id',nargs='?');p.add_argument('status',nargs='?',choices=['pending','running','done','blocked']);p.add_argument('--note',default='');a=p.parse_args(argv)
    source=load_json(ROOT/'contracts/development-tasks.json')['tasks'];index={t['id']:t for t in source}
    statefile=ROOT/'.hoh-dev/task-state.json';statefile.parent.mkdir(exist_ok=True)
    with (statefile.parent/'task-state.lock').open('a+') as lock:
        fcntl.flock(lock,fcntl.LOCK_EX)
        data=load_json(statefile) if statefile.exists() else {'tasks':{},'history':[]}
        def status(i):return data['tasks'].get(i,{}).get('status','pending')
        if a.action=='set':
            if a.task_id not in index or not a.status:p.error('valid task_id and status required')
            t=index[a.task_id]
            if a.status in ['running','done'] and any(status(d)!='done' for d in t['depends_on']):p.error('dependencies not done')
            if a.status=='done' and not a.note.strip():p.error('done requires a note pointing to real test/evidence')
            entry={'task_id':a.task_id,'status':a.status,'note':a.note,'at':utcnow()};data['tasks'][a.task_id]=entry;data['history'].append(entry);atomic_json(statefile,data)
        elif a.action=='next':
            ready=[t for t in source if status(t['id'])=='pending' and all(status(d)=='done' for d in t['depends_on'])]
            print(json.dumps(ready[0] if ready else {'message':'No ready pending task; inspect running/blocked and actual evidence.'},ensure_ascii=False,indent=2))
        else:
            for t in source:print(f"{t['id']} [{status(t['id'])}] {t['title']}")
    return 0
if __name__=='__main__':sys.exit(main())
