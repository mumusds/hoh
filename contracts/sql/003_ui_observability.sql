-- Migration 3 (CR-028): rebuildable UI identities, public segments and object lifecycle metadata.
-- M4-T09 wires this into Persistence's migration runner; M4-T03 consumes the queries and GC metadata.
-- Only empty structures are created here. No authoritative event, evidence or acceptance row is changed.
-- Backfill runs after reconcile, in writer batches <=500 rows/1 MiB, yielding to urgent controls.
-- Index generation switches only after a complete build to H; ordinary restarts keep the generation.
BEGIN;
CREATE TABLE ui_index_generations(
    generation INTEGER PRIMARY KEY CHECK(generation>0),
    indexed_through INTEGER NOT NULL DEFAULT 0 CHECK(indexed_through>=0),
    target_watermark INTEGER NOT NULL CHECK(target_watermark>=indexed_through),
    index_ready INTEGER NOT NULL DEFAULT 0 CHECK(index_ready IN (0,1)),
    active INTEGER NOT NULL DEFAULT 0 CHECK(active IN (0,1)),
    CHECK(active=0 OR index_ready=1)
);
CREATE UNIQUE INDEX ui_one_active_generation ON ui_index_generations(active) WHERE active=1;
CREATE TABLE ui_iteration_ranges(
    generation INTEGER NOT NULL REFERENCES ui_index_generations(generation),
    run_id TEXT NOT NULL REFERENCES runs(run_id),
    number INTEGER NOT NULL CHECK(number>0),
    iteration_key TEXT NOT NULL,
    start_seq INTEGER NOT NULL CHECK(start_seq>=0),
    close_seq INTEGER REFERENCES events(seq),
    PRIMARY KEY(generation,run_id,number),
    UNIQUE(generation,iteration_key),
    CHECK(close_seq IS NULL OR close_seq>start_seq)
);
-- Membership of terminal/INCOMPLETE tails is resolved at the query's H from immutable run facts.
-- The range is the possible interval, not a rewrite of historical events when a Run resumes.
CREATE TABLE ui_event_index(
    generation INTEGER NOT NULL REFERENCES ui_index_generations(generation),
    seq INTEGER NOT NULL REFERENCES events(seq),
    run_id TEXT REFERENCES runs(run_id),
    iteration_key TEXT,
    attempt_id TEXT REFERENCES attempts(attempt_id),
    role TEXT,
    role_epoch INTEGER CHECK(role_epoch IS NULL OR role_epoch>0),
    session_id TEXT REFERENCES sessions(session_id),
    PRIMARY KEY(generation,seq),
    FOREIGN KEY(generation,iteration_key) REFERENCES ui_iteration_ranges(generation,iteration_key),
    CHECK(iteration_key IS NULL OR run_id IS NOT NULL)
);
CREATE INDEX ui_events_run_seq ON ui_event_index(run_id,seq,generation);
CREATE INDEX ui_events_iteration_seq ON ui_event_index(iteration_key,seq,generation);
CREATE INDEX ui_events_attempt_seq ON ui_event_index(attempt_id,seq,generation);
CREATE INDEX ui_events_role_seq ON ui_event_index(role,seq,generation);
CREATE INDEX ui_events_session_seq ON ui_event_index(session_id,seq,generation);
-- An accepted user's segment may precede an Attempt. input_id binds it to later delivery.
CREATE TABLE public_segments(
    artifact_id TEXT PRIMARY KEY REFERENCES artifacts(artifact_id),
    attempt_id TEXT REFERENCES attempts(attempt_id),
    input_id TEXT REFERENCES input_queue(input_id),
    block_id TEXT NOT NULL,
    stream TEXT NOT NULL,
    first_offset INTEGER NOT NULL CHECK(first_offset>=0),
    bytes INTEGER NOT NULL CHECK(bytes>=0 AND bytes<=1048576),
    sha256 TEXT NOT NULL CHECK(length(sha256)=64),
    sealed INTEGER NOT NULL CHECK(sealed=1),
    created_seq INTEGER NOT NULL REFERENCES events(seq),
    CHECK(attempt_id IS NOT NULL OR input_id IS NOT NULL),
    UNIQUE(block_id,stream,first_offset)
);
CREATE TABLE public_native_parts(
    attempt_id TEXT NOT NULL REFERENCES attempts(attempt_id),
    native_stream TEXT NOT NULL,
    native_item_id TEXT NOT NULL,
    part INTEGER NOT NULL CHECK(part>=0),
    block_id TEXT NOT NULL,
    received_offset INTEGER NOT NULL CHECK(received_offset>=0),
    created_seq INTEGER NOT NULL REFERENCES events(seq),
    PRIMARY KEY(attempt_id,native_stream,native_item_id,part)
);
CREATE TABLE public_input_blocks(
    input_id TEXT PRIMARY KEY REFERENCES input_queue(input_id),
    block_id TEXT NOT NULL UNIQUE,
    created_seq INTEGER NOT NULL REFERENCES events(seq)
);
-- References are a derived closure of authoritative roots, never an independent acceptance truth.
-- Hash-only resource pins, prompt/context/replay references can exist without an artifacts row.
CREATE TABLE ui_object_references(
    owner_kind TEXT NOT NULL,
    owner_id TEXT NOT NULL,
    object_sha256 TEXT NOT NULL CHECK(length(object_sha256)=64),
    artifact_id TEXT REFERENCES artifacts(artifact_id),
    PRIMARY KEY(owner_kind,owner_id,object_sha256)
);
CREATE INDEX ui_object_reference_artifact ON ui_object_references(owner_kind,owner_id,artifact_id);
CREATE INDEX ui_object_reference_hash ON ui_object_references(object_sha256);
CREATE TABLE ui_object_metadata(
    object_sha256 TEXT PRIMARY KEY CHECK(length(object_sha256)=64),
    bytes INTEGER NOT NULL CHECK(bytes>=0),
    text_kind TEXT NOT NULL CHECK(text_kind IN ('text','binary')),
    validated_at TEXT NOT NULL
);
CREATE TABLE ui_object_tombstones(
    object_sha256 TEXT PRIMARY KEY CHECK(length(object_sha256)=64),
    collected_at TEXT NOT NULL,
    reason TEXT NOT NULL CHECK(reason='unreferenced_after_grace')
);
-- Reader pins/reservations coordinate with the sole writer. GC rechecks the full root closure and pins
-- immediately before tombstoning. Keep artifact registration rows for collected/missing/unknown diagnostics.
INSERT INTO schema_migrations VALUES(3,'2026-11-25T00:00:00Z');
PRAGMA user_version = 3;
COMMIT;
