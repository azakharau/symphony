use tracing::{info, warn};

use crate::{
    config::ProjectConfig,
    linear::LinearIssue,
    opencode::{
        OMP_CLEANUP_MARKER_ENV, OpenCodeLauncher, OpenCodeStartedSession, build_acp_launch_spec,
        terminate_process_tree,
    },
    state::{FailureRecord, LifecycleStage, OpenCodeSessionRecord, OpenCodeStage},
    storage::SqliteStore,
};

use std::path::Path;

use super::policy::recoverable_opencode_failure;

pub(super) async fn mark_historical_sessions_ignored(
    store: &SqliteStore,
    project: &ProjectConfig,
    issue: &LinearIssue,
) -> anyhow::Result<()> {
    for mut session in store
        .opencode_sessions_for_issue(&project.id, &issue.id)
        .await?
    {
        if matches!(
            session.lifecycle_stage,
            LifecycleStage::Failed | LifecycleStage::Canceled | LifecycleStage::Completed
        ) || matches!(
            session.stage,
            OpenCodeStage::Failed | OpenCodeStage::Completed
        ) {
            session.process_id = None;
            session.last_event = Some("stale_failed_session_ignored".into());
            store.upsert_opencode_session(&session).await?;
        }
    }
    Ok(())
}

pub(super) async fn mark_existing_session_queued(
    store: &SqliteStore,
    project: &ProjectConfig,
    issue: &LinearIssue,
) -> anyhow::Result<()> {
    let Some(mut session) = latest_session_for_issue(store, &project.id, &issue.id).await? else {
        return Ok(());
    };
    terminate_current_session_process(project, issue, &mut session).await?;
    session.process_id = None;
    session.lifecycle_stage = LifecycleStage::Queued;
    session.stage = OpenCodeStage::Silent;
    session.lifecycle_marker = Some("waiting_for_capacity".into());
    if !session
        .last_event
        .as_deref()
        .is_some_and(|event| event.starts_with("stale_killed:"))
    {
        session.last_event = Some("existing_session_waiting_for_capacity".into());
    }
    session.silence_observed = false;
    store.upsert_opencode_session(&session).await?;
    Ok(())
}

pub(super) async fn unresolved_runtime_defect(
    store: &SqliteStore,
    project: &ProjectConfig,
    issue: &LinearIssue,
) -> anyhow::Result<Option<FailureRecord>> {
    let Some(record) = store.issue(&project.id, &issue.id).await? else {
        return Ok(None);
    };
    let Some(failure) = record.failure else {
        return Ok(None);
    };
    let has_unaccepted_blocker = issue.blocked_by.iter().any(is_unaccepted_blocker);
    let reached_recoverable_failure_threshold = recoverable_opencode_failure(&failure)
        && failure.occurrence_count >= project.eval.max_identical_failure_fingerprints.max(1);
    if !has_unaccepted_blocker && !reached_recoverable_failure_threshold {
        return Ok(None);
    }
    if !matches!(
        failure.kind.as_str(),
        "malformed_handoff" | "runtime_defect"
    ) {
        return Ok(None);
    }
    Ok(Some(failure))
}

fn is_unaccepted_blocker(blocker: &crate::linear::LinearBlocker) -> bool {
    !matches!(blocker.state.as_deref(), Some("Done"))
}

pub(super) async fn mark_existing_session_blocked(
    store: &SqliteStore,
    project: &ProjectConfig,
    issue: &LinearIssue,
) -> anyhow::Result<()> {
    let Some(mut session) = latest_session_for_issue(store, &project.id, &issue.id).await? else {
        return Ok(());
    };
    terminate_current_session_process(project, issue, &mut session).await?;
    session.process_id = None;
    session.lifecycle_stage = LifecycleStage::Queued;
    if session.stage != OpenCodeStage::Failed {
        session.stage = OpenCodeStage::Silent;
        session.lifecycle_marker = Some("waiting_for_blocker".into());
        session.last_event = Some("existing_session_waiting_for_blocker".into());
    }
    session.silence_observed = false;
    store.upsert_opencode_session(&session).await?;
    Ok(())
}

pub(super) async fn mark_existing_session_waiting_for_project_owner_input(
    store: &SqliteStore,
    project: &ProjectConfig,
    issue: &LinearIssue,
) -> anyhow::Result<()> {
    let Some(mut session) = latest_session_for_issue(store, &project.id, &issue.id).await? else {
        return Ok(());
    };
    terminate_current_session_process(project, issue, &mut session).await?;
    session.process_id = None;
    session.lifecycle_stage = LifecycleStage::Queued;
    session.stage = OpenCodeStage::Silent;
    session.lifecycle_marker = Some("waiting_for_project_owner_input".into());
    session.last_event = Some("existing_session_waiting_for_project_owner_input".into());
    session.silence_observed = false;
    store.upsert_opencode_session(&session).await?;
    Ok(())
}

pub(super) async fn mark_existing_session_failed_for_unresolved_runtime_defect(
    store: &SqliteStore,
    project: &ProjectConfig,
    issue: &LinearIssue,
) -> anyhow::Result<()> {
    let Some(mut session) = latest_session_for_issue(store, &project.id, &issue.id).await? else {
        return Ok(());
    };
    terminate_current_session_process(project, issue, &mut session).await?;
    session.process_id = None;
    session.lifecycle_stage = LifecycleStage::Failed;
    session.stage = OpenCodeStage::Failed;
    session
        .lifecycle_marker
        .get_or_insert_with(|| "failed:unresolved_runtime_defect".into());
    session
        .last_event
        .get_or_insert_with(|| "failed:unresolved_runtime_defect".into());
    session.silence_observed = false;
    store.upsert_opencode_session(&session).await?;
    Ok(())
}

pub(super) async fn mark_existing_session_resume_failed(
    store: &SqliteStore,
    project: &ProjectConfig,
    issue: &LinearIssue,
    reason: &str,
) -> anyhow::Result<()> {
    let Some(mut session) = latest_active_session_for_issue(store, &project.id, &issue.id).await?
    else {
        return Ok(());
    };
    terminate_current_session_process(project, issue, &mut session).await?;
    session.process_id = None;
    session.lifecycle_stage = LifecycleStage::Failed;
    session.stage = OpenCodeStage::Failed;
    session.lifecycle_marker = Some("failed:resume_launch_failed".into());
    session.last_event = Some(format!("failed:resume_launch_failed:{reason}"));
    session.silence_observed = false;
    store.upsert_opencode_session(&session).await?;
    Ok(())
}

pub(super) async fn mark_issue_sessions_terminal(
    store: &SqliteStore,
    project: &ProjectConfig,
    issue: &LinearIssue,
    lifecycle_stage: LifecycleStage,
) -> anyhow::Result<bool> {
    let mut changed = false;
    for mut session in store
        .opencode_sessions_for_issue(&project.id, &issue.id)
        .await?
    {
        if session.process_id.is_none()
            && session.lifecycle_stage == LifecycleStage::Completed
            && session.stage == OpenCodeStage::Completed
            && session.lifecycle_marker.as_deref() == Some("linear_terminal_reconciled")
            && terminal_reconciliation_event_is_stable(session.last_event.as_deref())
            && !session.silence_observed
        {
            continue;
        }
        let previous_process_id = session.process_id;
        terminate_current_session_process(project, issue, &mut session).await?;
        let terminal_event =
            terminal_reconciliation_event(previous_process_id, session.last_event.as_deref());
        session.process_id = None;
        session.lifecycle_stage = lifecycle_stage;
        session.stage = OpenCodeStage::Completed;
        session.lifecycle_marker = Some("linear_terminal_reconciled".into());
        session.last_event = Some(terminal_event);
        session.silence_observed = false;
        store.upsert_opencode_session(&session).await?;
        changed = true;
    }
    Ok(changed)
}

fn terminal_reconciliation_event(
    previous_process_id: Option<u32>,
    last_event: Option<&str>,
) -> String {
    match (previous_process_id, last_event) {
        (Some(_), Some(event)) if event.starts_with("stale_killed:") => {
            format!("linear_terminal_reconciled:{event}")
        }
        _ => "linear_terminal_reconciled".into(),
    }
}

fn terminal_reconciliation_event_is_stable(last_event: Option<&str>) -> bool {
    last_event.is_some_and(|event| {
        event == "linear_terminal_reconciled"
            || event.starts_with("linear_terminal_reconciled:stale_killed:")
    })
}

pub(super) async fn resume_stale_opencode_session(
    project: &ProjectConfig,
    store: &SqliteStore,
    opencode: &impl OpenCodeLauncher,
    issue: &LinearIssue,
) -> anyhow::Result<()> {
    let Some(mut session) = latest_active_session_for_issue(store, &project.id, &issue.id).await?
    else {
        return Ok(());
    };
    if !session_requires_resume(&session).await {
        return Ok(());
    }
    let launch_spec = build_acp_launch_spec(project, issue);
    let existing_issue = store.issue(&project.id, &issue.id).await?;
    terminate_current_session_process(project, issue, &mut session).await?;
    if let Some(failure) = existing_issue
        .as_ref()
        .and_then(|record| record.failure.as_ref())
        .filter(|failure| recoverable_opencode_failure(failure))
    {
        let started =
            continue_failed_session_repair(opencode, &launch_spec, &session, failure).await?;
        apply_repair_process(&mut session, started, failure);
    } else {
        let started = continue_stale_session(opencode, &launch_spec, &session).await?;
        apply_continued_process(&mut session, started);
    }
    info!(
        project_id = %project.id,
        issue = %issue.identifier,
        session_id = %session.session_id,
        process_id = session.process_id,
        "OpenCode ACP session resumed"
    );
    store.upsert_opencode_session(&session).await?;
    Ok(())
}

async fn latest_session_for_issue(
    store: &SqliteStore,
    project_id: &str,
    issue_id: &str,
) -> anyhow::Result<Option<OpenCodeSessionRecord>> {
    Ok(store
        .opencode_sessions_for_issue(project_id, issue_id)
        .await?
        .pop())
}

async fn latest_active_session_for_issue(
    store: &SqliteStore,
    project_id: &str,
    issue_id: &str,
) -> anyhow::Result<Option<OpenCodeSessionRecord>> {
    let mut sessions: Vec<_> = store
        .opencode_sessions_for_issue(project_id, issue_id)
        .await?
        .into_iter()
        .filter(reusable_session_record)
        .collect();
    Ok(sessions.pop())
}

pub(super) async fn latest_running_session_for_issue(
    store: &SqliteStore,
    project_id: &str,
    issue_id: &str,
) -> anyhow::Result<Option<OpenCodeSessionRecord>> {
    let mut sessions: Vec<_> = store
        .opencode_sessions_for_issue(project_id, issue_id)
        .await?
        .into_iter()
        .filter(|session| {
            session.lifecycle_stage == LifecycleStage::Running
                && !failed_or_completed_stage(session)
        })
        .collect();
    Ok(sessions.pop())
}

pub(super) async fn has_reusable_existing_session(
    store: &SqliteStore,
    project_id: &str,
    issue_id: &str,
) -> anyhow::Result<bool> {
    Ok(latest_active_session_for_issue(store, project_id, issue_id)
        .await?
        .is_some())
}

fn reusable_session_record(session: &OpenCodeSessionRecord) -> bool {
    (matches!(
        session.lifecycle_stage,
        LifecycleStage::Running | LifecycleStage::Queued | LifecycleStage::Blocked
    ) || retired_provider_blocker_retry_session(session))
        && !terminal_completed_stage(session)
        && !non_reusable_failed_launch_session(session)
        && !non_reusable_failed_handoff_session(session)
}

fn terminal_completed_stage(session: &OpenCodeSessionRecord) -> bool {
    matches!(session.stage, OpenCodeStage::Completed)
}

fn non_reusable_failed_launch_session(session: &OpenCodeSessionRecord) -> bool {
    if session.stage != OpenCodeStage::Failed {
        return false;
    }
    let marker = session.lifecycle_marker.as_deref().unwrap_or_default();
    marker == "failed:launch_failed" || marker == "failed:resume_launch_failed"
}

fn non_reusable_failed_handoff_session(session: &OpenCodeSessionRecord) -> bool {
    if session.stage != OpenCodeStage::Failed {
        return false;
    }
    let marker = session.lifecycle_marker.as_deref().unwrap_or_default();
    let last_event = session.last_event.as_deref().unwrap_or_default();
    marker == "failed:malformed_handoff"
        || marker == "failed:runtime_defect"
        || last_event.starts_with("failed:missing_handoff_sidecar")
        || last_event.starts_with("failed:malformed_handoff_sidecar")
}

fn retired_provider_blocker_retry_session(session: &OpenCodeSessionRecord) -> bool {
    session.lifecycle_stage == LifecycleStage::Canceled
        && session.stage == OpenCodeStage::Failed
        && session.lifecycle_marker.as_deref() == Some("retry_retired_provider_blocker")
}

fn failed_or_completed_stage(session: &OpenCodeSessionRecord) -> bool {
    matches!(
        session.stage,
        OpenCodeStage::Failed | OpenCodeStage::Completed
    )
}

pub(super) async fn session_requires_resume(session: &OpenCodeSessionRecord) -> bool {
    if !matches!(
        session.lifecycle_stage,
        LifecycleStage::Running
            | LifecycleStage::Queued
            | LifecycleStage::Blocked
            | LifecycleStage::Failed
    ) {
        return false;
    }
    let Some(process_id) = session.process_id else {
        return true;
    };
    if !session_process_is_alive(session, process_id).await {
        return true;
    }
    false
}

pub(super) async fn session_has_live_process(session: &OpenCodeSessionRecord) -> bool {
    let Some(process_id) = session.process_id else {
        return false;
    };
    session_process_is_alive(session, process_id).await
}

async fn session_process_is_alive(session: &OpenCodeSessionRecord, process_id: u32) -> bool {
    match session.provider_mode {
        crate::state::RuntimeProviderMode::OpenCodeAcp => {
            opencode_process_is_alive(process_id).await
        }
        crate::state::RuntimeProviderMode::OmpAcp => {
            omp_acp_process_is_alive(session, process_id).await
        }
    }
}

async fn omp_acp_process_is_alive(session: &OpenCodeSessionRecord, process_id: u32) -> bool {
    if session.provider_id.is_none() {
        return false;
    }

    let Ok(environ) = tokio::fs::read(format!("/proc/{process_id}/environ")).await else {
        return false;
    };
    environ.split(|byte| *byte == 0).any(|entry| {
        entry == format!("SYMPHONY_ISSUE_WORKTREE={}", session.worktree_path).as_bytes()
    })
}

async fn session_process_matches_cleanup_owner(
    project: &ProjectConfig,
    issue: &LinearIssue,
    session: &OpenCodeSessionRecord,
    process_id: u32,
) -> bool {
    match session.provider_mode {
        crate::state::RuntimeProviderMode::OpenCodeAcp => {
            opencode_process_is_alive(process_id).await
        }
        crate::state::RuntimeProviderMode::OmpAcp => {
            omp_acp_process_matches_cleanup_owner(project, issue, session, process_id).await
        }
    }
}

async fn omp_acp_process_matches_cleanup_owner(
    project: &ProjectConfig,
    issue: &LinearIssue,
    session: &OpenCodeSessionRecord,
    process_id: u32,
) -> bool {
    if session.provider_id.is_none() {
        return false;
    }

    let Ok(environ) = tokio::fs::read(format!("/proc/{process_id}/environ")).await else {
        return false;
    };
    let issue_worktree = project.branch.worktree_root.join(&issue.identifier);
    omp_acp_environ_matches_cleanup_owner(&issue_worktree, &issue.identifier, session, &environ)
}

fn omp_acp_environ_matches_cleanup_owner(
    issue_worktree: &Path,
    issue_identifier: &str,
    session: &OpenCodeSessionRecord,
    environ: &[u8],
) -> bool {
    let Some(provider_id) = session.provider_id.as_deref() else {
        return false;
    };
    let expected_marker = format!(
        "{OMP_CLEANUP_MARKER_ENV}=provider={provider_id};issue={issue_identifier};cwd={}",
        session.worktree_path
    );
    if environ
        .split(|byte| *byte == 0)
        .any(|entry| entry == expected_marker.as_bytes())
    {
        return true;
    }

    if session.worktree_path != issue_worktree.display().to_string() {
        return false;
    }
    environ.split(|byte| *byte == 0).any(|entry| {
        entry == format!("SYMPHONY_ISSUE_WORKTREE={}", session.worktree_path).as_bytes()
    })
}

async fn opencode_process_is_alive(process_id: u32) -> bool {
    let path = format!("/proc/{process_id}/cmdline");
    let Ok(cmdline) = tokio::fs::read(path).await else {
        return false;
    };
    cmdline
        .split(|byte| *byte == 0)
        .filter_map(|part| std::str::from_utf8(part).ok())
        .any(|part| part.contains("opencode"))
}

pub(super) async fn process_elapsed_seconds(process_id: u32) -> Option<u64> {
    let uptime_seconds = process_uptime_seconds(process_id).await?;
    let system_uptime = tokio::fs::read_to_string("/proc/uptime").await.ok()?;
    let system_uptime_seconds = system_uptime
        .split_whitespace()
        .next()
        .and_then(|value| value.parse::<f64>().ok())?;
    if system_uptime_seconds < uptime_seconds {
        return None;
    }
    Some((system_uptime_seconds - uptime_seconds).floor() as u64)
}

async fn process_uptime_seconds(process_id: u32) -> Option<f64> {
    let stat = tokio::fs::read_to_string(format!("/proc/{process_id}/stat"))
        .await
        .ok()?;
    let start_time_ticks = process_start_time_ticks(&stat)?;
    let ticks_per_second = clock_ticks_per_second().await?;
    Some(start_time_ticks as f64 / ticks_per_second as f64)
}

fn process_start_time_ticks(stat: &str) -> Option<u64> {
    let after_command = stat.rsplit_once(") ")?.1;
    after_command.split_whitespace().nth(19)?.parse().ok()
}

async fn clock_ticks_per_second() -> Option<u64> {
    let output = tokio::process::Command::new("getconf")
        .arg("CLK_TCK")
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

pub(super) async fn terminate_current_session_process(
    project: &ProjectConfig,
    issue: &LinearIssue,
    session: &mut OpenCodeSessionRecord,
) -> anyhow::Result<()> {
    let Some(process_id) = session.process_id else {
        return Ok(());
    };
    if !session_process_matches_cleanup_owner(project, issue, session, process_id).await {
        return Ok(());
    }
    info!(
        project_id = %project.id,
        issue = %issue.identifier,
        session_id = %session.session_id,
        process_id,
        "terminating stale OpenCode ACP process before session continuation"
    );
    let evidence =
        terminate_process_tree(process_id, "stale_opencode_session_continuation").await?;
    info!(
        project_id = %project.id,
        issue = %issue.identifier,
        session_id = %session.session_id,
        process_id = evidence.root_process_id,
        descendant_process_ids = ?evidence.descendant_process_ids,
        term_signal_sent = evidence.term_signal_sent,
        kill_signal_sent = evidence.kill_signal_sent,
        still_alive = evidence.still_alive,
        reason = %evidence.reason,
        "stale OpenCode ACP process tree termination evidence"
    );
    if evidence.still_alive {
        warn!(
            project_id = %project.id,
            issue = %issue.identifier,
            session_id = %session.session_id,
            process_id,
            "stale OpenCode ACP process tree was still alive after termination attempts"
        );
    }
    session.last_event = Some(format!(
        "stale_killed:{process_id}:term={}:kill={}:alive={}",
        evidence.term_signal_sent, evidence.kill_signal_sent, evidence.still_alive
    ));
    Ok(())
}

async fn continue_stale_session(
    opencode: &impl OpenCodeLauncher,
    spec: &crate::opencode::OpenCodeLaunchSpec,
    session: &OpenCodeSessionRecord,
) -> anyhow::Result<OpenCodeStartedSession> {
    Ok(opencode
        .continue_session(
            spec,
            session,
            "The previous ACP stdio process ended or was killed while this Linear issue was still In Progress. Inspect the current repository/session state, continue the remaining work in this same session, and write the structured Symphony handoff JSON when done.",
        )
        .await?)
}

fn apply_continued_process(session: &mut OpenCodeSessionRecord, started: OpenCodeStartedSession) {
    session.process_id = started.process_id;
    session.lifecycle_stage = LifecycleStage::Running;
    session.stage = OpenCodeStage::Running;
    session.lifecycle_marker = Some("continuation_prompted".into());
    if !session
        .last_event
        .as_deref()
        .is_some_and(|event| event.starts_with("stale_killed:"))
    {
        session.last_event = started
            .process_id
            .map(|process_id| format!("continuation_prompted:{process_id}"))
            .or_else(|| Some("continuation_prompted".into()));
    }
    session.silence_observed = false;
}

async fn continue_failed_session_repair(
    opencode: &impl OpenCodeLauncher,
    spec: &crate::opencode::OpenCodeLaunchSpec,
    session: &OpenCodeSessionRecord,
    failure: &FailureRecord,
) -> anyhow::Result<OpenCodeStartedSession> {
    let fingerprint = failure
        .fingerprint
        .as_deref()
        .unwrap_or(failure.kind.as_str());
    Ok(opencode
        .continue_repair(spec, session, fingerprint, &failure.message)
        .await?)
}

fn apply_repair_process(
    session: &mut OpenCodeSessionRecord,
    started: OpenCodeStartedSession,
    failure: &FailureRecord,
) {
    let fingerprint = failure
        .fingerprint
        .as_deref()
        .unwrap_or(failure.kind.as_str());
    session.process_id = started.process_id;
    session.lifecycle_stage = LifecycleStage::Running;
    session.stage = OpenCodeStage::Running;
    session.lifecycle_marker = Some("repair_prompted".into());
    session.last_event = Some(format!("repair_prompted:{fingerprint}"));
    session.silence_observed = false;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{LifecycleStage, RuntimeProviderMode};

    fn omp_session(worktree_path: &Path) -> OpenCodeSessionRecord {
        OpenCodeSessionRecord {
            project_id: "project".into(),
            issue_id: "issue".into(),
            session_id: "session".into(),
            provider_mode: RuntimeProviderMode::OmpAcp,
            provider_id: Some("omp-primary".into()),
            agent: "implementer".into(),
            model: None,
            worktree_path: worktree_path.display().to_string(),
            process_id: Some(123),
            lifecycle_stage: LifecycleStage::Running,
            stage: OpenCodeStage::Starting,
            active_agent: Some("implementer".into()),
            active_model: None,
            message_count: 0,
            todo_count: 0,
            part_count: 0,
            token_count: 0,
            cost_micros: 0,
            subagent_count: 0,
            eval_stage: None,
            lifecycle_marker: None,
            last_event: None,
            runtime_failure_kind: None,
            acp_frame_count: 0,
            session_evidence_refs: Vec::new(),
            silence_observed: false,
        }
    }

    #[test]
    fn project_repo_omp_cleanup_marker_must_match_issue_owner() {
        let repo_path = Path::new("/repo/shared");
        let issue_worktree = Path::new("/worktrees/SYM-102");
        let session = omp_session(repo_path);
        let other_issue_environ = format!(
            "SYMPHONY_ISSUE_WORKTREE=/repo/shared\0{OMP_CLEANUP_MARKER_ENV}=provider=omp-primary;issue=SYM-103;cwd=/repo/shared\0"
        );

        assert!(!omp_acp_environ_matches_cleanup_owner(
            issue_worktree,
            "SYM-102",
            &session,
            other_issue_environ.as_bytes(),
        ));

        let owned_environ = format!(
            "SYMPHONY_ISSUE_WORKTREE=/repo/shared\0{OMP_CLEANUP_MARKER_ENV}=provider=omp-primary;issue=SYM-102;cwd=/repo/shared\0"
        );
        assert!(omp_acp_environ_matches_cleanup_owner(
            issue_worktree,
            "SYM-102",
            &session,
            owned_environ.as_bytes(),
        ));
    }
}
