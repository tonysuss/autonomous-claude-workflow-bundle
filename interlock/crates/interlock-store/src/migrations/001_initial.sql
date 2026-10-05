-- interlock store, schema 1. Records are stored as schema-validated JSON,
-- with the columns queries need pulled out beside them.

CREATE TABLE meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE tasks (
    id         TEXT PRIMARY KEY,
    state      TEXT NOT NULL,
    record     TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE attempts (
    id         TEXT PRIMARY KEY,
    task_id    TEXT NOT NULL REFERENCES tasks(id),
    epoch      INTEGER NOT NULL,
    role       TEXT NOT NULL,
    status     TEXT NOT NULL,
    token_hash TEXT NOT NULL,
    record     TEXT NOT NULL
);
CREATE INDEX attempts_by_task ON attempts(task_id);

CREATE TABLE results (
    seq        INTEGER PRIMARY KEY AUTOINCREMENT,
    id         TEXT NOT NULL UNIQUE,
    task_id    TEXT NOT NULL REFERENCES tasks(id),
    attempt_id TEXT NOT NULL REFERENCES attempts(id),
    epoch      INTEGER NOT NULL,
    status     TEXT NOT NULL,
    record     TEXT NOT NULL
);
CREATE INDEX results_by_task ON results(task_id);

-- Worker claims and verifier assessments never share a table.
CREATE TABLE claims (
    seq          INTEGER PRIMARY KEY AUTOINCREMENT,
    id           TEXT NOT NULL UNIQUE,
    task_id      TEXT NOT NULL REFERENCES tasks(id),
    criterion_id TEXT NOT NULL,
    attempt_id   TEXT NOT NULL REFERENCES attempts(id),
    record       TEXT NOT NULL
);
CREATE INDEX claims_by_task ON claims(task_id);

CREATE TABLE assessments (
    seq          INTEGER PRIMARY KEY AUTOINCREMENT,
    id           TEXT NOT NULL UNIQUE,
    task_id      TEXT NOT NULL REFERENCES tasks(id),
    criterion_id TEXT NOT NULL,
    attempt_id   TEXT NOT NULL REFERENCES attempts(id),
    record       TEXT NOT NULL
);
CREATE INDEX assessments_by_task ON assessments(task_id);

-- An event row is written in the same transaction as its effect, already
-- acknowledged. A repeated id finds the row and returns its outcome.
CREATE TABLE events (
    id          TEXT PRIMARY KEY,
    task_id     TEXT NOT NULL,
    attempt_id  TEXT,
    type        TEXT NOT NULL,
    received_at TEXT NOT NULL,
    record      TEXT NOT NULL
);

CREATE TABLE transitions (
    seq        INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id    TEXT NOT NULL REFERENCES tasks(id),
    signal     TEXT NOT NULL,
    from_state TEXT NOT NULL,
    to_state   TEXT NOT NULL,
    reason     TEXT NOT NULL,
    attempt_id TEXT,
    at         TEXT NOT NULL
);
CREATE INDEX transitions_by_task ON transitions(task_id);

CREATE TABLE grants (
    id         TEXT PRIMARY KEY,
    record     TEXT NOT NULL
);

CREATE TABLE operations (
    id      TEXT PRIMARY KEY,
    task_id TEXT NOT NULL REFERENCES tasks(id),
    kind    TEXT NOT NULL,
    state   TEXT NOT NULL,
    record  TEXT NOT NULL
);

CREATE TABLE host_capabilities (
    host         TEXT PRIMARY KEY,
    record       TEXT NOT NULL,
    inspected_at TEXT NOT NULL
);

-- Append-only tables.
CREATE TRIGGER claims_no_update BEFORE UPDATE ON claims
BEGIN SELECT RAISE(ABORT, 'claims are append-only'); END;
CREATE TRIGGER claims_no_delete BEFORE DELETE ON claims
BEGIN SELECT RAISE(ABORT, 'claims are append-only'); END;
CREATE TRIGGER assessments_no_update BEFORE UPDATE ON assessments
BEGIN SELECT RAISE(ABORT, 'assessments are append-only'); END;
CREATE TRIGGER assessments_no_delete BEFORE DELETE ON assessments
BEGIN SELECT RAISE(ABORT, 'assessments are append-only'); END;
CREATE TRIGGER results_no_update BEFORE UPDATE ON results
BEGIN SELECT RAISE(ABORT, 'results are append-only'); END;
CREATE TRIGGER results_no_delete BEFORE DELETE ON results
BEGIN SELECT RAISE(ABORT, 'results are append-only'); END;
CREATE TRIGGER events_no_update BEFORE UPDATE ON events
BEGIN SELECT RAISE(ABORT, 'events are append-only'); END;
CREATE TRIGGER events_no_delete BEFORE DELETE ON events
BEGIN SELECT RAISE(ABORT, 'events are append-only'); END;
CREATE TRIGGER transitions_no_update BEFORE UPDATE ON transitions
BEGIN SELECT RAISE(ABORT, 'transitions are append-only'); END;
CREATE TRIGGER transitions_no_delete BEFORE DELETE ON transitions
BEGIN SELECT RAISE(ABORT, 'transitions are append-only'); END;
