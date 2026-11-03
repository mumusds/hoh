"""Executable design reference for invariant tests; NOT the Rust product runtime."""
from __future__ import annotations
from dataclasses import dataclass
from typing import Iterable
@dataclass(frozen=True)
class Task:
    id: str
    deps: tuple[str,...]=()
    severity: int=2 # 0: critical/regression, 1: unblock, 2: normal
    value: float=1
    unblock: float=0
    risk: float=0
    information: float=0
    age: float=0
    cost: float=1
    seq: int=0
    waited: int=0         # scheduling rounds eligible but not selected
    eligible_since: int=0 # sequence number of the first round it became eligible
MAX_WAIT_ROUNDS=20  # REQ-038 (CR-002): bounded waiting via an aged FIFO sub-layer between layers 0 and 1

def topo(tasks: Iterable[Task]) -> list[str]:
    items=list(tasks);m={t.id:t for t in items}
    if len(m)!=len(items):raise ValueError('duplicate task')
    if any(d not in m for t in items for d in t.deps):raise ValueError('missing dependency')
    indeg={t.id:len(set(t.deps)) for t in items};edges={t.id:[] for t in items}
    for t in items:
        for d in set(t.deps):edges[d].append(t.id)
    ready=sorted(i for i,d in indeg.items() if d==0);result=[]
    while ready:
        i=ready.pop(0);result.append(i)
        for n in edges[i]:
            indeg[n]-=1
            if indeg[n]==0:ready.append(n);ready.sort()
    if len(result)!=len(items):raise ValueError('cycle')
    return result

def choose(tasks: Iterable[Task],accepted:set[str],leased:set[str],budget:float,max_wait_rounds:int=MAX_WAIT_ROUNDS) -> str|None:
    items=list(tasks);topo(items)
    eligible=[t for t in items if t.id not in accepted|leased and set(t.deps)<=accepted and max(t.cost,1)<=budget]
    def key(t:Task):
        if t.cost<0 or not 0<=t.severity<=2:raise ValueError('bad task parameters')
        if t.severity>0 and t.waited>=max_wait_rounds:return 0.5,0,t.eligible_since,t.seq,t.id  # aged FIFO sub-layer
        score=(4*t.value+3*t.unblock+3*t.risk+2*t.information+min(max(t.age,0),10))/max(t.cost,1)
        return t.severity,-score,0,t.seq,t.id
    return min(eligible,key=key).id if eligible else None

def decision_current(expected_revision:int,current_revision:int,epoch:int,current_epoch:int)->bool:
    return expected_revision==current_revision and epoch==current_epoch

def evidence_valid(e:dict,candidate:dict)->bool:
    return bool(e.get('observations')) and e.get('status')=='PASS' and e.get('source_unchanged') is True and all(
        e.get(k)==candidate.get(k) and candidate.get(k) for k in ['candidate_id','spec_sha256','environment_sha256','test_plan_sha256'])

def project_event(state:dict,event:dict)->dict:
    seq=event['seq']
    if seq<=state.get('last_seq',0):return dict(state)
    out=dict(state);out['last_seq']=seq;out['last_kind']=event['kind']
    if event['kind']=='requirement.accepted':out['accepted']=set(state.get('accepted',set()))|{event['req']}
    if event['kind']=='requirement.invalidated':out['accepted']=set(state.get('accepted',set()))-{event['req']}
    return out
