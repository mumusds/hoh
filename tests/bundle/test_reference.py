import unittest
from helpers import ROOT
from reference_model import Task,topo,choose,decision_current,evidence_valid,project_event

class Scheduling(unittest.TestCase):
    def test_topological_order(self):self.assertEqual(topo([Task('b',('a',)),Task('a')]),['a','b'])
    def test_duplicate(self):
        with self.assertRaisesRegex(ValueError,'duplicate'):topo([Task('a'),Task('a')])
    def test_cycle(self):
        with self.assertRaisesRegex(ValueError,'cycle'):topo([Task('a',('b',)),Task('b',('a',))])
    def test_missing_dependency(self):
        with self.assertRaisesRegex(ValueError,'missing'):topo([Task('a',('missing',))])
    def test_dependency_blocks(self):self.assertEqual(choose([Task('a'),Task('b',('a',),value=100)],set(),set(),100),'a')
    def test_accepted_unblocks(self):self.assertEqual(choose([Task('a'),Task('b',('a',))],{'a'},set(),5),'b')
    def test_leased_not_chosen(self):self.assertIsNone(choose([Task('a')],set(),{'a'},5))
    def test_budget_limit(self):self.assertIsNone(choose([Task('a',cost=6)],set(),set(),5))
    def test_hard_severity(self):self.assertEqual(choose([Task('a',severity=0,value=0),Task('b',severity=2,value=100)],set(),set(),5),'a')
    def test_stable_tie(self):self.assertEqual(choose([Task('b'),Task('a')],set(),set(),5),'a')
    def test_aging_priority(self):self.assertEqual(choose([Task('a'),Task('b',age=10)],set(),set(),5),'b')
    def test_bad_cost(self):
        with self.assertRaises(ValueError):choose([Task('a',cost=-1)],set(),set(),5)
    def test_current_decision(self):self.assertTrue(decision_current(2,2,3,3))
    def test_stale_revision(self):self.assertFalse(decision_current(2,3,3,3))
    def test_stale_role_epoch(self):self.assertFalse(decision_current(2,2,2,3))

class EvidenceAndProjection(unittest.TestCase):
    def setUp(self):self.c={'candidate_id':'a','spec_sha256':'s','environment_sha256':'e','test_plan_sha256':'t'};self.e=self.c|{'observations':['actual'],'status':'PASS','source_unchanged':True}
    def test_valid(self):self.assertTrue(evidence_valid(self.e,self.c))
    def test_empty_evidence(self):self.e['observations']=[];self.assertFalse(evidence_valid(self.e,self.c))
    def test_source_changed(self):self.e['source_unchanged']=False;self.assertFalse(evidence_valid(self.e,self.c))
    def test_wrong_environment(self):self.e['environment_sha256']='other';self.assertFalse(evidence_valid(self.e,self.c))
    def test_failed(self):self.e['status']='FAIL';self.assertFalse(evidence_valid(self.e,self.c))
    def test_duplicate_event(self):
        e={'seq':1,'kind':'requirement.accepted','req':'r'};s=project_event({},e);self.assertEqual(project_event(s,e),s)
    def test_invalidation(self):
        s=project_event({},{'seq':1,'kind':'requirement.accepted','req':'r'});s=project_event(s,{'seq':2,'kind':'requirement.invalidated','req':'r'});self.assertEqual(s['accepted'],set())
    def test_old_event_ignored(self):self.assertEqual(project_event({'last_seq':5},{'seq':4,'kind':'unused'}),{'last_seq':5})
