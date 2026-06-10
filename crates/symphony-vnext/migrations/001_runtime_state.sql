CREATE TABLE IF NOT EXISTS schema_migrations (
    id TEXT PRIMARY KEY,
    applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS projects (
    project_id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    enabled INTEGER NOT NULL,
    lifecycle_stage TEXT NOT NULL,
    cleanup_status TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS issues (
    project_id TEXT NOT NULL,
    issue_id TEXT NOT NULL,
    identifier TEXT NOT NULL,
    title TEXT NOT NULL,
    state TEXT NOT NULL,
    lifecycle_stage TEXT NOT NULL,
    blocker_json TEXT,
    failure_json TEXT,
    git_ref_json TEXT,
    cleanup_status TEXT NOT NULL,
    PRIMARY KEY (project_id, issue_id),
    FOREIGN KEY (project_id) REFERENCES projects(project_id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS opencode_sessions (
    project_id TEXT NOT NULL,
    issue_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    agent TEXT NOT NULL,
    model TEXT,
    lifecycle_stage TEXT NOT NULL,
    last_event TEXT,
    PRIMARY KEY (project_id, issue_id, session_id),
    FOREIGN KEY (project_id, issue_id) REFERENCES issues(project_id, issue_id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS eval_runs (
    project_id TEXT NOT NULL,
    issue_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    suite TEXT NOT NULL,
    status TEXT NOT NULL,
    details_json TEXT,
    PRIMARY KEY (project_id, issue_id, run_id),
    FOREIGN KEY (project_id, issue_id) REFERENCES issues(project_id, issue_id) ON DELETE CASCADE
);

INSERT OR IGNORE INTO schema_migrations (id) VALUES ('001_runtime_state');
