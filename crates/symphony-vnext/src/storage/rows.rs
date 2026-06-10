use std::str::FromStr;

use libsql::Row;
use serde::{Serialize, de::DeserializeOwned};

use crate::{
    state::{
        BlockerRecord, CleanupStatus, EvalRunRecord, FailureRecord, GitRefRecord, IssueStateRecord,
        LifecycleStage, OpenCodeSessionRecord, OpenCodeStage, OpenCodeStageEventRecord,
        ProjectStateRecord,
    },
    storage::StorageError,
};

pub(super) async fn collect_rows<T>(
    rows: &mut libsql::Rows,
    mut map: impl FnMut(&Row) -> Result<T, StorageError>,
) -> Result<Vec<T>, StorageError> {
    let mut values = Vec::new();
    while let Some(row) = rows.next().await? {
        values.push(map(&row)?);
    }
    Ok(values)
}

pub(super) async fn optional_row<T>(
    rows: &mut libsql::Rows,
    mut map: impl FnMut(&Row) -> Result<T, StorageError>,
) -> Result<Option<T>, StorageError> {
    rows.next().await?.as_ref().map(&mut map).transpose()
}

pub(super) fn project_from_row(row: &Row) -> Result<ProjectStateRecord, StorageError> {
    let lifecycle_stage: String = row.get(3)?;
    let cleanup_status: String = row.get(4)?;
    Ok(ProjectStateRecord {
        project_id: row.get(0)?,
        name: row.get(1)?,
        enabled: row.get::<bool>(2)?,
        lifecycle_stage: parse_lifecycle(&lifecycle_stage)?,
        cleanup_status: parse_cleanup(&cleanup_status)?,
    })
}

pub(super) fn issue_from_row(row: &Row) -> Result<IssueStateRecord, StorageError> {
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

pub(super) fn session_from_row(row: &Row) -> Result<OpenCodeSessionRecord, StorageError> {
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
        silence_observed: row.get::<bool>(19)?,
    })
}

pub(super) fn stage_event_from_row(row: &Row) -> Result<OpenCodeStageEventRecord, StorageError> {
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

pub(super) fn eval_run_from_row(row: &Row) -> Result<EvalRunRecord, StorageError> {
    Ok(EvalRunRecord {
        project_id: row.get(0)?,
        issue_id: row.get(1)?,
        run_id: row.get(2)?,
        suite: row.get(3)?,
        status: row.get(4)?,
        details_json: row.get(5)?,
    })
}

pub(super) fn encode_optional<T: Serialize>(
    value: &Option<T>,
) -> Result<Option<String>, StorageError> {
    value
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(StorageError::from)
}

fn decode_optional<T: DeserializeOwned>(value: Option<String>) -> Result<Option<T>, StorageError> {
    value
        .map(|json| serde_json::from_str(&json))
        .transpose()
        .map_err(StorageError::from)
}

fn parse_lifecycle(input: &str) -> Result<LifecycleStage, StorageError> {
    LifecycleStage::from_str(input).map_err(StorageError::State)
}

fn parse_cleanup(input: &str) -> Result<CleanupStatus, StorageError> {
    CleanupStatus::from_str(input).map_err(StorageError::State)
}

fn parse_opencode_stage(input: &str) -> Result<OpenCodeStage, StorageError> {
    OpenCodeStage::from_str(input).map_err(StorageError::State)
}

fn get_u64(row: &Row, index: i32) -> Result<u64, StorageError> {
    let value: i64 = row.get(index)?;
    Ok(value.max(0) as u64)
}
