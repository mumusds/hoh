"""REQ-032 / AC-032-01 (TEST-032-01, check traceability): the product's trace export checked by its mirror in
scripts/product/traceability.py (docs/adr/ADR-039.md decision 3). tests/bundle/fixtures/trace-export.json is the
product's current export of the fixture project (2 REQs, 4 ACs, 3 Tasks, 1 candidate): crates/hoh-app/tests/trace.rs
regenerates it through the product host and fails while the committed file differs. The defects are injected into a
copy of it, never into a product store."""
import copy,json,unittest
from helpers import ROOT,load_json
from test_traceability import tr

FIXTURE=ROOT/'tests/bundle/fixtures/trace-export.json'
ORDER=('requirement','acceptance','task','candidate','test','evidence','result')


def reach(graph,start):
    seen,todo={start},[start]
    while todo:
        for nxt in graph.get(todo.pop(),()):
            if nxt not in seen:seen.add(nxt);todo.append(nxt)
    return seen


class TraceExport(unittest.TestCase):
    def setUp(self):self.export=load_json(FIXTURE)

    def assert_chains(self,export,skip=()):
        """Every node but those of `skip` reaches every kind after its own along the chain and every kind before it
        against it (REQ-032 behaviour 1)."""
        forward=tr.links(export);backward={}
        for a,bs in forward.items():
            backward.setdefault(a,set())
            for b in bs:backward.setdefault(b,set()).add(a)
        for node in forward:
            if node in skip:continue
            at=ORDER.index(node[0])
            ahead={k for k,_ in reach(forward,node)};behind={k for k,_ in reach(backward,node)}
            self.assertTrue(set(ORDER[at:])<=ahead,f'{node}: forward reaches only {sorted(ahead)}')
            self.assertTrue(set(ORDER[:at+1])<=behind,f'{node}: backward reaches only {sorted(behind)}')
        return forward

    def test_032_01_export_links_broken_and_orphan(self):
        e=self.export
        self.assertEqual((len(e['requirements']),len(e['acceptance']),len(e['tasks']),len(e['candidates'])),(2,4,3,1))
        self.assertIsNotNone(e['candidate_id'])
        # The mirror computes from the nodes what the product wrote; the fixture's chains are whole.
        mirror=tr.chain(e)
        self.assertEqual(mirror,{k:e[k] for k in ('broken','orphan','stale')})
        self.assertEqual(mirror,{'broken':[],'orphan':[],'stale':[]})
        forward=self.assert_chains(e)
        self.assertEqual({k for k,_ in forward},set(ORDER))
        # Inject: drop one TEST's evidence; rebind another AC's PASS to an older candidate.
        bad=copy.deepcopy(e);gone,moved=bad['tests'][0],bad['tests'][1]
        dropped=next(r['evidence_id'] for r in bad['results'] if r['ac_id']==gone['ac_id'])
        bad['evidence']=[x for x in bad['evidence'] if x['evidence_id']!=dropped]
        rebound=next(r for r in bad['results'] if r['ac_id']==moved['ac_id'])
        rebound['candidate_id']='CAND-'+'0'*64
        report=tr.chain(bad)
        self.assertEqual(report['broken'],[{'node':'test','id':gone['test_id'],'missing':'evidence'}])
        self.assertEqual(report['orphan'],[{k:rebound[k] for k in ('ac_id','spec_revision','candidate_id','evidence_id',
                                                                    'status')}])
        self.assertEqual(report['stale'],[])
        # The other chains are whole: every node outside the broken TEST's chain still reaches both ways.
        cut={('test',gone['test_id']),('acceptance',gone['ac_id'])}
        cut|={n for n in tr.links(bad) if n[0]=='requirement' and gone['ac_id'] in
              next(r['ac_ids'] for r in bad['requirements'] if r['requirement_id']==n[1])}
        self.assert_chains(bad,skip=cut)
