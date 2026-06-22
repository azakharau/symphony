use std::str::FromStr;

use libsql::Row;
use serde::{Serialize, de::DeserializeOwned};

use crate::{
    state::{
        BlockerRecord, CleanupStatus, EvalRunRecord, FailureRecord, GitRefRecord, IssueStateRecord,
        LifecycleStage, ProjectRuntimeLivenessRecord, ProjectStateRecord, RunnerSessionRecord,
        RunnerStage, RunnerStageEventRecord, RuntimeFailureKind, RuntimeLivenessStatus,
        RuntimeProviderMode,
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
    let lifecycle_stage: String = row.get(4)?;
    let blocker_json: Option<String> = row.get(5)?;
    let failure_json: Option<String> = row.get(6)?;
    let git_ref_json: Option<String> = row.get(7)?;
    let cleanup_status: String = row.get(8)?;

    Ok(IssueStateRecord {
        project_id: row.get(0)?,
        issue_id: row.get(1)?,
        identifier: row.get(2)?,
        title: row.get(3)?,
        lifecycle_stage: parse_lifecycle(&lifecycle_stage)?,
        blocker: decode_optional::<BlockerRecord>(blocker_json)?,
        failure: decode_optional::<FailureRecord>(failure_json)?,
        git_ref: decode_optional::<GitRefRecord>(git_ref_json)?,
        cleanup_status: parse_cleanup(&cleanup_status)?,
    })
}

pub(super) fn liveness_from_row(row: &Row) -> Result<ProjectRuntimeLivenessRecord, StorageError> {
    let status: String = row.get(1)?;
    Ok(ProjectRuntimeLivenessRecord {
        project_id: row.get(0)?,
        status: parse_liveness_status(&status)?,
        reason: row.get(2)?,
        last_poll_at: row.get(3)?,
        last_successful_candidate_scan_at: row.get(4)?,
        max_sessions: get_u64(row, 5)? as u32,
        running_sessions: get_u64(row, 6)? as u32,
        available_sessions: get_u64(row, 7)? as u32,
    })
}

pub(super) fn session_from_row(row: &Row) -> Result<RunnerSessionRecord, StorageError> {
    let provider_mode: String = row.get(3)?;
    let lifecycle_stage: String = row.get(9)?;
    let stage: String = row.get(10)?;
    let process_id = row
        .get::<Option<i64>>(8)?
        .and_then(|value| u32::try_from(value).ok());
    let runtime_failure_kind: Option<String> = row.get(22)?;
    let session_evidence_refs_json: Option<String> = row.get(24)?;
    Ok(RunnerSessionRecord {
        project_id: row.get(0)?,
        issue_id: row.get(1)?,
        session_id: row.get(2)?,
        provider_mode: RuntimeProviderMode::from_str(&provider_mode)
            .map_err(StorageError::State)?,
        provider_id: row.get(4)?,
        agent: row.get(5)?,
        model: row.get(6)?,
        worktree_path: row.get(7)?,
        process_id,
        lifecycle_stage: parse_lifecycle(&lifecycle_stage)?,
        stage: parse_runner_stage(&stage)?,
        active_agent: row.get(11)?,
        active_model: row.get(12)?,
        message_count: get_u64(row, 13)?,
        todo_count: get_u64(row, 14)?,
        part_count: get_u64(row, 15)?,
        token_count: get_u64(row, 16)?,
        cost_micros: get_u64(row, 17)?,
        subagent_count: get_u64(row, 18)?,
        eval_stage: row.get(19)?,
        lifecycle_marker: row.get(20)?,
        last_event: row.get(21)?,
        runtime_failure_kind: runtime_failure_kind
            .as_deref()
            .map(RuntimeFailureKind::from_str)
            .transpose()
            .map_err(StorageError::State)?,
        acp_frame_count: get_u64(row, 23)?,
        session_evidence_refs: session_evidence_refs_json
            .map(|json| serde_json::from_str(&json))
            .transpose()?
            .unwrap_or_default(),
        silence_observed: row.get::<bool>(25)?,
    })
}

pub(super) fn stage_event_from_row(row: &Row) -> Result<RunnerStageEventRecord, StorageError> {
    let stage: String = row.get(4)?;
    Ok(RunnerStageEventRecord {
        project_id: row.get(0)?,
        issue_id: row.get(1)?,
        session_id: row.get(2)?,
        sequence: get_u64(row, 3)?,
        stage: parse_runner_stage(&stage)?,
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

fn parse_runner_stage(input: &str) -> Result<RunnerStage, StorageError> {
    RunnerStage::from_str(input).map_err(StorageError::State)
}

fn parse_liveness_status(input: &str) -> Result<RuntimeLivenessStatus, StorageError> {
    RuntimeLivenessStatus::from_str(input).map_err(StorageError::State)
}

fn get_u64(row: &Row, index: i32) -> Result<u64, StorageError> {
    let value: i64 = row.get(index)?;
    Ok(value.max(0) as u64)
}
