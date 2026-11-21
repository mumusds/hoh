"""M2R-001 / CR-029: synthetic envelopes, never live acceptance evidence."""
import unittest

from helpers import ReleaseFixture
from release_gate import GateError, validate_manifest


class NativeSessionIndependence(unittest.TestCase):
    def setUp(self):
        self.fixture = ReleaseFixture(profile='v0.1')
        self.addCleanup(self.fixture.close)
        self.manifest = self.fixture.manifest

    def reject_overlap(self, first, shared='shared-native-fixture'):
        for role in ['qa', 'reviewer']:
            ids = self.fixture.attempts[role]['session_ids']
            self.fixture.edit_attempt(role, session_ids=list(dict.fromkeys(ids + [shared])))
        self.manifest['independent_reports'].sort(key=lambda r: r['role'] != first)
        try:
            result = validate_manifest(self.fixture.root, self.manifest)
        except GateError as error:
            self.assertIn('QA/reviewer native sessions must be distinct', str(error))
        else:
            self.fail(f'shared native session accepted by the synthetic gate: {result}')

    def test_054_01_native_overlap_qa_then_reviewer(self):
        self.reject_overlap('qa')

    def test_054_01_native_overlap_reviewer_then_qa(self):
        self.reject_overlap('reviewer')

    def test_054_01_native_overlap_with_other_reports_primary_session(self):
        self.reject_overlap('reviewer', 'qa-independent-fixture')

    def test_054_01_reviewer_attempt_must_exclude_developer(self):
        self.fixture.edit_attempt('reviewer', session_ids=[
            'reviewer-independent-fixture', self.manifest['developer_session_id']])
        with self.assertRaisesRegex(GateError, 'reused the developer session'):
            validate_manifest(self.fixture.root, self.manifest)

    def test_054_01_disjoint_native_sets_allow_independent_reports(self):
        for role in ['qa', 'reviewer']:
            self.fixture.edit_attempt(role, session_ids=[role + '-independent-fixture', role + '-native-fixture'])
        result = validate_manifest(self.fixture.root, self.manifest)
        self.assertEqual((result['status'], result['release_status']), ('PASS', 'INTERIM_RELEASED'))
