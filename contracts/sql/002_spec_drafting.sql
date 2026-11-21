-- Migration 2 (CR-026, C22 (a)): before a project's first approved specification revision, its drafting Task has no
-- specification (spec_id NULL); every other Task keeps one. tasks gains kind: 'spec_draft' for the Runtime's drafting
-- Tasks, 'work' for every other Task and every existing row. A NOT NULL cannot be dropped in place, so tasks is rebuilt
-- by the documented table-rebuild steps; the migration runner runs every migration with foreign_keys off and commits
-- it only if PRAGMA foreign_key_check returns no row (docs/cr/CR-026-m3-detailed-spec.md, section 2).
BEGIN;
CREATE TABLE tasks_new(task_id TEXT PRIMARY KEY, run_id TEXT NOT NULL REFERENCES runs(run_id), iteration_id TEXT REFERENCES iterations(iteration_id), spec_id TEXT REFERENCES specs(spec_id), state TEXT NOT NULL CHECK(state IN ('PENDING','READY','LEASED','RUNNING','IMPLEMENTED','VERIFYING','ACCEPTED','BLOCKED','CANCELLED')), revision INTEGER NOT NULL DEFAULT 0 CHECK(revision>=0), role TEXT NOT NULL, writer INTEGER NOT NULL DEFAULT 0 CHECK(writer IN (0,1)), created_seq INTEGER NOT NULL, user_value INTEGER NOT NULL DEFAULT 0 CHECK(user_value BETWEEN 0 AND 5), unblock_value INTEGER NOT NULL DEFAULT 0 CHECK(unblock_value BETWEEN 0 AND 5), risk_reduction INTEGER NOT NULL DEFAULT 0 CHECK(risk_reduction BETWEEN 0 AND 5), information_gain INTEGER NOT NULL DEFAULT 0 CHECK(information_gain BETWEEN 0 AND 5), cost_estimate INTEGER NOT NULL DEFAULT 3 CHECK(cost_estimate BETWEEN 1 AND 5), layer INTEGER NOT NULL DEFAULT 2 CHECK(layer IN (0,1,2)), wait_rounds INTEGER NOT NULL DEFAULT 0 CHECK(wait_rounds>=0), first_eligible_seq INTEGER, attempt_count INTEGER NOT NULL DEFAULT 0 CHECK(attempt_count>=0), next_retry_at TEXT, blocked_reason TEXT, kind TEXT NOT NULL DEFAULT 'work' CHECK(kind IN ('work','spec_draft')), CHECK(spec_id IS NOT NULL OR kind='spec_draft'), CHECK(kind<>'spec_draft' OR writer=1));
INSERT INTO tasks_new(task_id, run_id, iteration_id, spec_id, state, revision, role, writer, created_seq, user_value, unblock_value, risk_reduction, information_gain, cost_estimate, layer, wait_rounds, first_eligible_seq, attempt_count, next_retry_at, blocked_reason, kind) SELECT task_id, run_id, iteration_id, spec_id, state, revision, role, writer, created_seq, user_value, unblock_value, risk_reduction, information_gain, cost_estimate, layer, wait_rounds, first_eligible_seq, attempt_count, next_retry_at, blocked_reason, 'work' FROM tasks;
DROP TABLE tasks;
ALTER TABLE tasks_new RENAME TO tasks;
CREATE INDEX tasks_run_state ON tasks(run_id,state,created_seq);
CREATE INDEX tasks_retry ON tasks(next_retry_at) WHERE next_retry_at IS NOT NULL;
-- Only a drafting Task may lack a specification, and a new one only while its project has no approved revision; one
-- that had none keeps none after the first approval (its history).
CREATE TRIGGER tasks_spec_draft_insert BEFORE INSERT ON tasks WHEN NEW.spec_id IS NULL AND EXISTS(SELECT 1 FROM specs s JOIN runs r ON r.project_id=s.project_id WHERE r.run_id=NEW.run_id) BEGIN SELECT RAISE(ABORT,'SPEC_DRAFT: the project has an approved specification'); END;
-- A Task keeps its kind and whether it has a specification; a non-NULL spec_id may move to a newer revision (CR-018).
CREATE TRIGGER tasks_spec_draft_update BEFORE UPDATE OF spec_id, kind ON tasks WHEN NEW.kind IS NOT OLD.kind OR ((OLD.spec_id IS NULL) <> (NEW.spec_id IS NULL)) BEGIN SELECT RAISE(ABORT,'SPEC_DRAFT: a task keeps its kind and whether it has a specification'); END;
INSERT INTO schema_migrations VALUES(2,'2026-11-17T00:00:00Z');
PRAGMA user_version = 2;
COMMIT;
