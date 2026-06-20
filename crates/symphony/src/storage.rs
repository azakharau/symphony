mod rows;
mod self_defects;

use std::{borrow::Borrow, path::Path, time::Duration};

use libsql::{Builder, Connection, params};
use thiserror::Error;

use crate::{
    config::RootConfig,
    state::{
        CleanupStatus, EvalRunRecord, IssueStateRecord, LifecycleStage, OpenCodeSessionRecord,
        OpenCodeStageEventRecord, ProjectRuntimeLivenessRecord, ProjectStateRecord,
        RuntimeLivenessStatus, StateParseError,
    },
};
use rows::{
    collect_rows, encode_optional, eval_run_from_row, issue_from_row, liveness_from_row,
    optional_row, project_from_row, session_from_row, stage_event_from_row,
};

const RUNTIME_STATE_MIGRATION: &str = include_str!("../migrations/001_runtime_state.sql");

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CleanupReport {
    pub issues_deleted: u64,
    pub sessions_deleted: u64,
    pub stage_events_deleted: u64,
    pub eval_runs_deleted: u64,
    pub self_defects_deleted: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenCodeCleanupCandidate {
    pub project_id: String,
    pub issue_id: String,
    pub issue_identifier: String,
    pub session_id: String,
}

#[derive(Clone)]
pub struct SqliteStore {
    conn: Connection,
}

impl SqliteStore {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let database = Builder::new_local(path.as_ref().display().to_string())
            .build()
            .await?;
        let conn = database.connect()?;
        conn.execute_batch(
            r#"
            PRAGMA foreign_keys = ON;
            PRAGMA busy_timeout = 5000;
            "#,
        )
        .await?;
        Ok(Self { conn })
    }

    pub async fn migrate(&self) -> Result<(), StorageError> {
        self.conn.execute_batch(RUNTIME_STATE_MIGRATION).await?;
        self.ensure_column("opencode_sessions", "process_id", "INTEGER")
            .await?;
        self.drop_issue_linear_state_column().await?;
        Ok(())
    }

    async fn drop_issue_linear_state_column(&self) -> Result<(), StorageError> {
        if !self.column_exists("issues", "state").await? {
            return Ok(());
        }

        self.conn
            .execute_batch(
                r#"
                PRAGMA foreign_keys = OFF;

                CREATE TABLE issues_without_linear_state (
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

                INSERT INTO issues_without_linear_state (
                    project_id,
                    issue_id,
                    identifier,
                    title,
                    lifecycle_stage,
                    blocker_json,
                    failure_json,
                    git_ref_json,
                    cleanup_status,
                    updated_at
                )
                SELECT
                    project_id,
                    issue_id,
                    identifier,
                    title,
                    lifecycle_stage,
                    blocker_json,
                    failure_json,
                    git_ref_json,
                    cleanup_status,
                    updated_at
                FROM issues;

                DROP TABLE issues;
                ALTER TABLE issues_without_linear_state RENAME TO issues;

                PRAGMA foreign_keys = ON;
                "#,
            )
            .await?;
        Ok(())
    }

    async fn ensure_column(
        &self,
        table: &str,
        column: &str,
        definition: &str,
    ) -> Result<(), StorageError> {
        if self.column_exists(table, column).await? {
            return Ok(());
        }
        self.conn
            .execute(
                format!("ALTER TABLE {table} ADD COLUMN {column} {definition}").as_str(),
                (),
            )
            .await?;
        Ok(())
    }

    async fn column_exists(&self, table: &str, column: &str) -> Result<bool, StorageError> {
        let mut rows = self
            .conn
            .query(format!("PRAGMA table_info({table})").as_str(), ())
            .await?;
        while let Some(row) = rows.next().await? {
            let name: String = row.get(1)?;
            if name == column {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub async fn applied_migrations(&self) -> Result<Vec<String>, StorageError> {
        let mut rows = self
            .conn
            .query("SELECT id FROM schema_migrations ORDER BY id ASC", ())
            .await?;
        let mut migrations = Vec::new();
        while let Some(row) = rows.next().await? {
            migrations.push(row.get::<String>(0)?);
        }
        Ok(migrations)
    }

    pub async fn upsert_project<P>(&self, project: P) -> Result<(), StorageError>
    where
        P: Borrow<ProjectStateRecord> + Send + Sync,
    {
        let project = project.borrow();
        self.conn
            .execute(
                r#"
                INSERT INTO projects (project_id, name, enabled, lifecycle_stage, cleanup_status)
                VALUES (?1, ?2, ?3, ?4, ?5)
                ON CONFLICT(project_id) DO UPDATE SET
                    name = excluded.name,
                    enabled = excluded.enabled,
                    lifecycle_stage = excluded.lifecycle_stage,
                    cleanup_status = excluded.cleanup_status,
                    updated_at = CURRENT_TIMESTAMP
                "#,
                params![
                    project.project_id.as_str(),
                    project.name.as_str(),
                    project.enabled,
                    project.lifecycle_stage.as_str(),
                    project.cleanup_status.as_str(),
                ],
            )
            .await?;
        Ok(())
    }

    pub async fn reconcile_projects(&self, config: &RootConfig) -> Result<(), StorageError> {
        for project in config.projects() {
            self.upsert_project(ProjectStateRecord {
                project_id: project.id.clone(),
                name: project.name.clone(),
                enabled: project.enabled,
                lifecycle_stage: LifecycleStage::Queued,
                cleanup_status: CleanupStatus::Clean,
            })
            .await?;
        }
        Ok(())
    }

    pub async fn projects(&self) -> Result<Vec<ProjectStateRecord>, StorageError> {
        let mut rows = self
            .conn
            .query(
                "SELECT project_id, name, enabled, lifecycle_stage, cleanup_status FROM projects ORDER BY project_id ASC",
                (),
            )
            .await?;
        collect_rows(&mut rows, project_from_row).await
    }

    pub async fn project(
        &self,
        project_id: &str,
    ) -> Result<Option<ProjectStateRecord>, StorageError> {
        let mut rows = self
            .conn
            .query(
                "SELECT project_id, name, enabled, lifecycle_stage, cleanup_status FROM projects WHERE project_id = ?1",
                params![project_id],
            )
            .await?;
        optional_row(&mut rows, project_from_row).await
    }

    pub async fn upsert_project_liveness<L>(&self, liveness: L) -> Result<(), StorageError>
    where
        L: Borrow<ProjectRuntimeLivenessRecord> + Send + Sync,
    {
        let liveness = liveness.borrow();
        self.conn
            .execute(
                r#"
                INSERT INTO project_runtime_liveness (
                    project_id,
                    status,
                    reason,
                    last_poll_at,
                    last_successful_candidate_scan_at,
                    max_sessions,
                    running_sessions,
                    available_sessions
                )
                VALUES (?1, ?2, ?3, COALESCE(?4, CURRENT_TIMESTAMP), ?5, ?6, ?7, ?8)
                ON CONFLICT(project_id) DO UPDATE SET
                    status = excluded.status,
                    reason = excluded.reason,
                    last_poll_at = excluded.last_poll_at,
                    last_successful_candidate_scan_at = excluded.last_successful_candidate_scan_at,
                    max_sessions = excluded.max_sessions,
                    running_sessions = excluded.running_sessions,
                    available_sessions = excluded.available_sessions,
                    updated_at = CURRENT_TIMESTAMP
                "#,
                params![
                    liveness.project_id.as_str(),
                    liveness.status.as_str(),
                    liveness.reason.as_str(),
                    liveness.last_poll_at.as_deref(),
                    liveness.last_successful_candidate_scan_at.as_deref(),
                    liveness.max_sessions as i64,
                    liveness.running_sessions as i64,
                    liveness.available_sessions as i64,
                ],
            )
            .await?;
        Ok(())
    }

    pub async fn project_liveness(
        &self,
        project_id: &str,
    ) -> Result<Option<ProjectRuntimeLivenessRecord>, StorageError> {
        let mut rows = self
            .conn
            .query(
                r#"
                SELECT project_id, status, reason, last_poll_at, last_successful_candidate_scan_at,
                       max_sessions, running_sessions, available_sessions
                FROM project_runtime_liveness
                WHERE project_id = ?1
                "#,
                params![project_id],
            )
            .await?;
        optional_row(&mut rows, liveness_from_row).await
    }

    pub async fn mark_project_liveness_poll(
        &self,
        project_id: &str,
        status: RuntimeLivenessStatus,
        reason: &str,
        max_sessions: u32,
        running_sessions: u32,
        successful_candidate_scan: bool,
    ) -> Result<(), StorageError> {
        let available_sessions = max_sessions.saturating_sub(running_sessions);
        self.conn
            .execute(
                r#"
                INSERT INTO project_runtime_liveness (
                    project_id,
                    status,
                    reason,
                    last_poll_at,
                    last_successful_candidate_scan_at,
                    max_sessions,
                    running_sessions,
                    available_sessions
                )
                VALUES (
                    ?1,
                    ?2,
                    ?3,
                    CURRENT_TIMESTAMP,
                    CASE WHEN ?7 THEN CURRENT_TIMESTAMP ELSE NULL END,
                    ?4,
                    ?5,
                    ?6
                )
                ON CONFLICT(project_id) DO UPDATE SET
                    status = excluded.status,
                    reason = excluded.reason,
                    last_poll_at = excluded.last_poll_at,
                    last_successful_candidate_scan_at = CASE
                        WHEN ?7 THEN excluded.last_successful_candidate_scan_at
                        ELSE project_runtime_liveness.last_successful_candidate_scan_at
                    END,
                    max_sessions = excluded.max_sessions,
                    running_sessions = excluded.running_sessions,
                    available_sessions = excluded.available_sessions,
                    updated_at = CURRENT_TIMESTAMP
                "#,
                params![
                    project_id,
                    status.as_str(),
                    reason,
                    max_sessions as i64,
                    running_sessions as i64,
                    available_sessions as i64,
                    successful_candidate_scan,
                ],
            )
            .await?;
        Ok(())
    }

    pub async fn upsert_issue<I>(&self, issue: I) -> Result<(), StorageError>
    where
        I: Borrow<IssueStateRecord> + Send + Sync,
    {
        let issue = issue.borrow();
        let blocker_json = encode_optional(&issue.blocker)?;
        let failure_json = encode_optional(&issue.failure)?;
        let git_ref_json = encode_optional(&issue.git_ref)?;

        self.conn
            .execute(
                r#"
                INSERT INTO issues (
                    project_id,
                    issue_id,
                    identifier,
                    title,
                    lifecycle_stage,
                    blocker_json,
                    failure_json,
                    git_ref_json,
                    cleanup_status
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                ON CONFLICT(project_id, issue_id) DO UPDATE SET
                    identifier = excluded.identifier,
                    title = excluded.title,
                    lifecycle_stage = excluded.lifecycle_stage,
                    blocker_json = excluded.blocker_json,
                    failure_json = excluded.failure_json,
                    git_ref_json = excluded.git_ref_json,
                    cleanup_status = excluded.cleanup_status,
                    updated_at = CURRENT_TIMESTAMP
                "#,
                params![
                    issue.project_id.as_str(),
                    issue.issue_id.as_str(),
                    issue.identifier.as_str(),
                    issue.title.as_str(),
                    issue.lifecycle_stage.as_str(),
                    blocker_json,
                    failure_json,
                    git_ref_json,
                    issue.cleanup_status.as_str(),
                ],
            )
            .await?;
        Ok(())
    }

    pub async fn issues_for_project(
        &self,
        project_id: &str,
    ) -> Result<Vec<IssueStateRecord>, StorageError> {
        let mut rows = self
            .conn
            .query(
                r#"
                SELECT project_id, issue_id, identifier, title, lifecycle_stage,
                       blocker_json, failure_json, git_ref_json, cleanup_status
                FROM issues
                WHERE project_id = ?1
                ORDER BY identifier ASC, issue_id ASC
                "#,
                params![project_id],
            )
            .await?;
        collect_rows(&mut rows, issue_from_row).await
    }

    pub async fn issue(
        &self,
        project_id: &str,
        issue_id: &str,
    ) -> Result<Option<IssueStateRecord>, StorageError> {
        let mut rows = self
            .conn
            .query(
                r#"
                SELECT project_id, issue_id, identifier, title, lifecycle_stage,
                       blocker_json, failure_json, git_ref_json, cleanup_status
                FROM issues
                WHERE project_id = ?1 AND issue_id = ?2
                "#,
                params![project_id, issue_id],
            )
            .await?;
        optional_row(&mut rows, issue_from_row).await
    }

    pub async fn upsert_opencode_session<S>(&self, session: S) -> Result<(), StorageError>
    where
        S: Borrow<OpenCodeSessionRecord> + Send + Sync,
    {
        let session = session.borrow();
        self.conn
            .execute(
                r#"
                INSERT INTO opencode_sessions (
                    project_id,
                    issue_id,
                    session_id,
                    agent,
                    model,
                    worktree_path,
                    process_id,
                    lifecycle_stage,
                    stage,
                    active_agent,
                    active_model,
                    message_count,
                    todo_count,
                    part_count,
                    token_count,
                    cost_micros,
                    subagent_count,
                    eval_stage,
                    lifecycle_marker,
                    last_event,
                    silence_observed
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)
                ON CONFLICT(project_id, issue_id, session_id) DO UPDATE SET
                    agent = excluded.agent,
                    model = excluded.model,
                    worktree_path = excluded.worktree_path,
                    process_id = excluded.process_id,
                    lifecycle_stage = excluded.lifecycle_stage,
                    stage = excluded.stage,
                    active_agent = excluded.active_agent,
                    active_model = excluded.active_model,
                    message_count = excluded.message_count,
                    todo_count = excluded.todo_count,
                    part_count = excluded.part_count,
                    token_count = excluded.token_count,
                    cost_micros = excluded.cost_micros,
                    subagent_count = excluded.subagent_count,
                    eval_stage = excluded.eval_stage,
                    lifecycle_marker = excluded.lifecycle_marker,
                    last_event = excluded.last_event,
                    silence_observed = excluded.silence_observed,
                    updated_at = CURRENT_TIMESTAMP
                "#,
                params![
                    session.project_id.as_str(),
                    session.issue_id.as_str(),
                    session.session_id.as_str(),
                    session.agent.as_str(),
                    session.model.as_deref(),
                    session.worktree_path.as_str(),
                    session.process_id.map(i64::from),
                    session.lifecycle_stage.as_str(),
                    session.stage.as_str(),
                    session.active_agent.as_deref(),
                    session.active_model.as_deref(),
                    session.message_count as i64,
                    session.todo_count as i64,
                    session.part_count as i64,
                    session.token_count as i64,
                    session.cost_micros as i64,
                    session.subagent_count as i64,
                    session.eval_stage.as_deref(),
                    session.lifecycle_marker.as_deref(),
                    session.last_event.as_deref(),
                    session.silence_observed,
                ],
            )
            .await?;
        Ok(())
    }

    pub async fn opencode_session(
        &self,
        project_id: &str,
        issue_id: &str,
        session_id: &str,
    ) -> Result<Option<OpenCodeSessionRecord>, StorageError> {
        let mut rows = self
            .conn
            .query(
                r#"
                SELECT project_id, issue_id, session_id, agent, model, worktree_path,
                       process_id, lifecycle_stage, stage, active_agent, active_model, message_count,
                       todo_count, part_count, token_count, cost_micros, subagent_count,
                       eval_stage, lifecycle_marker, last_event, silence_observed
                FROM opencode_sessions
                WHERE project_id = ?1 AND issue_id = ?2 AND session_id = ?3
                "#,
                params![project_id, issue_id, session_id],
            )
            .await?;
        optional_row(&mut rows, session_from_row).await
    }

    pub async fn delete_opencode_session(
        &self,
        project_id: &str,
        issue_id: &str,
        session_id: &str,
    ) -> Result<(), StorageError> {
        self.conn
            .execute(
                r#"
                DELETE FROM opencode_sessions
                WHERE project_id = ?1 AND issue_id = ?2 AND session_id = ?3
                "#,
                params![project_id, issue_id, session_id],
            )
            .await?;
        Ok(())
    }

    pub async fn opencode_sessions_for_issue(
        &self,
        project_id: &str,
        issue_id: &str,
    ) -> Result<Vec<OpenCodeSessionRecord>, StorageError> {
        let mut rows = self
            .conn
            .query(
                r#"
                SELECT project_id, issue_id, session_id, agent, model, worktree_path,
                       process_id, lifecycle_stage, stage, active_agent, active_model, message_count,
                       todo_count, part_count, token_count, cost_micros, subagent_count,
                       eval_stage, lifecycle_marker, last_event, silence_observed
                FROM opencode_sessions
                WHERE project_id = ?1 AND issue_id = ?2
                ORDER BY updated_at ASC, rowid ASC, session_id ASC
                "#,
                params![project_id, issue_id],
            )
            .await?;
        collect_rows(&mut rows, session_from_row).await
    }

    pub async fn active_opencode_sessions(
        &self,
    ) -> Result<Vec<OpenCodeSessionRecord>, StorageError> {
        let mut rows = self
            .conn
            .query(
                r#"
                SELECT project_id, issue_id, session_id, agent, model, worktree_path,
                       process_id, lifecycle_stage, stage, active_agent, active_model, message_count,
                       todo_count, part_count, token_count, cost_micros, subagent_count,
                       eval_stage, lifecycle_marker, last_event, silence_observed
                FROM opencode_sessions
                WHERE lifecycle_stage = 'running'
                   OR stage IN ('starting', 'running', 'eval', 'review', 'handoff', 'silent')
                ORDER BY updated_at ASC, rowid ASC, session_id ASC
                "#,
                (),
            )
            .await?;
        collect_rows(&mut rows, session_from_row).await
    }

    pub async fn upsert_opencode_stage_event<E>(&self, event: E) -> Result<(), StorageError>
    where
        E: Borrow<OpenCodeStageEventRecord> + Send + Sync,
    {
        let event = event.borrow();
        self.conn
            .execute(
                r#"
                INSERT INTO opencode_stage_events (
                    project_id,
                    issue_id,
                    session_id,
                    sequence,
                    stage,
                    event
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                ON CONFLICT(project_id, issue_id, session_id, sequence) DO UPDATE SET
                    stage = excluded.stage,
                    event = excluded.event,
                    updated_at = CURRENT_TIMESTAMP
                "#,
                params![
                    event.project_id.as_str(),
                    event.issue_id.as_str(),
                    event.session_id.as_str(),
                    event.sequence as i64,
                    event.stage.as_str(),
                    event.event.as_deref(),
                ],
            )
            .await?;
        Ok(())
    }

    pub async fn opencode_stage_events_for_session(
        &self,
        project_id: &str,
        issue_id: &str,
        session_id: &str,
    ) -> Result<Vec<OpenCodeStageEventRecord>, StorageError> {
        let mut rows = self
            .conn
            .query(
                r#"
                SELECT project_id, issue_id, session_id, sequence, stage, event
                FROM opencode_stage_events
                WHERE project_id = ?1 AND issue_id = ?2 AND session_id = ?3
                ORDER BY sequence ASC
                "#,
                params![project_id, issue_id, session_id],
            )
            .await?;
        collect_rows(&mut rows, stage_event_from_row).await
    }

    pub async fn upsert_eval_run<E>(&self, eval: E) -> Result<(), StorageError>
    where
        E: Borrow<EvalRunRecord> + Send + Sync,
    {
        let eval = eval.borrow();
        self.conn
            .execute(
                r#"
                INSERT INTO eval_runs (
                    project_id,
                    issue_id,
                    run_id,
                    suite,
                    status,
                    details_json
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                ON CONFLICT(project_id, issue_id, run_id) DO UPDATE SET
                    suite = excluded.suite,
                    status = excluded.status,
                    details_json = excluded.details_json,
                    updated_at = CURRENT_TIMESTAMP
                "#,
                params![
                    eval.project_id.as_str(),
                    eval.issue_id.as_str(),
                    eval.run_id.as_str(),
                    eval.suite.as_str(),
                    eval.status.as_str(),
                    eval.details_json.as_deref(),
                ],
            )
            .await?;
        Ok(())
    }

    pub async fn eval_runs_for_issue(
        &self,
        project_id: &str,
        issue_id: &str,
    ) -> Result<Vec<EvalRunRecord>, StorageError> {
        let mut rows = self
            .conn
            .query(
                r#"
                SELECT project_id, issue_id, run_id, suite, status, details_json
                FROM eval_runs
                WHERE project_id = ?1 AND issue_id = ?2
                ORDER BY run_id ASC
                "#,
                params![project_id, issue_id],
            )
            .await?;
        collect_rows(&mut rows, eval_run_from_row).await
    }

    pub async fn cleanup_runtime_state(
        &self,
        retention: Duration,
    ) -> Result<CleanupReport, StorageError> {
        let retention_seconds = i64::try_from(retention.as_secs()).unwrap_or(i64::MAX);
        let cutoff_modifier = format!("-{retention_seconds} seconds");

        let eval_runs_deleted = self
            .conn
            .execute(
                r#"
                DELETE FROM eval_runs
                WHERE updated_at <= datetime('now', ?1)
                  AND NOT EXISTS (
                    SELECT 1
                    FROM issues
                    WHERE issues.project_id = eval_runs.project_id
                      AND issues.issue_id = eval_runs.issue_id
                      AND issues.lifecycle_stage IN ('running', 'blocked')
                  )
                "#,
                params![cutoff_modifier.as_str()],
            )
            .await?;

        let stage_events_deleted = self
            .conn
            .execute(
                r#"
                DELETE FROM opencode_stage_events
                WHERE updated_at <= datetime('now', ?1)
                  AND NOT EXISTS (
                    SELECT 1
                    FROM opencode_sessions
                    WHERE opencode_sessions.project_id = opencode_stage_events.project_id
                      AND opencode_sessions.issue_id = opencode_stage_events.issue_id
                      AND opencode_sessions.session_id = opencode_stage_events.session_id
                      AND opencode_sessions.lifecycle_stage = 'running'
                  )
                "#,
                params![cutoff_modifier.as_str()],
            )
            .await?;

        let sessions_deleted = self
            .conn
            .execute(
                r#"
                DELETE FROM opencode_sessions
                WHERE lifecycle_stage != 'running'
                  AND updated_at <= datetime('now', ?1)
                "#,
                params![cutoff_modifier.as_str()],
            )
            .await?;

        let self_defects_deleted = self
            .conn
            .execute(
                r#"
                DELETE FROM self_defect_registry
                WHERE resolution_state != 'open'
                  AND last_seen_at <= datetime('now', ?1)
                "#,
                params![cutoff_modifier.as_str()],
            )
            .await?;

        let issues_deleted = self
            .conn
            .execute(
                r#"
                DELETE FROM issues
                WHERE lifecycle_stage NOT IN ('running', 'blocked')
                  AND updated_at <= datetime('now', ?1)
                  AND NOT EXISTS (
                    SELECT 1
                    FROM opencode_sessions
                    WHERE opencode_sessions.project_id = issues.project_id
                      AND opencode_sessions.issue_id = issues.issue_id
                  )
                "#,
                params![cutoff_modifier.as_str()],
            )
            .await?;

        Ok(CleanupReport {
            issues_deleted,
            sessions_deleted,
            stage_events_deleted,
            eval_runs_deleted,
            self_defects_deleted,
        })
    }

    pub async fn opencode_cleanup_candidates(
        &self,
        retention: Duration,
    ) -> Result<Vec<OpenCodeCleanupCandidate>, StorageError> {
        let retention_seconds = i64::try_from(retention.as_secs()).unwrap_or(i64::MAX);
        let cutoff_modifier = format!("-{retention_seconds} seconds");
        let mut rows = self
            .conn
            .query(
                r#"
                SELECT issues.project_id, issues.issue_id, issues.identifier, opencode_sessions.session_id
                FROM issues
                INNER JOIN opencode_sessions
                  ON opencode_sessions.project_id = issues.project_id
                 AND opencode_sessions.issue_id = issues.issue_id
                WHERE issues.lifecycle_stage = 'completed'
                  AND opencode_sessions.updated_at <= datetime('now', ?1)
                ORDER BY issues.project_id ASC, issues.identifier ASC, opencode_sessions.session_id ASC
                "#,
                params![cutoff_modifier.as_str()],
            )
            .await?;

        let mut candidates = Vec::new();
        while let Some(row) = rows.next().await? {
            candidates.push(OpenCodeCleanupCandidate {
                project_id: row.get(0)?,
                issue_id: row.get(1)?,
                issue_identifier: row.get(2)?,
                session_id: row.get(3)?,
            });
        }
        Ok(candidates)
    }

    pub async fn delete_opencode_session_record(
        &self,
        project_id: &str,
        issue_id: &str,
        session_id: &str,
    ) -> Result<u64, StorageError> {
        Ok(self
            .conn
            .execute(
                r#"
                DELETE FROM opencode_sessions
                WHERE project_id = ?1 AND issue_id = ?2 AND session_id = ?3
                "#,
                params![project_id, issue_id, session_id],
            )
            .await?)
    }
}

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] libsql::Error),
    #[error("state parse error: {0}")]
    State(#[from] StateParseError),
    #[error("state serialization error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("storage invariant violation: {0}")]
    Invariant(String),
}
