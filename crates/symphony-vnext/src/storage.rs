use std::{path::Path, str::FromStr};

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Serialize, de::DeserializeOwned};
use thiserror::Error;

use crate::{
    config::RootConfig,
    state::{
        BlockerRecord, CleanupStatus, EvalRunRecord, FailureRecord, GitRefRecord, IssueStateRecord,
        LifecycleStage, OpenCodeSessionRecord, OpenCodeStage, OpenCodeStageEventRecord,
        ProjectStateRecord, StateParseError,
    },
};

const RUNTIME_STATE_MIGRATION: &str = include_str!("../migrations/001_runtime_state.sql");

pub struct SqliteStore {
    conn: Connection,
}

impl SqliteStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        Ok(Self { conn })
    }

    pub fn migrate(&self) -> Result<(), StorageError> {
        self.conn.execute_batch(RUNTIME_STATE_MIGRATION)?;
        Ok(())
    }

    pub fn applied_migrations(&self) -> Result<Vec<String>, StorageError> {
        let mut statement = self
            .conn
            .prepare("SELECT id FROM schema_migrations ORDER BY id ASC")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }

    pub fn upsert_project(&self, project: ProjectStateRecord) -> Result<(), StorageError> {
        self.conn.execute(
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
                project.project_id,
                project.name,
                project.enabled,
                project.lifecycle_stage.as_str(),
                project.cleanup_status.as_str()
            ],
        )?;
        Ok(())
    }

    pub fn reconcile_projects(&self, config: &RootConfig) -> Result<(), StorageError> {
        for project in config.projects() {
            self.upsert_project(ProjectStateRecord {
                project_id: project.id.clone(),
                name: project.name.clone(),
                enabled: project.enabled,
                lifecycle_stage: LifecycleStage::Queued,
                cleanup_status: CleanupStatus::Clean,
            })?;
        }

        Ok(())
    }

    pub fn projects(&self) -> Result<Vec<ProjectStateRecord>, StorageError> {
        let mut statement = self.conn.prepare(
            "SELECT project_id, name, enabled, lifecycle_stage, cleanup_status FROM projects ORDER BY project_id ASC",
        )?;
        let rows = statement.query_map([], project_from_row)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }

    pub fn project(&self, project_id: &str) -> Result<Option<ProjectStateRecord>, StorageError> {
        self.conn
            .query_row(
                "SELECT project_id, name, enabled, lifecycle_stage, cleanup_status FROM projects WHERE project_id = ?1",
                params![project_id],
                project_from_row,
            )
            .optional()
            .map_err(StorageError::from)
    }

    pub fn upsert_issue(&self, issue: IssueStateRecord) -> Result<(), StorageError> {
        let blocker_json = encode_optional(&issue.blocker)?;
        let failure_json = encode_optional(&issue.failure)?;
        let git_ref_json = encode_optional(&issue.git_ref)?;

        self.conn.execute(
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
                issue.project_id,
                issue.issue_id,
                issue.identifier,
                issue.title,
                issue.state,
                issue.lifecycle_stage.as_str(),
                blocker_json,
                failure_json,
                git_ref_json,
                issue.cleanup_status.as_str()
            ],
        )?;
        Ok(())
    }

    pub fn issues_for_project(
        &self,
        project_id: &str,
    ) -> Result<Vec<IssueStateRecord>, StorageError> {
        let mut statement = self.conn.prepare(
            r#"
            SELECT project_id, issue_id, identifier, title, state, lifecycle_stage,
                   blocker_json, failure_json, git_ref_json, cleanup_status
            FROM issues
            WHERE project_id = ?1
            ORDER BY identifier ASC, issue_id ASC
            "#,
        )?;
        let rows = statement.query_map(params![project_id], issue_from_row)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }

    pub fn issue(
        &self,
        project_id: &str,
        issue_id: &str,
    ) -> Result<Option<IssueStateRecord>, StorageError> {
        self.conn
            .query_row(
                r#"
                SELECT project_id, issue_id, identifier, title, state, lifecycle_stage,
                       blocker_json, failure_json, git_ref_json, cleanup_status
                FROM issues
                WHERE project_id = ?1 AND issue_id = ?2
                "#,
                params![project_id, issue_id],
                issue_from_row,
            )
            .optional()
            .map_err(StorageError::from)
    }

    pub fn upsert_opencode_session(
        &self,
        session: OpenCodeSessionRecord,
    ) -> Result<(), StorageError> {
        self.conn.execute(
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
                session.project_id,
                session.issue_id,
                session.session_id,
                session.agent,
                session.model,
                session.worktree_path,
                session.lifecycle_stage.as_str(),
                session.stage.as_str(),
                session.active_agent,
                session.active_model,
                session.message_count,
                session.todo_count,
                session.part_count,
                session.token_count,
                session.cost_micros,
                session.subagent_count,
                session.eval_stage,
                session.lifecycle_marker,
                session.last_event,
                session.silence_observed
            ],
        )?;
        Ok(())
    }

    pub fn opencode_session(
        &self,
        project_id: &str,
        issue_id: &str,
        session_id: &str,
    ) -> Result<Option<OpenCodeSessionRecord>, StorageError> {
        self.conn
            .query_row(
                r#"
                SELECT project_id, issue_id, session_id, agent, model, worktree_path,
                       lifecycle_stage, stage, active_agent, active_model, message_count,
                       todo_count, part_count, token_count, cost_micros, subagent_count,
                       eval_stage, lifecycle_marker, last_event, silence_observed
                FROM opencode_sessions
                WHERE project_id = ?1 AND issue_id = ?2 AND session_id = ?3
                "#,
                params![project_id, issue_id, session_id],
                session_from_row,
            )
            .optional()
            .map_err(StorageError::from)
    }

    pub fn opencode_sessions_for_issue(
        &self,
        project_id: &str,
        issue_id: &str,
    ) -> Result<Vec<OpenCodeSessionRecord>, StorageError> {
        let mut statement = self.conn.prepare(
            r#"
            SELECT project_id, issue_id, session_id, agent, model, worktree_path,
                   lifecycle_stage, stage, active_agent, active_model, message_count,
                   todo_count, part_count, token_count, cost_micros, subagent_count,
                   eval_stage, lifecycle_marker, last_event, silence_observed
            FROM opencode_sessions
            WHERE project_id = ?1 AND issue_id = ?2
            ORDER BY session_id ASC
            "#,
        )?;
        let rows = statement.query_map(params![project_id, issue_id], session_from_row)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }

    pub fn upsert_opencode_stage_event(
        &self,
        event: OpenCodeStageEventRecord,
    ) -> Result<(), StorageError> {
        self.conn.execute(
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
                event.project_id,
                event.issue_id,
                event.session_id,
                event.sequence as i64,
                event.stage.as_str(),
                event.event
            ],
        )?;
        Ok(())
    }

    pub fn opencode_stage_events_for_session(
        &self,
        project_id: &str,
        issue_id: &str,
        session_id: &str,
    ) -> Result<Vec<OpenCodeStageEventRecord>, StorageError> {
        let mut statement = self.conn.prepare(
            r#"
            SELECT project_id, issue_id, session_id, sequence, stage, event
            FROM opencode_stage_events
            WHERE project_id = ?1 AND issue_id = ?2 AND session_id = ?3
            ORDER BY sequence ASC
            "#,
        )?;
        let rows = statement.query_map(
            params![project_id, issue_id, session_id],
            stage_event_from_row,
        )?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }

    pub fn upsert_eval_run(&self, eval: EvalRunRecord) -> Result<(), StorageError> {
        self.conn.execute(
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
                eval.project_id,
                eval.issue_id,
                eval.run_id,
                eval.suite,
                eval.status,
                eval.details_json
            ],
        )?;
        Ok(())
    }

    pub fn eval_runs_for_issue(
        &self,
        project_id: &str,
        issue_id: &str,
    ) -> Result<Vec<EvalRunRecord>, StorageError> {
        let mut statement = self.conn.prepare(
            r#"
            SELECT project_id, issue_id, run_id, suite, status, details_json
            FROM eval_runs
            WHERE project_id = ?1 AND issue_id = ?2
            ORDER BY run_id ASC
            "#,
        )?;
        let rows = statement.query_map(params![project_id, issue_id], eval_run_from_row)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }
}

fn project_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProjectStateRecord> {
    let lifecycle_stage: String = row.get(3)?;
    let cleanup_status: String = row.get(4)?;
    Ok(ProjectStateRecord {
        project_id: row.get(0)?,
        name: row.get(1)?,
        enabled: row.get::<_, i64>(2)? != 0,
        lifecycle_stage: parse_lifecycle(&lifecycle_stage)?,
        cleanup_status: parse_cleanup(&cleanup_status)?,
    })
}

fn issue_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<IssueStateRecord> {
    let lifecycle_stage: String = row.get(5)?;
    let blocker_json: Option<String> = row.get(6)?;
    let failure_json: Option<String> = row.get(7)?;
    let git_ref_json: Option<String> = row.get(8)?;
    let cleanup_status: String = row.get(9)?;

    Ok(IssueStateRecord {
        project_id: row.get(0)?,
        issue_id: row.get(1)?,
        identifier: row.get(2)?,
        title: row.get(3)?,
        state: row.get(4)?,
        lifecycle_stage: parse_lifecycle(&lifecycle_stage)?,
        blocker: decode_optional::<BlockerRecord>(blocker_json)?,
        failure: decode_optional::<FailureRecord>(failure_json)?,
        git_ref: decode_optional::<GitRefRecord>(git_ref_json)?,
        cleanup_status: parse_cleanup(&cleanup_status)?,
    })
}

fn session_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<OpenCodeSessionRecord> {
    let lifecycle_stage: String = row.get(6)?;
    let stage: String = row.get(7)?;
    Ok(OpenCodeSessionRecord {
        project_id: row.get(0)?,
        issue_id: row.get(1)?,
        session_id: row.get(2)?,
        agent: row.get(3)?,
        model: row.get(4)?,
        worktree_path: row.get(5)?,
        lifecycle_stage: parse_lifecycle(&lifecycle_stage)?,
        stage: parse_opencode_stage(&stage)?,
        active_agent: row.get(8)?,
        active_model: row.get(9)?,
        message_count: get_u64(row, 10)?,
        todo_count: get_u64(row, 11)?,
        part_count: get_u64(row, 12)?,
        token_count: get_u64(row, 13)?,
        cost_micros: get_u64(row, 14)?,
        subagent_count: get_u64(row, 15)?,
        eval_stage: row.get(16)?,
        lifecycle_marker: row.get(17)?,
        last_event: row.get(18)?,
        silence_observed: row.get::<_, i64>(19)? != 0,
    })
}

fn stage_event_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<OpenCodeStageEventRecord> {
    let stage: String = row.get(4)?;
    Ok(OpenCodeStageEventRecord {
        project_id: row.get(0)?,
        issue_id: row.get(1)?,
        session_id: row.get(2)?,
        sequence: get_u64(row, 3)?,
        stage: parse_opencode_stage(&stage)?,
        event: row.get(5)?,
    })
}

fn eval_run_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<EvalRunRecord> {
    Ok(EvalRunRecord {
        project_id: row.get(0)?,
        issue_id: row.get(1)?,
        run_id: row.get(2)?,
        suite: row.get(3)?,
        status: row.get(4)?,
        details_json: row.get(5)?,
    })
}

fn parse_lifecycle(input: &str) -> rusqlite::Result<LifecycleStage> {
    LifecycleStage::from_str(input).map_err(sql_conversion_error)
}

fn parse_cleanup(input: &str) -> rusqlite::Result<CleanupStatus> {
    CleanupStatus::from_str(input).map_err(sql_conversion_error)
}

fn parse_opencode_stage(input: &str) -> rusqlite::Result<OpenCodeStage> {
    OpenCodeStage::from_str(input).map_err(sql_conversion_error)
}

fn get_u64(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let value: i64 = row.get(index)?;
    Ok(value.max(0) as u64)
}

fn sql_conversion_error(error: StateParseError) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
}

fn encode_optional<T: Serialize>(value: &Option<T>) -> Result<Option<String>, StorageError> {
    value
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(StorageError::from)
}

fn decode_optional<T: DeserializeOwned>(value: Option<String>) -> rusqlite::Result<Option<T>> {
    value
        .map(|json| {
            serde_json::from_str(&json).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })
        })
        .transpose()
}

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("state serialization error: {0}")]
    Json(#[from] serde_json::Error),
}
