-- pipeline-memory · canonical schema · CLAUDE.md §"Memory architecture"
-- Run via Memory::open · idempotent (CREATE IF NOT EXISTS).

CREATE TABLE IF NOT EXISTS projects (
    id           TEXT PRIMARY KEY,
    name         TEXT NOT NULL,
    stack        TEXT,
    config_json  TEXT,
    created_at   TEXT NOT NULL,
    last_active  TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS sessions (
    id          TEXT PRIMARY KEY,
    project_id  TEXT NOT NULL,
    agent_id    TEXT,
    started_at  TEXT NOT NULL,
    ended_at    TEXT,
    goal        TEXT,
    outcome     TEXT,
    summary     TEXT,
    FOREIGN KEY (project_id) REFERENCES projects(id)
);

CREATE TABLE IF NOT EXISTS session_locks (
    project_id  TEXT PRIMARY KEY,
    session_id  TEXT NOT NULL,
    locked_at   TEXT NOT NULL,
    agent_id    TEXT,
    FOREIGN KEY (session_id) REFERENCES sessions(id)
);

CREATE TABLE IF NOT EXISTS pipeline_runs (
    id            TEXT PRIMARY KEY,
    project_id    TEXT NOT NULL,
    session_id    TEXT,
    profile       TEXT NOT NULL,
    stage         TEXT NOT NULL,
    status        TEXT NOT NULL,
    duration_ms   INTEGER NOT NULL,
    triggered_by  TEXT,
    commit_sha    TEXT,
    created_at    TEXT NOT NULL,
    stdout        TEXT,
    stderr        TEXT,
    failure_json  TEXT,
    FOREIGN KEY (project_id) REFERENCES projects(id)
);

CREATE TABLE IF NOT EXISTS failures (
    id             TEXT PRIMARY KEY,
    run_id         TEXT NOT NULL,
    stage          TEXT NOT NULL,
    error_message  TEXT NOT NULL,
    file           TEXT,
    line           INTEGER,
    fix_applied    TEXT,
    fix_worked     INTEGER,
    created_at     TEXT NOT NULL,
    FOREIGN KEY (run_id) REFERENCES pipeline_runs(id)
);

CREATE TABLE IF NOT EXISTS memory_kv (
    id          TEXT PRIMARY KEY,
    project_id  TEXT NOT NULL,
    scope       TEXT NOT NULL,
    key         TEXT NOT NULL,
    value       TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    expires_at  TEXT,
    UNIQUE (project_id, scope, key)
);

CREATE INDEX IF NOT EXISTS idx_runs_project ON pipeline_runs(project_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_runs_session ON pipeline_runs(session_id);
CREATE INDEX IF NOT EXISTS idx_failures_run ON failures(run_id);
CREATE INDEX IF NOT EXISTS idx_memory_lookup ON memory_kv(project_id, scope, key);
CREATE INDEX IF NOT EXISTS idx_sessions_project ON sessions(project_id, started_at DESC);
