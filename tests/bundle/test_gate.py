import copy,json,unittest
from helpers import ReleaseFixture,load_json,json_file
from release_gate import validate_manifest,GateError

class ReleaseGate(unittest.TestCase):
    def setUp(self):self.f=ReleaseFixture();self.addCleanup(self.f.close);self.m=self.f.manifest
    def reject(self,match=None):
        with self.assertRaisesRegex((GateError,ValueError),match or '.'):
            validate_manifest(self.f.root,self.m)
    def test_synthetic_valid_envelope_only(self):
        r=validate_manifest(self.f.root,self.m);self.assertEqual(r['required_acceptance'],70);self.assertEqual(r['status'],'PASS')
    def test_empty_checks(self):self.m['checks']=[];self.reject()
    def test_missing_check(self):self.m['checks'].pop(0);self.reject('missing required')
    def test_duplicate_check(self):self.m['checks'].append(self.m['checks'][0]);self.reject('duplicate')
    def test_skipped_required(self):self.m['checks'][0]['status']='SKIPPED';self.reject('not PASS')
    def test_flaky_required(self):self.m['checks'][0]['status']='FLAKY';self.reject('not PASS')
    def test_mock_not_live(self):self.f.edit_execution('adapter-live',mode='mock');self.reject('mock/bundle')
    def test_product_not_live(self):self.f.edit_execution('adapter-live',mode='product');self.reject('needs live')
    def test_bundle_not_product(self):self.f.edit_execution('unit',mode='bundle');self.reject('mock/bundle')
    def test_stale_candidate(self):self.f.edit_execution('unit',candidate_id='stale');self.reject('stale')
    def test_stale_spec(self):self.f.edit_execution('unit',spec_sha256='0'*64);self.reject('stale')
    def test_stale_environment(self):self.f.edit_execution('unit',environment_sha256='0'*64);self.reject('environment')
    def test_check_source_modified(self):self.f.edit_execution('unit',source_unchanged=False);self.reject('source changed')
    def test_live_source_modified(self):(self.f.root/'source.txt').write_text('changed');self.reject('dirty')
    def test_wrong_record_check_id(self):self.f.edit_execution('unit',check_id='wrong');self.reject('binding')
    def test_failed_command(self):self.f.edit_execution('unit',exit_code=1);self.reject('failed')
    def test_timed_out_command(self):self.f.edit_execution('unit',timed_out=True);self.reject('failed')
    def test_changed_stdout(self):
        p=self.f.root/self.f.records['unit']['stdout']['path'];p.write_text('tampered');self.reject('hash mismatch')
    def test_record_hash_tampered(self):
        self.m['checks'][0]['execution']['sha256']='0'*64;self.reject('hash mismatch')
    def test_path_traversal(self):self.m['checks'][0]['execution']['path']='../escape.json';self.reject('escapes')
    def test_absolute_artifact(self):self.m['checks'][0]['execution']['path']='/tmp/escape.json';self.reject('relative')
    def test_missing_ac(self):self.m['acceptance'].pop();self.reject('AC list')
    def test_duplicate_ac(self):self.m['acceptance'].append(self.m['acceptance'][0]);self.reject('duplicate')
    def test_failed_ac(self):self.m['acceptance'][0]['status']='FAIL';self.reject('not PASS')
    def test_unknown_check(self):self.m['acceptance'][0]['check_ids']=['nonsense'];self.reject('unknown')
    def test_stale_observation(self):self.f.edit_observation(spec_sha256='0'*64);self.reject('stale observation')
    def test_wrong_test_binding(self):self.f.edit_observation(test_id='TEST-999');self.reject('AC/test')
    def test_failed_observation(self):self.f.edit_observation(status='FAIL');self.reject('inconsistent observation')
    def test_empty_observation(self):self.f.edit_observation(raw_evidence=[]);self.reject()
    def test_missing_qa(self):self.m['independent_reports'].pop(0);self.reject()
    def test_same_developer_qa(self):self.m['independent_reports'][0]['session_id']=self.m['developer_session_id'];self.reject('distinct')
    def test_same_qa_reviewer(self):self.m['independent_reports'][1]['session_id']=self.m['independent_reports'][0]['session_id'];self.reject('distinct')
    def test_failed_review(self):self.m['independent_reports'][1]['status']='FAIL';self.reject('failed')
    def test_critical_issue(self):self.m['open_issues']=[{'id':'i','severity':'critical','description':'broken'}];self.reject('blocking issues')
    def test_high_issue(self):self.m['open_issues']=[{'id':'i','severity':'high','description':'broken'}];self.reject('blocking issues')
    def test_short_soak(self):
        e=self.f.records['soak'];self.f.edit_execution('soak',duration_seconds=1,finished_at='2020-01-01T00:00:01+00:00');self.reject('soak duration')
    def test_invalid_duration(self):self.f.edit_execution('unit',duration_seconds=200);self.reject('inconsistent duration')
    def test_wrong_soak_count(self):self.m['soak']['live_iterations']=100;self.reject('count')
    def test_reused_record_id(self):self.f.edit_execution('unit',record_id=self.f.records['build']['record_id']);self.reject('reused')
    def test_empty_artifact(self):
        p=self.f.root/self.m['release_artifact']['path'];p.write_bytes(b'');self.m['release_artifact']=json_file(self.f.root,'.hoh-dev/unrelated.json',{})
        # A truly zero-byte file with a matching digest must still fail.
        from common import ref_for
        self.m['release_artifact']=ref_for(p,self.f.root);self.reject('empty artifact')
    def test_baseline_changed(self):
        p=self.f.root/'contracts/release-policy.json';d=load_json(p);d['minimum_soak_seconds']=0;p.write_text(json.dumps(d));self.reject('baseline contract changed')
    def test_symlink_escape(self):
        p=self.f.root/'.hoh-dev/escape';p.symlink_to('/etc/passwd');self.m['checks'][0]['execution']['path']=str(p.relative_to(self.f.root));self.reject('escapes')
