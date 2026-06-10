use std::{path::Path, str::FromStr};

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Serialize, de::DeserializeOwned};
use thiserror::Error;

use crate::{
    config::RootConfig,
    state::{
        BlockerRecord, CleanupStatus, FailureRecord, GitRefRecord, IssueStateRecord,
        LifecycleStage, OpenCodeSessionRecord, ProjectStateRecord, StateParseError,
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
                lifecycle_stage,
                last_event
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ON CONFLICT(project_id, issue_id, session_id) DO UPDATE SET
                agent = excluded.agent,
                model = excluded.model,
                lifecycle_stage = excluded.lifecycle_stage,
                last_event = excluded.last_event
            "#,
            params![
                session.project_id,
                session.issue_id,
                session.session_id,
                session.agent,
                session.model,
                session.lifecycle_stage.as_str(),
                session.last_event
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
                SELECT project_id, issue_id, session_id, agent, model, lifecycle_stage, last_event
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
            SELECT project_id, issue_id, session_id, agent, model, lifecycle_stage, last_event
            FROM opencode_sessions
            WHERE project_id = ?1 AND issue_id = ?2
            ORDER BY session_id ASC
            "#,
        )?;
        let rows = statement.query_map(params![project_id, issue_id], session_from_row)?;
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
    let lifecycle_stage: String = row.get(5)?;
    Ok(OpenCodeSessionRecord {
        project_id: row.get(0)?,
        issue_id: row.get(1)?,
        session_id: row.get(2)?,
        agent: row.get(3)?,
        model: row.get(4)?,
        lifecycle_stage: parse_lifecycle(&lifecycle_stage)?,
        last_event: row.get(6)?,
    })
}

fn parse_lifecycle(input: &str) -> rusqlite::Result<LifecycleStage> {
    LifecycleStage::from_str(input).map_err(sql_conversion_error)
}

fn parse_cleanup(input: &str) -> rusqlite::Result<CleanupStatus> {
    CleanupStatus::from_str(input).map_err(sql_conversion_error)
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
