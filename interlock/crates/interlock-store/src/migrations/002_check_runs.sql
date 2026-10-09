-- interlock store, schema 2: interlock's own check runs. Output lives in
-- content-addressed files beside the database.

CREATE TABLE check_runs (
    seq          INTEGER PRIMARY KEY AUTOINCREMENT,
    id           TEXT NOT NULL UNIQUE,
    task_id      TEXT NOT NULL REFERENCES tasks(id),
    criterion_id TEXT NOT NULL,
    attempt_id   TEXT,
    target       TEXT NOT NULL,
    record       TEXT NOT NULL
);
CREATE INDEX check_runs_by_task ON check_runs(task_id);

CREATE TRIGGER check_runs_no_update BEFORE UPDATE ON check_runs
BEGIN SELECT RAISE(ABORT, 'check runs are append-only'); END;
CREATE TRIGGER check_runs_no_delete BEFORE DELETE ON check_runs
BEGIN SELECT RAISE(ABORT, 'check runs are append-only'); END;
