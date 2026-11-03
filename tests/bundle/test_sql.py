import json,sqlite3,tempfile,unittest
from pathlib import Path
from helpers import ROOT

class SQLContract(unittest.TestCase):
    def setUp(self):
        self.c=sqlite3.connect(':memory:');self.addCleanup(self.c.close)
        self.c.executescript((ROOT/'contracts/sql/001_initial.sql').read_text())
        self.c.execute("INSERT INTO projects VALUES('p','/tmp/p',0,'now')")
        self.c.execute("INSERT INTO specs VALUES('s','p',?,1)",('a'*64,))
        self.c.execute("INSERT INTO runs(run_id,project_id,spec_id,state,mode,created_at) VALUES('r','p','s','READY','AUTO','now')")
        self.c.commit()
    def test_integrity(self):self.assertEqual(self.c.execute('PRAGMA integrity_check').fetchone()[0],'ok')
    def test_foreign_key(self):
        with self.assertRaises(sqlite3.IntegrityError):self.c.execute("INSERT INTO specs VALUES('bad','missing',?,1)",('a'*64,))
    def test_invalid_state(self):
        with self.assertRaises(sqlite3.IntegrityError):self.c.execute("UPDATE runs SET state='DONE_MAYBE' WHERE run_id='r'")
    def test_revision_cas(self):
        a=self.c.execute("UPDATE runs SET revision=revision+1 WHERE run_id='r' AND revision=0").rowcount
        b=self.c.execute("UPDATE runs SET revision=revision+1 WHERE run_id='r' AND revision=0").rowcount
        self.assertEqual((a,b),(1,0))
    def test_atomic_state_event_rollback(self):
        try:
            with self.c:
                self.c.execute("UPDATE runs SET state='RUNNING' WHERE run_id='r'")
                self.c.execute("INSERT INTO events(project_id,event_id,run_id,kind,payload_json,created_at) VALUES('p','e','r','x','{}','now')")
                raise RuntimeError('fault after event before outbox')
        except RuntimeError:pass
        self.assertEqual(self.c.execute("SELECT state FROM runs").fetchone()[0],'READY')
        self.assertEqual(self.c.execute('SELECT count(*) FROM events').fetchone()[0],0)
    def test_outbox_receipt_deduplication(self):
        self.c.execute("INSERT INTO events(project_id,event_id,run_id,kind,payload_json,created_at) VALUES('p','e','r','x','{}','now')")
        self.c.execute("INSERT INTO outbox(message_id,event_seq,recipient_role,payload_json,state) VALUES('m',1,'qa','{}','PENDING')")
        self.c.execute("INSERT INTO receipts VALUES('qa','m','now','{}')")
        with self.assertRaises(sqlite3.IntegrityError):self.c.execute("INSERT INTO receipts VALUES('qa','m','now','{}')")
    def test_invalid_json(self):
        with self.assertRaises(sqlite3.IntegrityError):self.c.execute("INSERT INTO events(project_id,event_id,run_id,kind,payload_json,created_at) VALUES('p','e','r','x','NOTJSON','now')")
    def test_command_idempotency_key(self):
        args=('c1','r','same','a'*64,0,None);self.c.execute('INSERT INTO commands VALUES(?,?,?,?,?,?)',args)
        with self.assertRaises(sqlite3.IntegrityError):self.c.execute('INSERT INTO commands VALUES(?,?,?,?,?,?)',('c2',)+args[1:])
    def test_single_writer_lease(self):
        self.c.execute("INSERT INTO tasks(task_id,run_id,spec_id,state,role,created_seq) VALUES('t','r','s','READY','developer',1)")
        self.c.execute("INSERT INTO attempts(attempt_id,task_id,state,role_epoch,host_epoch,config_revision) VALUES('a','t','PREPARED',1,1,0)")
        self.c.execute("INSERT INTO leases VALUES('p','writer','a',1,'later')")
        with self.assertRaises(sqlite3.IntegrityError):self.c.execute("INSERT INTO leases VALUES('p','writer','a',2,'later')")
    def test_lease_kinds(self):
        self.c.execute("INSERT INTO tasks(task_id,run_id,spec_id,state,role,created_seq) VALUES('t2','r','s','READY','qa',2)")
        self.c.execute("INSERT INTO attempts(attempt_id,task_id,state,role_epoch,host_epoch,config_revision) VALUES('a2','t2','PREPARED',1,1,0)")
        self.c.execute("INSERT INTO leases VALUES('p','reader-1','a2',1,'later')");self.c.execute("INSERT INTO leases VALUES('p','reader-2','a2',1,'later')")
        with self.assertRaises(sqlite3.IntegrityError):self.c.execute("INSERT INTO leases VALUES('p','reader-x','a2',1,'later')")
    def test_sqlite_online_backup(self):
        self.c.commit();other=sqlite3.connect(':memory:');self.addCleanup(other.close);self.c.backup(other)
        self.assertEqual(other.execute('SELECT run_id FROM runs').fetchone()[0],'r')
    def test_wal_local_database(self):
        with tempfile.TemporaryDirectory() as d:
            c=sqlite3.connect(str(Path(d)/'state.db'))
            try:self.assertEqual(c.execute('PRAGMA journal_mode=WAL').fetchone()[0],'wal')
            finally:c.close()
    def test_schema_version_and_rerun_refused(self):
        self.assertEqual(self.c.execute('PRAGMA user_version').fetchone()[0],1)
        self.assertEqual(self.c.execute('SELECT max(version) FROM schema_migrations').fetchone()[0],1)
        with self.assertRaises(sqlite3.Error):self.c.executescript((ROOT/'contracts/sql/001_initial.sql').read_text())
        self.c.execute('ROLLBACK') if self.c.in_transaction else None
        self.assertEqual(self.c.execute('SELECT count(*) FROM runs').fetchone()[0],1)
    def test_primary_keys_not_null(self):
        with self.assertRaises(sqlite3.IntegrityError):self.c.execute('INSERT INTO task_dependencies VALUES(NULL,NULL)')
    def test_foreign_keys_indexed(self):
        missing=[]
        for (table,) in self.c.execute("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'").fetchall():
            fks={row[3] for row in self.c.execute(f'PRAGMA foreign_key_list({table})') if row[1]==0}  # leading column of each FK
            pk=[r[1] for r in sorted(self.c.execute(f'PRAGMA table_info({table})').fetchall(),key=lambda r:r[5]) if r[5]]
            leading={pk[0]} if pk else set()
            for (idx,*_) in [(r[1],) for r in self.c.execute(f'PRAGMA index_list({table})')]:
                cols=[r[2] for r in self.c.execute(f'PRAGMA index_info({idx})')]
                if cols:leading.add(cols[0])
            missing+=[f'{table}.{c}' for c in fks-leading]
        allowed={'specs.project_id','role_bindings.session_id','requirements.spec_id','acceptance_criteria.requirement_id','iterations.run_id','tasks.iteration_id','tasks.spec_id','prompt_revisions.parent_revision','attempts.prompt_revision','supervisor_triggers.spec_id','change_requests.base_spec_id','change_requests.result_spec_id','handoffs.package_artifact_id','releases.candidate_id','issues.candidate_id','outbox.run_id','acceptance_results.ac_id','runs.spec_id'}
        self.assertEqual(sorted(set(missing)-allowed),[])
    def test_degraded_only_for_evidence_and_results(self):
        self.c.execute("INSERT INTO requirements VALUES('REQ-048','s','plugin',1,'degradable')")
        with self.assertRaises(sqlite3.IntegrityError):self.c.execute("INSERT INTO requirements VALUES('REQ-001','s','x',1,'optional')")
    def test_release_status(self):
        with self.assertRaises(sqlite3.IntegrityError):
            self.c.execute("INSERT INTO candidates VALUES('c','r',?,?,?,?,?,'p','now')",('a'*40,'b'*40,'c'*64,'d'*64,'e'*64))
            self.c.execute("INSERT INTO releases VALUES('rel','r','c','v1','DONE',?, 'now')",('f'*64,))
    def test_git_ids_exact_length(self):
        with self.assertRaises(sqlite3.IntegrityError):
            self.c.execute("INSERT INTO candidates VALUES('c2','r',?,?,?,?,?,'p','now')",('a'*41,'b'*40,'c'*64,'d'*64,'e'*64))
