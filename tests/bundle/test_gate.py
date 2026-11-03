import copy,json,shutil,unittest
from datetime import timedelta
from helpers import ReleaseFixture,load_json,json_file,ref_for,sha256
from release_gate import validate_manifest,GateError
import baseline

PLUGIN_AC='AC-048-01'

class ReleaseGate(unittest.TestCase):
    def setUp(self):self.f=ReleaseFixture();self.addCleanup(self.f.close);self.m=self.f.manifest
    def reject(self,match=None):
        with self.assertRaisesRegex((GateError,ValueError),match or '.'):
            validate_manifest(self.f.root,self.m)
    def first_ac_with(self,check):return next(a for a,c in self.f.ac_check.items() if c==check)
    # Positive envelope
    def test_synthetic_valid_envelope_only(self):
        r=validate_manifest(self.f.root,self.m);total=sum(len(x['acceptance']) for x in load_json(self.f.root/'contracts/requirements.json')['requirements'])
        self.assertEqual((r['status'],r['release_status'],r['required_acceptance']),('PASS','COMPLETED',total))
        self.assertEqual(r['degraded'],{'checks':[],'acceptance':[]})
    # Checks and execution records
    def test_empty_checks(self):self.m['checks']=[];self.reject()
    def test_missing_check(self):self.m['checks'].pop(0);self.reject('missing required')
    def test_duplicate_check(self):self.m['checks'].append(self.m['checks'][0]);self.reject('duplicate')
    def test_unknown_check(self):
        row=copy.deepcopy(self.f.check_row('unit'));row['id']='made-up';self.m['checks'].append(row);self.reject('not a release-policy check')
    def test_skipped_required(self):self.f.check_row('build')['status']='SKIPPED';self.reject('not PASS')
    def test_flaky_required(self):self.f.check_row('build')['status']='FLAKY';self.reject('not PASS')
    def test_unknown_status(self):self.f.check_row('build')['status']='UNKNOWN';self.reject('not PASS')
    def test_mock_not_live(self):self.f.edit_execution('adapter-live',mode='mock');self.reject('needs live')
    def test_product_not_live(self):self.f.edit_execution('adapter-live',mode='product');self.reject('needs live')
    def test_bundle_not_product(self):self.f.edit_execution('unit',mode='bundle');self.reject('needs product')
    def test_command_not_planned(self):self.f.edit_execution('unit',command=['true']);self.reject('differs from the frozen check plan')
    def test_cwd_not_planned(self):self.f.edit_execution('unit',cwd='elsewhere');self.reject('different cwd')
    def test_stale_candidate(self):self.f.edit_execution('unit',candidate_id='CAND-'+'0'*64);self.reject('stale')
    def test_stale_spec(self):self.f.edit_execution('unit',spec_sha256='0'*64);self.reject('stale')
    def test_stale_environment(self):self.f.edit_execution('unit',environment_sha256='0'*64);self.reject('environment')
    def test_check_source_modified(self):self.f.edit_execution('unit',source_unchanged=False);self.reject('source changed')
    def test_live_source_modified(self):(self.f.root/'source.txt').write_text('changed');self.reject('dirty')
    def test_wrong_record_check_id(self):self.f.edit_execution('unit',check_id='wrong');self.reject('binding')
    def test_failed_command(self):self.f.edit_execution('unit',exit_code=1);self.reject('failed')
    def test_timed_out_command(self):self.f.edit_execution('unit',timed_out=True);self.reject('timed out')
    def test_missing_declared_artifact(self):self.f.edit_execution('soak',missing_artifacts=['timeline.jsonl']);self.reject('artifacts missing')
    def test_undeclared_artifact_set(self):self.f.edit_execution('adapter-live',artifacts=[]);self.reject('artifacts differ')
    def test_evidence_outside_execution_dir(self):self.f.edit_execution('unit',stdout=self.f.records['build']['stdout']);self.reject('outside its execution directory')
    def test_changed_stdout(self):
        p=self.f.root/self.f.records['unit']['stdout']['path'];p.write_text('tampered');self.reject('hash mismatch')
    def test_record_hash_tampered(self):self.m['checks'][0]['execution']['sha256']='0'*64;self.reject('hash mismatch')
    def test_path_traversal(self):self.m['checks'][0]['execution']['path']='../escape.json';self.reject('escapes')
    def test_absolute_artifact(self):self.m['checks'][0]['execution']['path']='/tmp/escape.json';self.reject()
    def test_symlink_escape(self):
        p=self.f.root/'.hoh-dev/escape';p.symlink_to('/etc/passwd');self.m['checks'][0]['execution']['path']=str(p.relative_to(self.f.root));self.reject('escapes')
    def test_invalid_duration(self):self.f.edit_execution('unit',duration_seconds=200);self.reject('inconsistent duration')
    def test_reused_record_id(self):self.f.edit_execution('unit',record_id=self.f.records['build']['record_id']);self.reject('reused')
    # Live evidence
    def test_live_sessions_wrong_candidate(self):
        s=load_json(self.f.root/self.f.records['adapter-live']['artifacts'][0]['path']);s['harness_candidate_id']='CAND-'+'1'*64
        self.f.rewrite_artifact('adapter-live','live-sessions.json',json.dumps(s));self.reject('another check/candidate')
    def test_live_sessions_empty(self):
        s=load_json(self.f.root/self.f.records['adapter-live']['artifacts'][0]['path']);s['sessions']=[]
        self.f.rewrite_artifact('adapter-live','live-sessions.json',json.dumps(s));self.reject('live-session')
    # Acceptance criteria
    def test_missing_ac(self):self.m['acceptance'].pop();self.reject('AC list')
    def test_duplicate_ac(self):self.m['acceptance'].append(self.m['acceptance'][0]);self.reject('duplicate')
    def test_failed_ac(self):self.m['acceptance'][0]['status']='FAIL';self.reject('not PASS')
    def test_unknown_ac_check(self):self.m['acceptance'][0]['check_ids']=['nonsense'];self.reject('unknown check')
    def test_ac_must_cite_scenario_check(self):
        ac=self.first_ac_with('tui-pty');self.f.set_ac(ac,check_ids=['unit'],raw_evidence=[self.f.records['unit']['stdout']]);self.reject('scenario requires check tui-pty')
    def test_raw_evidence_must_come_from_cited_check(self):
        ac=self.first_ac_with('tui-pty');self.f.edit_observation(ac,raw_evidence=[self.f.records['build']['stdout']]);self.reject('not output of its cited checks')
    def test_raw_evidence_dotdot_escape(self):
        ac=self.first_ac_with('tui-pty');ref=dict(self.f.records['build']['stdout']);ref['path']=self.f.dirs['tui-pty']+'/../build/stdout.log'
        self.f.edit_observation(ac,raw_evidence=[ref]);self.reject('not output of its cited checks')
    def test_extra_cited_check_cannot_supply_all_evidence(self):
        ac=self.first_ac_with('tui-pty');self.f.set_ac(ac,check_ids=['tui-pty','unit'],raw_evidence=[self.f.records['unit']['stdout']]);self.reject('no raw evidence from its scenario check')
    def test_nested_execution_directories(self):
        rec=copy.deepcopy(self.f.records['unit']);d=self.f.dirs['build']+'/nested-unit'
        for k in ['stdout','stderr']:
            src=self.f.root/rec[k]['path'];dst=self.f.root/d/src.name;dst.parent.mkdir(parents=True,exist_ok=True);shutil.copy(src,dst);rec[k]=ref_for(dst,self.f.root)
        self.f.check_row('unit')['execution']=json_file(self.f.root,d+'/execution.json',rec);self.reject('overlaps')
    def test_stale_observation(self):self.f.edit_observation(spec_sha256='0'*64);self.reject('stale observation')
    def test_wrong_test_binding(self):self.f.edit_observation(test_id='TEST-999-01');self.reject('AC/test')
    def test_failed_observation(self):self.f.edit_observation(status='FAIL');self.reject('inconsistent observation')
    def test_empty_observation(self):self.f.edit_observation(raw_evidence=[]);self.reject()
    # Degradation (CR-001)
    def plugin_acs(self):return sorted(a for a,c in self.f.ac_check.items() if c=='plugin-live')
    def degrade_plugins(self):
        self.f.degrade_check('plugin-live');self.f.edit_execution('plugin-live',exit_code=3)
        for ac in self.plugin_acs():self.f.degrade_ac(ac,'plugin-live')
    def test_degraded_plugins_report_completed_degraded(self):
        self.degrade_plugins();r=validate_manifest(self.f.root,self.m)
        self.assertEqual(r['release_status'],'COMPLETED_DEGRADED');self.assertEqual(r['degraded'],{'checks':['plugin-live'],'acceptance':self.plugin_acs()})
    def test_only_degradable_requirements_use_plugin_check(self):
        self.assertEqual({a[:6] for a in self.plugin_acs()},{'AC-048','AC-049','AC-050'})
    def test_degraded_ac_needs_details(self):self.f.degrade_check('plugin-live');self.f.set_ac(PLUGIN_AC,status='DEGRADED');self.reject('degradation')
    def test_degraded_check_needs_details(self):self.f.check_row('plugin-live')['status']='DEGRADED';self.reject('degradation')
    def test_ac_cannot_pass_on_degraded_check(self):
        self.f.degrade_check('plugin-live');self.f.edit_execution('plugin-live',exit_code=3);self.reject('cannot PASS on degraded')
    def test_ac_cannot_degrade_when_check_passed(self):self.f.degrade_ac(PLUGIN_AC,'plugin-live');self.reject('although its scenario check')
    def test_required_ac_cannot_degrade(self):
        ac=self.first_ac_with('unit');self.f.degrade_ac(ac,'unit');self.reject('not PASS')
    def test_required_check_cannot_degrade(self):self.f.degrade_check('unit');self.reject('not PASS')
    def test_degradation_diagnostic_from_own_check(self):
        self.degrade_plugins();self.f.check_row('plugin-live')['degradation']['diagnostic']=[self.f.records['unit']['stdout']];self.reject('not output of this check')
    # Independent reports
    def test_missing_qa(self):self.m['independent_reports'].pop(0);self.reject()
    def test_same_developer_qa(self):self.m['independent_reports'][0]['session_id']=self.m['developer_session_id'];self.reject('distinct')
    def test_same_qa_reviewer(self):self.m['independent_reports'][1]['session_id']=self.m['independent_reports'][0]['session_id'];self.reject('distinct')
    def test_failed_review(self):self.m['independent_reports'][1]['status']='FAIL';self.reject('failed')
    def test_report_session_not_observed(self):self.f.edit_attempt('qa',session_ids=['someone-else']);self.reject('not observed')
    def test_report_attempt_other_role(self):self.f.edit_attempt('reviewer',role='developer');self.reject('another role')
    def test_report_attempt_incomplete(self):self.f.edit_attempt('qa',status='NATIVE_FAILED');self.reject('did not complete')
    def test_report_attempt_reuses_developer(self):self.f.edit_attempt('qa',session_ids=['qa-independent-fixture','developer-fixture']);self.reject('reused the developer')
    def test_report_attempt_other_candidate(self):self.f.edit_attempt('qa',candidate_id='CAND-'+'3'*64);self.reject('another candidate')
    def test_report_raw_session_must_be_attempt_output(self):self.f.edit_attempt('reviewer',stdout_sha256='0'*64);self.reject('raw session is not the native attempt output')
    def test_report_attempt_malformed(self):self.f.edit_attempt('qa',session_ids='qa-independent-fixture');self.reject('native-attempt')
    def test_report_session_listed_in_check_live_sessions(self):
        s=load_json(self.f.root/self.f.records['independent-qa']['artifacts'][0]['path']);s['sessions'][0]['session_id']='someone-else'
        self.f.rewrite_artifact('independent-qa','live-sessions.json',json.dumps(s));self.reject('not listed in independent-qa live sessions')
    def test_report_material_from_its_check(self):
        row=next(x for x in self.m['independent_reports'] if x['role']=='qa');report=load_json(self.f.root/row['artifact']['path'])
        row['artifact']=json_file(self.f.root,'.hoh-dev/handwritten-qa-report.json',report);self.reject('not the independent-report.json produced by the independent-qa check')
    def test_report_blocking_finding(self):
        self.f.edit_report('reviewer',findings=[{'id':'F1','severity':'critical','summary':'data loss on restart'}]);self.reject('blocking findings')
    def test_root_evidence_rejected(self):self.f.edit_execution('unit',runner={'tool':'tools/run_check.py','euid':0});self.reject('non-root')
    def test_handwritten_runner_rejected(self):self.f.edit_execution('unit',runner={'tool':'my-script','euid':1000});self.reject('run_check/hoh')
    def test_critical_issue(self):self.m['open_issues']=[{'id':'i','severity':'critical','description':'broken'}];self.reject('blocking issues')
    def test_high_issue(self):self.m['open_issues']=[{'id':'i','severity':'high','description':'broken'}];self.reject('blocking issues')
    # Soak
    def test_short_soak(self):self.f.edit_execution('soak',duration_seconds=1,finished_at='2020-01-01T00:00:01+00:00');self.reject('soak duration')
    def test_wrong_soak_count(self):self.m['soak']['live_iterations']=100;self.reject('count')
    def test_soak_timeline_must_come_from_soak_check(self):
        other=json_file(self.f.root,'.hoh-dev/handwritten-timeline.jsonl',{});self.m['soak']['timeline']=other;self.reject('not the one produced')
    def test_soak_event_other_candidate(self):
        p=self.f.root/self.m['soak']['timeline']['path'];lines=p.read_text().splitlines();e=json.loads(lines[0]);e['harness_candidate_id']='CAND-'+'2'*64
        self.f.rewrite_artifact('soak','timeline.jsonl','\n'.join([json.dumps(e)]+lines[1:])+'\n');self.reject('different harness candidate')
    def test_soak_events_out_of_order(self):
        p=self.f.root/self.m['soak']['timeline']['path'];lines=p.read_text().splitlines()
        self.f.rewrite_artifact('soak','timeline.jsonl','\n'.join(lines[::-1])+'\n');self.reject('out of order')
    def test_soak_event_schema(self):
        p=self.f.root/self.m['soak']['timeline']['path'];lines=p.read_text().splitlines();e=json.loads(lines[0]);del e['iteration_id']
        self.f.rewrite_artifact('soak','timeline.jsonl','\n'.join([json.dumps(e)]+lines[1:])+'\n');self.reject('iteration_id')
    def test_soak_required_by_v1(self):del self.m['soak'];self.reject('soak evidence required')
    def test_soak_wall_clock_short(self):
        short=self.f.start+timedelta(seconds=self.f.policy['minimum_soak_seconds']-2000)
        self.f.edit_execution('soak',finished_at=short.isoformat());self.reject('soak duration')
    # Baseline and identity
    def test_empty_artifact(self):
        self.f.rewrite_artifact('build','hoh-release.tar.gz','');art=next(a for a in self.f.records['build']['artifacts'] if a['name']=='hoh-release.tar.gz')
        self.m['release_artifact']={'path':art['path'],'sha256':art['sha256']};self.reject('empty artifact')
    def test_baseline_changed(self):
        p=self.f.root/'contracts/release-policy.json';d=load_json(p);d['minimum_soak_seconds']=0;p.write_text(json.dumps(d));self.reject('baseline contract changed')
    def test_gate_tool_changed(self):
        p=self.f.root/'tools/release_gate.py';p.write_text(p.read_text()+'\n# weakened\n');self.reject('baseline contract changed')
    def test_change_request_tampered(self):
        lock=load_json(self.f.root/'contracts/baseline-lock.json');p=self.f.root/lock['change_requests'][0]['path'];p.write_text(p.read_text()+'x');self.reject('change request record')
    def test_wrong_baseline_id(self):self.m['baseline_id']='HOH-BASELINE-9.9';self.reject('wrong baseline')
    def test_change_request_not_approved(self):
        lockp=self.f.root/'contracts/baseline-lock.json';lock=load_json(lockp);cr=lock['change_requests'][0];p=self.f.root/cr['path']
        p.write_text(p.read_text().replace('状态：APPROVED','状态：DRAFT'));cr['sha256']=sha256(p);lockp.write_text(json.dumps(lock));self.reject('not APPROVED')
    def test_committed_change_request_cannot_be_amended(self):
        lock=load_json(self.f.root/'contracts/baseline-lock.json');cr=self.f.root/lock['change_requests'][-1]['path']
        with self.assertRaisesRegex(ValueError,'already committed'):baseline.relock(self.f.root,cr,None,amend=True)
    def test_relock_add_keeps_change_request_identity(self):
        # Regression (CR-003 review): --add must not replace the CR path recorded in the lock.
        cr=self.f.root/'docs/cr/CR-099-test.md';cr.parent.mkdir(parents=True,exist_ok=True)
        cr.write_text('# test\n\nID：CR-099\n状态：APPROVED\n批准人：bundle test\n')
        extra=self.f.root/'contracts/extra.schema.json';extra.write_text('{}\n')
        lock=baseline.relock(self.f.root,cr,None,add=['contracts/extra.schema.json'])
        self.assertEqual(lock['change_requests'][-1],{'id':'CR-099','path':'docs/cr/CR-099-test.md','sha256':sha256(cr)})
        self.assertIn('contracts/extra.schema.json',lock['files']);baseline.verify_baseline(self.f.root)
    def test_relock_add_requires_existing_file(self):
        cr=self.f.root/'docs/cr/CR-098-test.md';cr.parent.mkdir(parents=True,exist_ok=True);cr.write_text('ID：CR-098\n状态：APPROVED\n批准人：bundle test\n')
        with self.assertRaisesRegex(ValueError,'not a file'):baseline.relock(self.f.root,cr,None,add=['contracts/missing.json'])
    def test_ac_needs_its_own_test_result(self):
        ac=self.m['acceptance'][0]['id'];key=self.f.ac_check[ac];tid='TEST-'+ac[3:]
        lines=[l for l in map(json.loads,(self.f.root/next(a for a in self.f.records[key]['artifacts'] if a['name']=='test-results.jsonl')['path']).read_text().splitlines()) if l['test_id']!=tid]
        self.f.edit_test_results(key,lines);self.reject('no test result for '+tid)
    def test_ac_test_result_must_pass(self):
        ac=self.m['acceptance'][0]['id'];key=self.f.ac_check[ac];tid='TEST-'+ac[3:]
        path=self.f.root/next(a for a in self.f.records[key]['artifacts'] if a['name']=='test-results.jsonl')['path']
        lines=list(map(json.loads,path.read_text().splitlines()))+[{'schema_version':'1.0','test_id':tid,'name':'test_flaky','status':'FAIL'}]
        self.f.edit_test_results(key,lines);self.reject('not PASS')
    def test_test_results_schema(self):
        key=self.f.ac_check[self.m['acceptance'][0]['id']]
        self.f.edit_test_results(key,[{'test_id':'AC-001-01','status':'ok'}]);self.reject('line 1')
    def test_scenario_check_without_test_results(self):
        key=self.f.ac_check[self.m['acceptance'][0]['id']];rec=self.f.records[key]
        self.f.edit_execution(key,artifacts=[a for a in rec['artifacts'] if a['name']!='test-results.jsonl']);self.reject('artifacts differ')
    def test_live_check_needs_required_agents(self):
        art=next(a for a in self.f.records['adapter-live']['artifacts'] if a['name']=='live-sessions.json');s=load_json(self.f.root/art['path'])
        s['sessions']=[x for x in s['sessions'] if x['agent']!='pi'];self.f.rewrite_artifact('adapter-live','live-sessions.json',json.dumps(s));self.reject('lack required agents: pi')
    def test_plugin_sessions_must_list_plugins(self):
        art=next(a for a in self.f.records['plugin-live']['artifacts'] if a['name']=='live-sessions.json');s=load_json(self.f.root/art['path'])
        for x in s['sessions']:x.pop('plugins',None)
        self.f.rewrite_artifact('plugin-live','live-sessions.json',json.dumps(s));self.reject('plugins it loaded')
    def test_report_must_be_declared_artifact(self):
        row=self.m['independent_reports'][0];other=self.f.root/self.f.dirs['independent-qa']/'artifacts/other-report.json'
        other.write_text((self.f.root/row['artifact']['path']).read_text());row['artifact']=ref_for(other,self.f.root);self.reject('is not the independent-report.json')
    def test_report_edited_after_check(self):
        row=self.m['independent_reports'][1];p=self.f.root/row['artifact']['path'];r=load_json(p);r['findings']=[]
        p.write_text(json.dumps(r)+' ');row['artifact']=ref_for(p,self.f.root);self.reject('hash mismatch')
    def test_release_artifact_must_be_build_bundle(self):
        p=self.f.root/'.hoh-dev/other.bin';p.write_bytes(b'x');self.m['release_artifact']=ref_for(p,self.f.root);self.reject('hoh-release.tar.gz produced by the build check')
    def test_unknown_profile(self):self.m['profile']='v9';self.reject('unknown release profile')

class InterimRelease(unittest.TestCase):
    def setUp(self):self.f=ReleaseFixture(profile='v0.1');self.addCleanup(self.f.close)
    def test_interim_is_never_completed(self):
        r=validate_manifest(self.f.root,self.f.manifest);self.assertEqual(r['release_status'],'INTERIM_RELEASED');self.assertEqual(r['profile'],'v0.1')
        reqs=load_json(self.f.root/'contracts/requirements.json')['requirements']
        self.assertEqual(r['required_acceptance'],sum(len(x['acceptance']) for x in reqs if 'v0.1' in x['release_targets']))
    def test_interim_needs_its_checks(self):
        self.f.manifest['checks']=[c for c in self.f.manifest['checks'] if c['id']!='adapter-live']
        with self.assertRaisesRegex(GateError,'missing required'):validate_manifest(self.f.root,self.f.manifest)
    def test_interim_ac_scope_exact(self):
        self.f.manifest['acceptance'].pop()
        with self.assertRaisesRegex(GateError,'AC list'):validate_manifest(self.f.root,self.f.manifest)

class PlanBinding(unittest.TestCase):
    def test_plan_cannot_weaken_live_mode(self):
        f=ReleaseFixture(plan_overrides={'adapter-live':{'mode':'mock','artifacts':[]}});self.addCleanup(f.close)
        with self.assertRaisesRegex(GateError,'policy needs live'):validate_manifest(f.root,f.manifest)
    def test_live_check_must_declare_sessions(self):
        f=ReleaseFixture(plan_overrides={'adapter-live':{'artifacts':['test-results.jsonl']}});self.addCleanup(f.close)
        with self.assertRaisesRegex(GateError,'lacks live-sessions.json'):validate_manifest(f.root,f.manifest)
