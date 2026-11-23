"""REQ-032 / AC-032-01 (TEST-032-01, check traceability): the product's trace exports checked by their mirror in
scripts/product/traceability.py (docs/adr/ADR-039.md decision 3 and its round 1 corrections, M3N-005).
tests/bundle/fixtures/trace-export.json holds the product's current exports of the fixture project (2 REQs, 4 ACs, 3
Tasks, 1 candidate), whole and over three faults written into the product's store: one TEST's evidence missing, a PASS
on a STALE older candidate, the candidate row missing. crates/hoh-app/tests/trace.rs
(test_032_01_product_export_over_fault_variants_matches_the_mirror) produces them through the product host, compares
each with the mirror there, and fails while the committed file differs from what the product exports now."""
import json,unittest
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
        self.assertEqual(set(self.export),{'whole','missing_evidence','stale_candidate','missing_candidate'})
        # Every product export, item by item: the mirror computes from its nodes what the product wrote.
        for name,e in self.export.items():
            with self.subTest(variant=name):
                self.assertEqual(tr.chain(e),{k:e[k] for k in ('broken','orphan','stale')})
        e=self.export['whole']
        self.assertEqual((len(e['requirements']),len(e['acceptance']),len(e['tasks']),len(e['candidates'])),(2,4,3,1))
        self.assertIsNotNone(e['candidate_id'])
        self.assertEqual({k:e[k] for k in ('broken','orphan','stale')},{'broken':[],'orphan':[],'stale':[]})
        forward=self.assert_chains(e)
        self.assertEqual({k for k,_ in forward},set(ORDER))
        # One TEST's evidence missing: only that TEST is broken, the other chains are whole both ways.
        bad=self.export['missing_evidence']
        self.assertEqual(bad['broken'],[{'node':'test','id':'TEST-A-01','missing':'evidence'}])
        self.assertEqual((bad['orphan'],bad['stale']),([],[]))
        cut={('test','TEST-A-01'),('acceptance','AC-A-01'),('requirement','REQ-A')}
        self.assert_chains(bad,skip=cut)
        # A PASS on an older STALE candidate: an orphan, and stale.
        old=self.export['stale_candidate']
        rebound=[{'ac_id':'AC-B-01','spec_revision':1,'candidate_id':'CAND-old','evidence_id':'evidence-AC-B-01',
                  'status':'PASS'}]
        self.assertEqual((old['broken'],old['orphan'],old['stale']),([],rebound,rebound))
        # The candidate row missing: its evidence and results are still there, each evidence names its missing link.
        gone=self.export['missing_candidate']
        self.assertIsNone(gone['candidate_id'])
        self.assertEqual(len(gone['evidence']),4);self.assertEqual(len(gone['results']),4)
        self.assertEqual(gone['broken'],[{'node':'evidence','id':x['evidence_id'],'missing':'candidate'}
                                         for x in gone['evidence']])
