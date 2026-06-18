CREATE TABLE IF NOT EXISTS schema_migrations (
    id TEXT PRIMARY KEY,
    applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS projects (
    project_id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    enabled INTEGER NOT NULL,
    lifecycle_stage TEXT NOT NULL,
    cleanup_status TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS project_runtime_liveness (
    project_id TEXT PRIMARY KEY,
    status TEXT NOT NULL,
    reason TEXT NOT NULL,
    last_poll_at TEXT,
    last_successful_candidate_scan_at TEXT,
    max_sessions INTEGER NOT NULL,
    running_sessions INTEGER NOT NULL,
    available_sessions INTEGER NOT NULL,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY (project_id) REFERENCES projects(project_id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS issues (
    project_id TEXT NOT NULL,
    issue_id TEXT NOT NULL,
    identifier TEXT NOT NULL,
    title TEXT NOT NULL,
    lifecycle_stage TEXT NOT NULL,
    blocker_json TEXT,
    failure_json TEXT,
    git_ref_json TEXT,
    cleanup_status TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (project_id, issue_id),
    FOREIGN KEY (project_id) REFERENCES projects(project_id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS opencode_sessions (
    project_id TEXT NOT NULL,
    issue_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    agent TEXT NOT NULL,
    model TEXT,
    worktree_path TEXT NOT NULL,
    process_id INTEGER,
    lifecycle_stage TEXT NOT NULL,
    stage TEXT NOT NULL,
    active_agent TEXT,
    active_model TEXT,
    message_count INTEGER NOT NULL,
    todo_count INTEGER NOT NULL,
    part_count INTEGER NOT NULL,
    token_count INTEGER NOT NULL,
    cost_micros INTEGER NOT NULL,
    subagent_count INTEGER NOT NULL,
    eval_stage TEXT,
    lifecycle_marker TEXT,
    last_event TEXT,
    silence_observed INTEGER NOT NULL,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (project_id, issue_id, session_id),
    FOREIGN KEY (project_id, issue_id) REFERENCES issues(project_id, issue_id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS opencode_stage_events (
    project_id TEXT NOT NULL,
    issue_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    sequence INTEGER NOT NULL,
    stage TEXT NOT NULL,
    event TEXT,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (project_id, issue_id, session_id, sequence),
    FOREIGN KEY (project_id, issue_id, session_id) REFERENCES opencode_sessions(project_id, issue_id, session_id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS eval_runs (
    project_id TEXT NOT NULL,
    issue_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    suite TEXT NOT NULL,
    status TEXT NOT NULL,
    details_json TEXT,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (project_id, issue_id, run_id),
    FOREIGN KEY (project_id, issue_id) REFERENCES issues(project_id, issue_id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS self_defect_registry (
    registry_id TEXT PRIMARY KEY,
    fingerprint TEXT NOT NULL,
    defect_kind TEXT NOT NULL,
    category TEXT NOT NULL,
    severity TEXT NOT NULL,
    initial_routing_decision TEXT NOT NULL,
    source_project_id TEXT NOT NULL,
    source_issue_id TEXT NOT NULL,
    source_issue_identifier TEXT NOT NULL,
    source_session_id TEXT,
    source_process_id INTEGER,
    managed_issue_id TEXT NOT NULL,
    managed_issue_identifier TEXT NOT NULL,
    occurrence_count INTEGER NOT NULL,
    first_seen_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    last_seen_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    latest_evidence_summary TEXT NOT NULL,
    resolution_state TEXT NOT NULL,
    relation_mode TEXT NOT NULL
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_self_defect_registry_open_fingerprint
ON self_defect_registry(fingerprint)
WHERE resolution_state = 'open';

CREATE INDEX IF NOT EXISTS idx_self_defect_registry_managed_issue
ON self_defect_registry(managed_issue_id);

CREATE TABLE IF NOT EXISTS self_defect_recommendations (
    recommendation_id TEXT PRIMARY KEY,
    evidence_fingerprint TEXT NOT NULL,
    defect_kind TEXT NOT NULL,
    defect_category TEXT NOT NULL,
    confidence TEXT NOT NULL,
    evidence_refs_json TEXT NOT NULL,
    recommended_action TEXT NOT NULL,
    rationale TEXT NOT NULL,
    source_project_id TEXT NOT NULL,
    source_issue_id TEXT NOT NULL,
    source_issue_identifier TEXT NOT NULL,
    source_session_id TEXT,
    source_process_id INTEGER,
    occurrence_count INTEGER NOT NULL,
    first_seen_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    last_seen_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    resolution_state TEXT NOT NULL DEFAULT 'open'
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_self_defect_recommendations_open_evidence
ON self_defect_recommendations(evidence_fingerprint)
WHERE resolution_state = 'open';

INSERT OR IGNORE INTO schema_migrations (id) VALUES ('001_runtime_state');
