mod rows;

use std::{borrow::Borrow, path::Path};

use libsql::{Builder, Connection, params};
use thiserror::Error;

use crate::{
    config::RootConfig,
    state::{
        CleanupStatus, EvalRunRecord, IssueStateRecord, LifecycleStage, OpenCodeSessionRecord,
        OpenCodeStageEventRecord, ProjectStateRecord, StateParseError,
    },
};
use rows::{
    collect_rows, encode_optional, eval_run_from_row, issue_from_row, optional_row,
    project_from_row, session_from_row, stage_event_from_row,
};

const RUNTIME_STATE_MIGRATION: &str = include_str!("../migrations/001_runtime_state.sql");

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
        conn.execute("PRAGMA foreign_keys = ON", ()).await?;
        Ok(Self { conn })
    }

    pub async fn migrate(&self) -> Result<(), StorageError> {
        self.conn.execute_batch(RUNTIME_STATE_MIGRATION).await?;
        Ok(())
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
                    cleanup_status = excluded.cleanup_status
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
                    state,
                    lifecycle_stage,
                    blocker_json,
                    failure_json,
                    git_ref_json,
                    cleanup_status
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                ON CONFLICT(project_id, issue_id) DO UPDATE SET
                    identifier = excluded.identifier,
                    title = excluded.title,
                    state = excluded.state,
                    lifecycle_stage = excluded.lifecycle_stage,
                    blocker_json = excluded.blocker_json,
                    failure_json = excluded.failure_json,
                    git_ref_json = excluded.git_ref_json,
                    cleanup_status = excluded.cleanup_status
                "#,
                params![
                    issue.project_id.as_str(),
                    issue.issue_id.as_str(),
                    issue.identifier.as_str(),
                    issue.title.as_str(),
                    issue.state.as_str(),
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
                SELECT project_id, issue_id, identifier, title, state, lifecycle_stage,
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
                SELECT project_id, issue_id, identifier, title, state, lifecycle_stage,
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
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)
                ON CONFLICT(project_id, issue_id, session_id) DO UPDATE SET
                    agent = excluded.agent,
                    model = excluded.model,
                    worktree_path = excluded.worktree_path,
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
                    silence_observed = excluded.silence_observed
                "#,
                params![
                    session.project_id.as_str(),
                    session.issue_id.as_str(),
                    session.session_id.as_str(),
                    session.agent.as_str(),
                    session.model.as_deref(),
                    session.worktree_path.as_str(),
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
                       lifecycle_stage, stage, active_agent, active_model, message_count,
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
                       lifecycle_stage, stage, active_agent, active_model, message_count,
                       todo_count, part_count, token_count, cost_micros, subagent_count,
                       eval_stage, lifecycle_marker, last_event, silence_observed
                FROM opencode_sessions
                WHERE project_id = ?1 AND issue_id = ?2
                ORDER BY session_id ASC
                "#,
                params![project_id, issue_id],
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
                    event = excluded.event
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
                    details_json = excluded.details_json
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
}

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] libsql::Error),
    #[error("state parse error: {0}")]
    State(#[from] StateParseError),
    #[error("state serialization error: {0}")]
    Json(#[from] serde_json::Error),
}
