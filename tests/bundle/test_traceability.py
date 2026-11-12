"""Principle one's replica, scripts/product/traceability.py (docs/adr/ADR-027.md principle one, decisions 10 and 12).
Synthetic fixtures only: NEVER product acceptance evidence."""
import copy,importlib.util,json,os,shutil,subprocess,sys,tempfile,unittest
from pathlib import Path
from helpers import ROOT,ReleaseFixture,json_file,load_json,own_python,ref_for,sha256
from release_gate import GateError,validate_manifest

def load_replica():
    spec=importlib.util.spec_from_file_location('traceability',ROOT/'scripts/product/traceability.py')
    module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module);return module
tr=load_replica()
DETAIL='pi plugin hook API incompatible with the installed Pi; upstream: example/upstream#1'
SELF_TESTS=('TEST-032-01','TEST-052-05','TEST-069-01')

class Envelope(unittest.TestCase):
    """A complete synthetic release envelope (tests/bundle/helpers.py) judged by the gate and by the replica."""
    profile='v1';overrides=None
    def setUp(self):
        self.f=ReleaseFixture(self.profile,plan_overrides=self.overrides);self.addCleanup(self.f.close);self.m=self.f.manifest
        self.requirements=load_json(self.f.root/'contracts/requirements.json')['requirements']
    def gate(self):
        """The gate's first error, or None when it passes."""
        try:validate_manifest(self.f.root,self.m)
        except (GateError,ValueError,KeyError,TypeError) as exc:return str(exc)
        return None
    def world(self):return tr.World(self.f.root,self.f.candidate,self.profile,requirements=self.requirements)
    def judged(self):w=self.world();return w,tr.members(w)
    def member(self,found,key):
        rel=self.f.dirs[key]+'/execution.json';return next(m for m in found if m.rel==rel)
    def report(self,own=()):w,found=self.judged();return tr.report(w,found,list(own))
    def scenario_tests(self,check):
        return sorted(t for t,test in self.world().tests().items() if test.check==check)
    def lines_of(self,key):
        art=next(a for a in self.f.records[key]['artifacts'] if a['name']=='test-results.jsonl')
        return [json.loads(l) for l in (self.f.root/art['path']).read_text().splitlines() if l.strip()]
    def another(self,key,name,lines=None,**fields):
        """Another run of check `key` on the candidate, in a directory and with a record id of its own: its outputs
        copied, its results `lines` (default: the original's), its record fields replaced by `fields`."""
        src=self.f.root/self.f.dirs[key];dst=self.f.root/'.hoh-dev/evidence'/name
        shutil.copytree(src,dst);record=copy.deepcopy(self.f.records[key])
        old=self.f.dirs[key];new=f'.hoh-dev/evidence/{name}'
        for ref in [record['stdout'],record['stderr']]+record['artifacts']:ref['path']=new+ref['path'][len(old):]
        record['record_id']=record['producer_attempt_id']='run-'+name
        (dst/'execution.json').unlink()
        if lines is not None:
            art=next(a for a in record['artifacts'] if a['name']=='test-results.jsonl')
            p=self.f.root/art['path'];p.write_text(''.join(json.dumps(l)+'\n' for l in lines));art['sha256']=sha256(p)
        record.update(fields);json_file(self.f.root,new+'/execution.json',record);return new+'/execution.json'

def own(*test_ids,status='PASS'):
    return [{'schema_version':'1.0','test_id':t,'name':'self_'+t,'status':status} for t in test_ids]

class GateComparison(Envelope):
    """Each case starts from the complete envelope both pass, then replaces one record by a variant: the gate rejects
    it with its rule's first error, and the replica vetoes it or concludes as the differences table lists
    (docs/adr/ADR-027.md, 与门禁的对照)."""
    def setUp(self):
        super().setUp()
        self.assertIsNone(self.gate())
        w,found=self.judged();self.assertEqual([(m.rel,m.reasons) for m in found if m.reasons],[])
    def vetoed(self,key,code,gate):
        self.assertRegex(self.gate() or 'gate passed',gate)
        w,found=self.judged();m=self.member(found,key)
        self.assertIn(code,m.reasons);self.assertEqual(m.outcome[0],'INVALID')
        rep=tr.report(w,found,own(*SELF_TESTS))
        self.assertIn(m.rel,[i['path'] for i in rep['invalid']])
        return m
    def concluded(self,key,gate,outcome,status='NOT_RUN'):
        """The gate rejects; the replica keeps the record valid and its outcome concludes nothing on its TESTs."""
        self.assertRegex(self.gate() or 'gate passed',gate)
        w,found=self.judged();m=self.member(found,key)
        self.assertEqual((m.reasons,m.outcome),([],outcome))
        rep=tr.report(w,found,own(*SELF_TESTS));self.assertEqual(rep['invalid'],[])
        for t in self.scenario_tests(key):self.assertEqual(rep['tests'][t]['status'],status,t)
        return rep
    def test_the_complete_envelope_passes_both(self):
        rep=self.report()
        self.assertEqual(rep['invalid'],[])
        self.assertEqual({t:v['status'] for t,v in rep['tests'].items() if v['status']!='PASS'},{t:'NOT_RUN' for t in SELF_TESTS})
        self.assertEqual(self.report(own(*SELF_TESTS))['unsatisfied'],[])
    # Rule 1.
    def test_rule_01_a_record_off_its_schema(self):
        self.f.edit_execution('unit',exit_code='zero')
        self.assertTrue(self.vetoed('unit','RECORD_INVALID','execution.schema.json').damaged)
    def test_rule_01_a_time_that_is_no_rfc3339_date_time(self):
        # M2T10R-750: Python's fromisoformat takes a space for the 'T'; the schema's date-time format does not.
        self.f.edit_execution('unit',started_at='2020-01-01 00:00:00+00:00')
        self.vetoed('unit','RECORD_INVALID','execution.schema.json')
    def test_rule_01_the_replica_refuses_to_judge_without_a_date_time_checker(self):
        # M2T10R-750: jsonschema installed without requirements-dev.txt's format extras would accept any string there.
        class NoDateTime(tr.FormatChecker):
            def __init__(self):super().__init__();self.checkers.pop('date-time',None)
        original=tr.FormatChecker;tr.FormatChecker=NoDateTime
        try:
            with self.assertRaisesRegex(ValueError,'date-time'):self.world()
        finally:tr.FormatChecker=original
    def test_rule_01_an_unreadable_record_holding_the_candidate(self):
        row=self.f.check_row('unit');p=self.f.root/row['execution']['path']
        p.write_text('{"candidate_id": "'+self.f.candidate['candidate_id']+'", cut');row['execution']=ref_for(p,self.f.root)
        m=self.vetoed('unit','RECORD_INVALID','Expecting');self.assertTrue(m.damaged);self.assertIsNone(m.check)
    def test_rule_01_a_result_line_off_its_schema(self):
        self.f.edit_test_results('unit',[{'schema_version':'1.0','test_id':'TEST-4-1','name':'x','status':'PASS'}])
        self.vetoed('unit','RESULT_LINE_INVALID','line 1')
    def test_rule_01_a_result_line_that_is_no_json(self):
        self.f.rewrite_artifact('unit','test-results.jsonl','not json\n');self.vetoed('unit','RESULT_LINE_INVALID','invalid JSON')
    # Rule 2.
    def test_rule_02_spec(self):self.f.edit_execution('unit',spec_sha256='0'*64);self.vetoed('unit','SPEC_MISMATCH','stale candidate/spec')
    def test_rule_02_environment(self):
        self.f.edit_execution('unit',environment_sha256='0'*64);self.vetoed('unit','ENVIRONMENT_MISMATCH','environment/test plan changed')
    def test_rule_02_test_plan(self):
        self.f.edit_execution('unit',test_plan_sha256='0'*64);self.vetoed('unit','TEST_PLAN_MISMATCH','environment/test plan changed')
    # Rule 3: every record and the release manifest rebound to the variant's own manifest reference first.
    def rebind_manifest(self,ref):
        for key in list(self.f.records):self.f.edit_execution(key,candidate_manifest=ref)
        self.m['candidate_manifest']=ref
    def test_rule_03_manifest_bytes(self):
        self.rebind_manifest(dict(self.m['candidate_manifest'],sha256='0'*64));self.vetoed('unit','MANIFEST_MISMATCH','hash mismatch')
    def test_rule_03_a_manifest_that_is_no_candidate_export(self):
        self.rebind_manifest(dict(self.f.records['unit']['stdout']));self.vetoed('unit','MANIFEST_MISMATCH','Expecting')
    # Rule 4.
    def test_rule_04_command(self):self.f.edit_execution('unit',command=['true']);self.vetoed('unit','COMMAND_MISMATCH','differs from the frozen check plan')
    def test_rule_04_mode(self):self.f.edit_execution('adapter-live',mode='mock');self.vetoed('adapter-live','MODE_MISMATCH','needs live')
    def test_rule_04_cwd(self):self.f.edit_execution('unit',cwd='elsewhere');self.vetoed('unit','CWD_MISMATCH','different cwd')
    def test_rule_04_a_check_neither_planned_nor_in_the_policy(self):
        rel=self.another('unit','made-up',check_id='made-up')
        self.m['checks'].append({'id':'made-up','status':'PASS','execution':ref_for(self.f.root/rel,self.f.root)})
        self.assertRegex(self.gate(),'not a release-policy check')
        w,found=self.judged();m=next(x for x in found if x.rel==rel)
        self.assertEqual(set(m.reasons)&{'CHECK_NOT_PLANNED','CHECK_NOT_IN_POLICY'},{'CHECK_NOT_PLANNED','CHECK_NOT_IN_POLICY'})
    # Rule 5.
    def test_rule_05_tool(self):self.f.edit_execution('unit',runner={'tool':'handwritten','euid':1000});self.vetoed('unit','RUNNER_TOOL','run_check/hoh')
    def test_rule_05_root(self):self.f.edit_execution('unit',runner={'tool':'tools/run_check.py','euid':0});self.vetoed('unit','RUNNER_ROOT','non-root')
    # Rule 6.
    def test_rule_06_source_changed(self):self.f.edit_execution('unit',source_unchanged=False);self.vetoed('unit','SOURCE_CHANGED','source changed')
    # Rule 7.
    def test_rule_07_stdout(self):
        (self.f.root/self.f.records['unit']['stdout']['path']).write_text('tampered');self.vetoed('unit','OUTPUT_CHANGED','hash mismatch')
    def test_rule_07_stderr(self):
        (self.f.root/self.f.records['unit']['stderr']['path']).write_text('tampered');self.vetoed('unit','ERROR_OUTPUT_CHANGED','hash mismatch')
    def test_rule_07_artifact(self):
        bundle=self.f.records['build']['artifacts'][0];(self.f.root/bundle['path']).write_bytes(b'tampered')
        self.vetoed('build','ARTIFACT_CHANGED','hash mismatch')
    def test_rule_07_outside_its_directory(self):
        self.f.edit_execution('unit',stdout=self.f.records['build']['stdout']);self.vetoed('unit','OUTSIDE_RECORD_DIR','outside its execution directory')
    def test_rule_07_directories_overlap(self):
        rel=self.another('unit','build/nested-unit');shutil.rmtree(self.f.root/self.f.dirs['unit'])
        self.f.check_row('unit')['execution']=ref_for(self.f.root/rel,self.f.root);self.f.dirs['unit']='.hoh-dev/evidence/build/nested-unit'
        self.f.records['unit']=load_json(self.f.root/rel)
        self.vetoed('unit','RECORD_DIR_OVERLAP','overlaps');w,found=self.judged()
        self.assertIn('RECORD_DIR_OVERLAP',self.member(found,'build').reasons)
    # Rule 8.
    def test_rule_08_artifacts_differ_from_the_plan(self):
        self.f.edit_execution('unit',artifacts=[],missing_artifacts=[]);self.vetoed('unit','ARTIFACTS_DIFFER_FROM_PLAN','artifacts differ')
    # Rule 9.
    def test_rule_09_an_interval_that_runs_backwards(self):
        r=self.f.records['unit'];self.f.edit_execution('unit',started_at=r['finished_at'],finished_at=r['started_at'])
        self.vetoed('unit','INTERVAL_INVALID','inconsistent duration')
    def test_rule_09_a_duration_that_disagrees_is_a_conclusion(self):
        self.f.edit_execution('unit',duration_seconds=200);self.concluded('unit','inconsistent duration',('FAIL','INCOMPLETE'))
    # Rule 10.
    def test_rule_10_sessions_bound_elsewhere(self):
        s=load_json(self.f.root/self.f.records['adapter-live']['artifacts'][0]['path']);s['harness_candidate_id']='CAND-'+'1'*64
        self.f.rewrite_artifact('adapter-live','live-sessions.json',json.dumps(s));self.vetoed('adapter-live','LIVE_SESSIONS_UNBOUND','another check/candidate')
    def test_rule_10_sessions_off_their_schema(self):
        s=load_json(self.f.root/self.f.records['adapter-live']['artifacts'][0]['path']);s['sessions']=[]
        self.f.rewrite_artifact('adapter-live','live-sessions.json',json.dumps(s));self.vetoed('adapter-live','LIVE_SESSIONS_UNBOUND','live-session')
    # Rule 11 (stricter than the gate, which judges only the scenario check's TEST).
    def test_rule_11_exit_0_with_a_failing_line(self):
        lines=self.lines_of('unit');lines[0]['status']='FAIL';self.f.edit_test_results('unit',lines)
        self.vetoed('unit','EXIT_INCONSISTENT','test results for')
    # Rule 12.
    def test_rule_12_artifacts_missing_on_a_success_claim(self):
        self.f.edit_execution('soak',missing_artifacts=['timeline.jsonl']);self.vetoed('soak','ARTIFACT_MISSING','artifacts missing')
    def test_rule_12_artifacts_missing_without_a_success_claim_conclude_nothing(self):
        r=self.f.records['unit'];self.f.edit_execution('unit',exit_code=3,artifacts=[],missing_artifacts=['test-results.jsonl'])
        self.concluded('unit','command failed',('BLOCKED',None))
    # Rule 13: timeouts and exit codes are conclusions.
    def test_rule_13_a_timeout(self):
        self.f.edit_execution('unit',timed_out=True);self.concluded('unit','timed out',('FAIL','TIMED_OUT'))
    def test_rule_13_an_exit_code(self):
        self.f.edit_execution('unit',exit_code=1);self.concluded('unit','command failed',('FAIL',None))
    # Rule 14.
    def degrade_plugins(self,detail=True):
        self.f.degrade_check('plugin-live');self.f.edit_execution('plugin-live',exit_code=3)
        for ac in sorted(a for a,c in self.f.ac_check.items() if c=='plugin-live'):self.f.degrade_ac(ac,'plugin-live')
        if detail:
            lines=self.lines_of('plugin-live')
            for l in lines:
                if l['status']=='DEGRADED':l['detail']=DETAIL
            self.f.edit_test_results('plugin-live',lines)
    def test_rule_14_a_compliant_degradation_degrades_both(self):
        self.degrade_plugins();self.assertEqual(validate_manifest(self.f.root,self.m)['release_status'],'COMPLETED_DEGRADED')
        w,found=self.judged();self.assertEqual(self.member(found,'plugin-live').outcome,('DEGRADED',None))
        rep=tr.report(w,found,own(*SELF_TESTS))
        self.assertEqual({rep['tests'][t]['status'] for t in self.scenario_tests('plugin-live')},{'DEGRADED'});self.assertEqual(rep['unsatisfied'],[])
    def test_rule_14_the_basis_in_the_line_is_stricter_than_the_gate(self):
        # Listed difference (M2T10D-803): the gate takes the basis from the manifest and the observation; the replica
        # and the product need it in the DEGRADED line's detail.
        self.degrade_plugins(detail=False);self.assertIsNone(self.gate())
        rep=self.report(own(*SELF_TESTS))
        self.assertEqual({rep['tests'][t]['status'] for t in self.scenario_tests('plugin-live')},{'FAIL'})
    # Rule 15.
    def test_rule_15_plugins_missing(self):
        s=load_json(self.f.root/self.f.records['plugin-live']['artifacts'][0]['path'])
        for x in s['sessions']:x.pop('plugins',None)
        self.f.rewrite_artifact('plugin-live','live-sessions.json',json.dumps(s))
        self.concluded('plugin-live','list the plugins',('FAIL','PLUGINS_MISSING'))
    # Rule 16.
    def test_rule_16_the_profile_agents(self):
        s=load_json(self.f.root/self.f.records['adapter-live']['artifacts'][0]['path']);s['sessions']=s['sessions'][:1]
        self.f.rewrite_artifact('adapter-live','live-sessions.json',json.dumps(s))
        self.concluded('adapter-live','lack required agents',('PASS',None))
    # Rule 17: the producer needs the product's store; neither the gate nor the replica checks it.
    def test_rule_17_is_not_the_replicas(self):
        self.f.edit_execution('unit',producer_attempt_id='developer-attempt');self.assertIsNone(self.gate())
        self.assertEqual(self.report(own(*SELF_TESTS))['unsatisfied'],[])
    # Rule 18.
    def test_rule_18_a_soak_event_of_another_candidate(self):
        p=self.f.root/self.m['soak']['timeline']['path'];lines=p.read_text().splitlines();e=json.loads(lines[0]);e['harness_candidate_id']='CAND-'+'2'*64
        self.f.rewrite_artifact('soak','timeline.jsonl','\n'.join([json.dumps(e)]+lines[1:])+'\n')
        self.concluded('soak','different harness candidate',('PASS',None))
    def test_rule_18_a_short_soak(self):
        self.f.edit_execution('soak',duration_seconds=1,finished_at='2020-01-01T00:00:01+00:00')
        self.assertRegex(self.gate(),'soak duration|outside execution window')
        rep=self.report(own(*SELF_TESTS))
        for t in self.scenario_tests('soak'):self.assertEqual(rep['tests'][t]['status'],'NOT_RUN')
    # Rule 19.
    def test_rule_19_a_record_id_used_twice(self):
        self.f.edit_execution('unit',record_id=self.f.records['build']['record_id'])
        self.vetoed('unit','RECORD_ID_REUSED','reused');w,found=self.judged()
        self.assertIn('RECORD_ID_REUSED',self.member(found,'build').reasons)
    # Listed differences beyond the rule rows.
    def test_release_level_checks_are_the_gates(self):
        # M2T10D-802: an empty release bundle is the gate's release-level check, not a per-record rule.
        self.f.rewrite_artifact('build','hoh-release.tar.gz','')
        bundle=next(a for a in self.f.records['build']['artifacts'] if a['name']=='hoh-release.tar.gz')
        self.m['release_artifact']={'path':bundle['path'],'sha256':bundle['sha256']}
        self.assertRegex(self.gate(),'empty artifact')
        w,found=self.judged();self.assertEqual((self.member(found,'build').reasons,self.member(found,'build').outcome),([],('PASS',None)))
    def test_a_failing_line_in_a_check_that_is_not_the_scenario(self):
        t=self.scenario_tests('unit')[0];static=self.lines_of('static')+[{'schema_version':'1.0','test_id':t,'name':'cross','status':'FAIL'}]
        self.f.edit_test_results('static',static);self.f.edit_execution('static',exit_code=1)
        self.assertRegex(self.gate(),'static: command failed')
        self.assertEqual(self.report(own(*SELF_TESTS))['tests'][t]['status'],'FLAKY')
    def test_rule_11_on_a_check_that_is_not_the_scenario(self):
        # Listed difference (M2T10R-54): the gate reads only the scenario check's lines; the replica vetoes any record
        # claiming success with a FAIL line.
        t=self.scenario_tests('unit')[0]
        self.f.edit_test_results('static',self.lines_of('static')+[{'schema_version':'1.0','test_id':t,'name':'cross','status':'FAIL'}])
        self.assertIsNone(self.gate());self.vetoed_without_gate('static','EXIT_INCONSISTENT')
    def test_a_degraded_line_on_a_check_that_is_not_the_scenario(self):
        # Listed difference (M2T10R-54): conclusion (3) on a non-scenario check's DEGRADED line, which the gate never reads.
        t=self.scenario_tests('unit')[0]
        self.f.edit_test_results('static',self.lines_of('static')+[{'schema_version':'1.0','test_id':t,'name':'cross','status':'DEGRADED','detail':DETAIL}])
        self.assertIsNone(self.gate())
        self.assertEqual(self.report(own(*SELF_TESTS))['tests'][t]['status'],'FLAKY')
    def vetoed_without_gate(self,key,code):
        w,found=self.judged();m=self.member(found,key);self.assertIn(code,m.reasons)
    def test_a_check_a_session_runs_beside_the_independent_record_is_no_overlap(self):
        # M2T10R-52, -152: a run_check record in <record>-observations beside the independent check's record overlaps
        # nothing; under the record's own directory it would veto both.
        self.another('unit','independent-qa-observations/unit-rerun');self.assertIsNone(self.gate())
        self.assertEqual(self.report(own(*SELF_TESTS))['invalid'],[])
        self.another('unit','independent-qa/artifacts/observations/unit-rerun')
        invalid=self.report(own(*SELF_TESTS))['invalid']
        self.assertEqual(sorted(i['path'] for i in invalid),['.hoh-dev/evidence/independent-qa/artifacts/observations/unit-rerun/execution.json',
                                                            '.hoh-dev/evidence/independent-qa/execution.json'])
        self.assertTrue(all(i['reasons']==['RECORD_DIR_OVERLAP'] for i in invalid),invalid)
    def test_a_member_outside_the_manifest(self):
        t=self.scenario_tests('unit')[0];lines=self.lines_of('unit')
        for l in lines:
            if l['test_id']==t:l['status']='FAIL'
        self.another('unit','unit-earlier',lines=lines,exit_code=101)
        self.assertIsNone(self.gate())
        rep=self.report(own(*SELF_TESTS));self.assertEqual(rep['tests'][t]['status'],'FLAKY');self.assertIn({'id':t,'status':'FLAKY'},rep['unsatisfied'])

class PlanModeNotPolicy(Envelope):
    overrides={'unit':{'mode':'mock'}}
    def test_rule_04_the_plans_mode_is_not_the_policys(self):
        self.assertRegex(self.gate(),'check plan mode mock but policy needs product')
        w,found=self.judged();self.assertIn('PLAN_MODE_NOT_POLICY',self.member(found,'unit').reasons)

class LiveSessionsUndeclared(Envelope):
    overrides={'adapter-live':{'artifacts':['test-results.jsonl']}}
    def test_rule_10_a_live_success_claim_needs_its_sessions_declared_or_not(self):
        self.assertRegex(self.gate(),'live check lacks live-sessions.json')
        w,found=self.judged();self.assertIn('LIVE_SESSIONS_MISSING',self.member(found,'adapter-live').reasons)

class NothingObserved(Envelope):
    overrides={'static':{'artifacts':[]}}
    def test_rule_12_a_success_claim_that_observed_nothing_is_insufficient(self):
        self.assertRegex(self.gate(),'produced no test-results.jsonl')
        w,found=self.judged();m=self.member(found,'static')
        self.assertEqual((m.reasons,m.outcome),(['NO_OBSERVATIONS'],('INVALID','INSUFFICIENT')))

# The non-success shapes the locked writers and the product driver write (docs/adr/ADR-027.md principle one): exit
# code, timed_out, whether the duration agrees with the interval, the artifacts missing.
SHAPES=[('run_check timeout',124,True,True,()),('run_check timeout, artifacts missing',124,True,True,('live-sessions.json',)),
        ('command not started',127,False,True,('live-sessions.json','test-results.jsonl')),
        ('run_check interrupted',130,False,False,()),('run_check interrupted, artifacts missing',130,False,False,('live-sessions.json',)),
        ('not implemented',3,False,True,('live-sessions.json','test-results.jsonl')),('adapter-live blocked',3,False,True,('live-sessions.json',)),
        ('tests failed',101,False,True,()),('killed by a signal',-15,False,True,()),('killed, artifacts missing',-15,False,True,('live-sessions.json',)),
        ('product cancelled',130,False,True,()),('product deadline, child exit 0',0,True,True,()),('product unreaped',-9,False,True,()),
        ('exit 0',0,False,True,()),('exit 0, duration disagrees',0,False,False,()),('exit 0, artifacts missing',0,False,True,('live-sessions.json',)),
        ('exit 3, complete',3,False,True,())]
LINES={'none':None,'pass':'PASS','skipped':'SKIPPED','fail':'FAIL','degraded':'DEGRADED','bare':'DEGRADED'}

def derived(exit_code,timed_out,agrees,missing,kind):
    """The derived table of principle one, row by row: (outcome, conclusion on the scenario check's TEST)."""
    if exit_code==0 and not timed_out and (missing or kind=='fail'):return ('INVALID',None),'INVALID'
    complete=not missing and agrees
    if timed_out:out=('FAIL','TIMED_OUT')
    elif kind=='degraded' and complete:out=('DEGRADED',None)
    elif exit_code==0:out=('PASS',None) if complete else ('FAIL','INCOMPLETE')
    elif exit_code==3:out=('BLOCKED',None)
    elif exit_code==127:out=('FAIL','NOT_STARTED')
    elif exit_code==130 or exit_code<0:out=('FAIL','INTERRUPTED')
    else:out=('FAIL',None)
    if kind in ('fail','bare'):return out,'FAIL'
    if kind=='pass' and out[0]=='PASS':return out,'PASS'
    if kind=='degraded' and out[0]=='DEGRADED':return out,'DEGRADED'
    return out,None

class Shapes(Envelope):
    """Every shape with every kind of result line on plugin-live (a degradable live check of degradable requirements):
    the replica's outcome and conclusion are the derived table's (M2T10D-603, -703, -752, -853)."""
    def test_every_shape_concludes_as_the_derived_table_says(self):
        key='plugin-live';base=copy.deepcopy(self.f.records[key]);tests=self.scenario_tests(key);t=tests[0]
        start=base['started_at'];finish='2020-01-01T00:00:30+00:00'
        for label,exit_code,timed_out,agrees,missing in SHAPES:
            for kind,status in LINES.items():
                if kind!='none' and 'test-results.jsonl' in missing:continue
                with self.subTest(shape=label,lines=kind):
                    lines=[{'schema_version':'1.0','test_id':x,'name':'case_'+x,'status':status} for x in tests] if status else []
                    if kind=='degraded':
                        for l in lines:l['detail']=DETAIL
                    record=copy.deepcopy(base);art={a['name']:a for a in record['artifacts']}
                    p=self.f.root/art['test-results.jsonl']['path'];p.write_text(''.join(json.dumps(l)+'\n' for l in lines))
                    art['test-results.jsonl']['sha256']=sha256(p)
                    record.update(exit_code=exit_code,timed_out=timed_out,started_at=start,
                                  finished_at=start if exit_code==127 else finish,duration_seconds=(30 if agrees else 0) if exit_code!=127 else 0,
                                  artifacts=[a for a in record['artifacts'] if a['name'] not in missing],missing_artifacts=list(missing))
                    json_file(self.f.root,self.f.dirs[key]+'/execution.json',record)
                    w,found=self.judged();m=self.member(found,key);test=w.tests()[t]
                    out,conclusion=derived(exit_code,timed_out,agrees,missing,kind)
                    self.assertEqual(m.outcome if out[0]!='INVALID' else (m.outcome[0],None),out)
                    self.assertEqual(tr.conclude(w,m,t,test),conclusion)
                    # The same lines on a check that is not the TEST's scenario conclude only INVALID or FAIL: any
                    # DEGRADED line there is not compliant.
                    other=tr.Test('unit',test.degradable)
                    expected='INVALID' if conclusion=='INVALID' else 'FAIL' if kind in ('fail','bare','degraded') else None
                    self.assertEqual(tr.conclude(w,m,t,other),expected)

class History(Envelope):
    """TEST status over all members of the candidate (principle one): failures from any check count, satisfaction
    only from the scenario check, FAIL is never undone, vetoes make the report unsatisfied."""
    def setUp(self):super().setUp();self.t=self.scenario_tests('unit')[0]
    def failing_lines(self):
        lines=self.lines_of('unit')
        for l in lines:
            if l['test_id']==self.t:l['status']='FAIL'
        return lines
    def status(self,own_lines=own(*SELF_TESTS)):return self.report(own_lines)['tests'][self.t]['status']
    def test_fail_then_pass_is_flaky(self):self.another('unit','u2',lines=self.failing_lines(),exit_code=101);self.assertEqual(self.status(),'FLAKY')
    def test_only_fail_is_fail(self):
        self.f.edit_test_results('unit',self.failing_lines());self.f.edit_execution('unit',exit_code=101);self.assertEqual(self.status(),'FAIL')
    def test_a_blocked_run_then_a_pass_is_satisfied(self):
        self.another('unit','u2',exit_code=3,artifacts=[],missing_artifacts=['test-results.jsonl'])
        rep=self.report(own(*SELF_TESTS));self.assertEqual((rep['tests'][self.t]['status'],rep['invalid']),('PASS',[]))
    def test_an_interrupted_run_then_a_pass_is_satisfied(self):
        # run_check writes 0 for an interrupted run's duration: incomplete, so it concludes nothing (M2T10D-650).
        self.another('unit','u2',exit_code=130,duration_seconds=0,finished_at='2020-01-01T00:00:30+00:00')
        rep=self.report(own(*SELF_TESTS));self.assertEqual((rep['tests'][self.t]['status'],rep['invalid']),('PASS',[]))
    def test_a_timed_out_run_then_a_pass_is_satisfied(self):
        self.another('unit','u2',exit_code=124,timed_out=True);self.assertEqual(self.status(),'PASS')
    def test_a_pass_line_in_another_check_does_not_satisfy(self):
        self.f.edit_test_results('unit',[l for l in self.lines_of('unit') if l['test_id']!=self.t])
        self.f.edit_test_results('static',self.lines_of('static')+[{'schema_version':'1.0','test_id':self.t,'name':'cross','status':'PASS'}])
        self.assertEqual(self.status(),'NOT_RUN')
    def test_a_check_without_members_is_missing(self):
        shutil.rmtree(self.f.root/self.f.dirs['unit']);self.assertEqual(self.status(),'MISSING')
    def test_an_earlier_traceability_failure_makes_the_self_test_flaky(self):
        failing=[{'schema_version':'1.0','test_id':'TEST-052-05','name':'old','status':'FAIL'}]
        self.another('traceability','tr-earlier',lines=failing,exit_code=1)
        rep=self.report(own(*SELF_TESTS));self.assertEqual(rep['tests']['TEST-052-05']['status'],'FLAKY')
    def test_another_check_sees_an_earlier_traceability_run_as_it_is(self):
        # M2T10R-51: only the traceability check's own run takes its TESTs from its self-test lines; any other check
        # (the independent ones' judgement) counts the earlier traceability members as any member.
        w=tr.World(self.f.root,self.f.candidate,self.profile,check_id='independent-review',requirements=self.requirements)
        rep=tr.report(w,tr.members(w),[]);self.assertEqual(rep['tests']['TEST-052-05']['status'],'PASS')
    def test_an_earlier_traceability_pass_does_not_satisfy_this_run(self):
        rep=self.report([]);self.assertEqual(rep['tests']['TEST-052-05']['status'],'NOT_RUN')
        self.assertIn({'id':'TEST-052-05','status':'NOT_RUN'},rep['unsatisfied'])
    def test_an_unreadable_record_holding_the_candidate_is_damaged(self):
        p=self.f.root/'.hoh-dev/evidence/broken/execution.json';p.parent.mkdir(parents=True)
        p.write_text('{"candidate_id": "'+self.f.candidate['candidate_id']+'"')
        rep=self.report(own(*SELF_TESTS))
        self.assertEqual(rep['invalid'],[{'path':'.hoh-dev/evidence/broken/execution.json','check_id':None,'damaged':True,'reasons':['RECORD_INVALID']}])
    def test_a_failure_tampered_later_beside_a_pass_stays_unsatisfied(self):
        rel=self.another('unit','u2',lines=self.failing_lines(),exit_code=101)
        r=load_json(self.f.root/rel);(self.f.root/r['stderr']['path']).write_text('rewritten')
        rep=self.report(own(*SELF_TESTS));self.assertEqual(rep['tests'][self.t]['status'],'INVALID')
        self.assertEqual([i['reasons'] for i in rep['invalid']],[['ERROR_OUTPUT_CHANGED']])
    def test_a_failure_whose_check_id_changed_later_beside_a_pass_stays_unsatisfied(self):
        self.another('unit','u2',lines=self.failing_lines(),exit_code=101,check_id='not-planned')
        rep=self.report(own(*SELF_TESTS));self.assertTrue(rep['invalid'])
        self.assertIn('CHECK_NOT_PLANNED',rep['invalid'][0]['reasons'])
    def test_a_member_of_another_spec_is_invalid(self):
        self.another('unit','u2',spec_sha256='1'*64);self.assertIn('SPEC_MISMATCH',self.report(own(*SELF_TESTS))['invalid'][0]['reasons'])
    def test_a_pass_whose_manifest_was_deleted_is_invalid(self):
        copied=self.f.root/'.hoh-dev/candidate-copy.json';shutil.copy(self.f.root/'.hoh-dev/candidate.json',copied)
        self.another('unit','u2',candidate_manifest=ref_for(copied,self.f.root));copied.unlink()
        self.assertEqual(self.report(own(*SELF_TESTS))['invalid'][0]['reasons'],['MANIFEST_MISMATCH'])
    def test_a_record_of_another_candidate_is_not_read(self):
        self.another('unit','u2',lines=self.failing_lines(),exit_code=101,candidate_id='CAND-'+'3'*64)
        self.assertEqual(self.status(),'PASS')

def clean_env():
    return {k:v for k,v in os.environ.items() if not k.startswith('HOH_')}

class EndToEnd(unittest.TestCase):
    """TEST-052-05: the real traceability.sh through run_check in a fixture ROOT of its own: a git repository, a
    fixture spec whose TESTs map to a `unit` check and to `traceability` itself, and fixture tests. The fixture's own
    self-test is its tests/bundle test, not this one, so nothing recurses."""
    FILES=['tools/run_check.py','tools/candidate.py','tools/common.py','scripts/product/traceability.sh','scripts/product/traceability.py',
           'scripts/product/test_results.py']+[f'contracts/{n}.schema.json' for n in ('execution','test-result','live-session','soak-event','candidate','check-plan')]
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory(prefix='hoh-traceability-');self.addCleanup(self.temp.cleanup);r=self.root=Path(self.temp.name)
        # Outside the fixture repository, whose tree must stay clean for the freeze.
        shims=Path(tempfile.mkdtemp(prefix='hoh-traceability-bin-'));self.addCleanup(shutil.rmtree,shims,True)
        self.path=own_python(shims,os.environ.get('PATH','/usr/bin:/bin'))
        for rel in self.FILES:(r/rel).parent.mkdir(parents=True,exist_ok=True);shutil.copy2(ROOT/rel,r/rel)
        spec={'requirements':[{'id':'REQ-901','release_targets':['v0.1'],'release_class':'required',
                               'acceptance':[{'id':'AC-901-01','test_id':'TEST-901-01'},{'id':'AC-901-02','test_id':'TEST-901-02'}]},
                              {'id':'REQ-902','release_targets':['v0.1'],'release_class':'required','acceptance':[{'id':'AC-902-01','test_id':'TEST-902-01'}]},
                              {'id':'REQ-904','release_targets':['v0.1'],'release_class':'required','acceptance':[{'id':'AC-901-03','test_id':'TEST-901-03'}]},
                              {'id':'REQ-903','release_targets':['v1'],'release_class':'required','acceptance':[{'id':'AC-903-01','test_id':'TEST-903-01'}]}]}
        (r/'contracts/requirements.json').write_text(json.dumps(spec,indent=1))
        (r/'contracts/acceptance-scenarios.json').write_text(json.dumps({'scenarios':[{'id':'TEST-901-01','check_id':'unit'},{'id':'TEST-901-02','check_id':'unit'},
                                                                                     {'id':'TEST-902-01','check_id':'traceability'},{'id':'TEST-903-01','check_id':'unit'},
                                                                                     {'id':'TEST-901-03','check_id':'integration'}]}))
        (r/'fixture-tests').mkdir()
        for name in ('fixture::test_901_01_first','fixture::test_901_02_second'):(r/'fixture-tests'/name.split('::')[1]).write_text(name+'\n')
        (r/'integration-tests').mkdir();(r/'integration-tests/test_901_03_third').write_text('integration::test_901_03_third\n')
        # A fixture `unit` check: each file under fixture-tests/ is a test, failing while .hoh-dev/fixture-fail exists.
        (r/'unit.sh').write_text('set -Eeuo pipefail\nlog="$(mktemp)";trap \'rm -f "$log"\' EXIT;rc=0\n'
                                 'for f in fixture-tests/*;do if [[ -e .hoh-dev/fixture-fail ]];then echo "test $(cat "$f") ... FAILED";rc=101;'
                                 'else echo "test $(cat "$f") ... ok";fi;done>"$log"\ncat "$log"\n'
                                 'python3 scripts/product/test_results.py --log "$log" --out "$HOH_EVIDENCE_DIR/test-results.jsonl"\nexit $rc\n')
        (r/'integ.sh').write_text((r/'unit.sh').read_text().replace('fixture-tests/','integration-tests/'))
        (r/'tests/bundle').mkdir(parents=True)
        (r/'tests/bundle/test_fixture.py').write_text('import unittest\nclass Fixture(unittest.TestCase):\n    def test_902_01_the_fixture_self_test(self):pass\n')
        plan={'schema_version':'1.0','plan_id':'traceability-fixture','description':'Synthetic fixture plan.','checks':{
            'unit':{'mode':'product','argv':['bash','unit.sh'],'artifacts':['test-results.jsonl'],'timeout_seconds':60},
            'integration':{'mode':'product','argv':['bash','integ.sh'],'artifacts':['test-results.jsonl'],'timeout_seconds':60},
            'traceability':{'mode':'product','argv':['bash','scripts/product/traceability.sh','v0.1'],'artifacts':['test-results.jsonl','traceability-report.json'],'timeout_seconds':300}}}
        (r/'plan.json').write_text(json.dumps(plan,indent=1))
        (r/'.gitignore').write_text('.hoh-dev/\n__pycache__/\n')
        self.git('init','-q');self.git('add','.');self.git('commit','-qm','fixture')
        (r/'.hoh-dev').mkdir();(r/'.hoh-dev/environment.json').write_text('{"mode":"SYNTHETIC_TRACEABILITY_FIXTURE"}\n')
        self.runs=0
    def git(self,*args):
        subprocess.run(['git','-c','user.name=t','-c','user.email=t@invalid.example',*args],cwd=self.root,check=True,env=clean_env()|{'GIT_CONFIG_GLOBAL':'/dev/null','GIT_CONFIG_NOSYSTEM':'1'})
    def tool(self,*argv):
        return subprocess.run([sys.executable,*argv],cwd=self.root,capture_output=True,text=True,env=clean_env()|{'PATH':self.path},timeout=300)
    def freeze(self,name):
        out=f'.hoh-dev/candidates/{name}.json'
        r=self.tool('tools/candidate.py','freeze','--spec','contracts/requirements.json','--environment','.hoh-dev/environment.json','--test-plan','plan.json','--out',out)
        self.assertEqual(r.returncode,0,r.stderr);return out
    def check(self,candidate,check):
        self.runs+=1;out=f'.hoh-dev/evidence/{self.runs}-{check}'
        r=self.tool('tools/run_check.py','--check-id',check,'--candidate-file',candidate,'--out',out)
        return r.returncode,self.root/out
    def traceability(self,candidate):
        rc,out=self.check(candidate,'traceability')
        report=load_json(out/'artifacts/traceability-report.json')
        validator=__import__('jsonschema').Draft202012Validator(load_json(ROOT/'contracts/test-result.schema.json'))
        for line in (out/'artifacts/test-results.jsonl').read_text().splitlines():validator.validate(json.loads(line))
        record=load_json(out/'execution.json');self.assertEqual((record['exit_code'],record['missing_artifacts']),(rc,[]))
        return rc,report,out
    def commit(self,message):self.git('add','-A');self.git('commit','-qm',message)
    def test_052_05_traceability_passes_c_and_lists_what_c_prime_lost(self):
        c=self.freeze('c');cid=load_json(self.root/c)['candidate_id']
        self.assertEqual((self.check(c,'unit')[0],self.check(c,'integration')[0]),(0,0))
        rc,report,out=self.traceability(c)
        self.assertEqual(rc,0,(out/'stderr.log').read_text())
        self.assertEqual(report['candidate_id'],cid)
        # Each TEST with its status, its source check and the passing tests that satisfy it (AC-052-05, M2T10R-50).
        self.assertEqual({t:(v['status'],v['check'],v['passing']) for t,v in report['tests'].items()},
                         {'TEST-901-01':('PASS','unit',['fixture::test_901_01_first']),'TEST-901-02':('PASS','unit',['fixture::test_901_02_second']),
                          'TEST-901-03':('PASS','integration',['integration::test_901_03_third']),
                          'TEST-902-01':('PASS','traceability',['test_fixture.Fixture.test_902_01_the_fixture_self_test'])})
        self.assertEqual(report['tests']['TEST-901-03']['members'],['.hoh-dev/evidence/2-integration/execution.json'])
        self.assertEqual((report['unsatisfied'],report['invalid']),([],[]))
        self.assertEqual([json.loads(l)['test_id'] for l in (out/'artifacts/test-results.jsonl').read_text().splitlines()],['TEST-902-01'])
        # C': a test of a v0.1 AC really deleted, frozen anew, its check rerun.
        (self.root/'fixture-tests/test_901_02_second').unlink();self.commit('drop a test')
        c2=self.freeze('c2');cid2=load_json(self.root/c2)['candidate_id'];self.assertNotEqual(cid2,cid)
        self.assertEqual((self.check(c2,'unit')[0],self.check(c2,'integration')[0]),(0,0))
        rc,report,out=self.traceability(c2)
        self.assertNotEqual(rc,0);self.assertEqual(report['candidate_id'],cid2)
        self.assertEqual(report['unsatisfied'],[{'id':'TEST-901-02','status':'NOT_RUN'}])
        self.assertIn('NOT_RUN TEST-901-02',(out/'stderr.log').read_text())
        # Each report stays bound to its own candidate.
        self.assertEqual(load_json(self.root/'.hoh-dev/evidence/3-traceability/artifacts/traceability-report.json')['candidate_id'],cid)
    def test_052_05_only_failures_or_a_failure_then_a_pass_exit_nonzero(self):
        c=self.freeze('c');self.check(c,'integration');(self.root/'.hoh-dev/fixture-fail').write_text('')
        self.assertEqual(self.check(c,'unit')[0],101)
        rc,report,_=self.traceability(c)
        self.assertEqual((rc,report['tests']['TEST-901-01']['status']),(1,'FAIL'))
        (self.root/'.hoh-dev/fixture-fail').unlink();self.assertEqual(self.check(c,'unit')[0],0)
        rc,report,_=self.traceability(c)
        self.assertEqual((rc,report['tests']['TEST-901-01']['status']),(1,'FLAKY'))
    def test_a_missing_self_test_writes_no_line_and_is_not_run(self):
        (self.root/'tests/bundle/test_fixture.py').write_text('import unittest\nclass Fixture(unittest.TestCase):\n    def test_renamed(self):pass\n')
        self.commit('lose the self-test');c=self.freeze('c');self.check(c,'unit');self.check(c,'integration')
        rc,report,out=self.traceability(c)
        self.assertEqual((rc,report['unsatisfied']),(1,[{'id':'TEST-902-01','status':'NOT_RUN'}]))
        self.assertEqual((out/'artifacts/test-results.jsonl').read_text(),'')

if __name__=='__main__':unittest.main()
