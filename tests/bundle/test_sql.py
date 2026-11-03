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
        self.c.execute("INSERT INTO attempts(attempt_id,task_id,state,role_epoch,host_epoch) VALUES('a','t','PREPARED',1,1)")
        self.c.execute("INSERT INTO leases VALUES('p','writer','a',1,'later')")
        with self.assertRaises(sqlite3.IntegrityError):self.c.execute("INSERT INTO leases VALUES('p','writer','a',2,'later')")
    def test_sqlite_online_backup(self):
        self.c.commit();other=sqlite3.connect(':memory:');self.addCleanup(other.close);self.c.backup(other)
        self.assertEqual(other.execute('SELECT run_id FROM runs').fetchone()[0],'r')
    def test_wal_local_database(self):
        with tempfile.TemporaryDirectory() as d:
            c=sqlite3.connect(str(Path(d)/'state.db'))
            try:self.assertEqual(c.execute('PRAGMA journal_mode=WAL').fetchone()[0],'wal')
            finally:c.close()
