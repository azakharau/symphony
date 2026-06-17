use tracing::{info, warn};

use crate::{
    config::ProjectConfig,
    linear::LinearIssue,
    opencode::{
        OpenCodeLauncher, OpenCodeStartedSession, build_acp_launch_spec, terminate_process_tree,
    },
    state::{FailureRecord, LifecycleStage, OpenCodeSessionRecord, OpenCodeStage},
    storage::SqliteStore,
};

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
    if recoverable_opencode_failure(&failure) {
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
    session.stage = OpenCodeStage::Silent;
    session.lifecycle_marker = Some("waiting_for_blocker".into());
    session.last_event = Some("existing_session_waiting_for_blocker".into());
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

pub(super) async fn mark_issue_sessions_terminal(
    store: &SqliteStore,
    project: &ProjectConfig,
    issue: &LinearIssue,
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
            && session.last_event.as_deref() == Some("linear_terminal_reconciled")
            && !session.silence_observed
        {
            continue;
        }
        session.process_id = None;
        session.lifecycle_stage = LifecycleStage::Completed;
        session.stage = OpenCodeStage::Completed;
        session.lifecycle_marker = Some("linear_terminal_reconciled".into());
        session.last_event = Some("linear_terminal_reconciled".into());
        session.silence_observed = false;
        store.upsert_opencode_session(&session).await?;
        changed = true;
    }
    Ok(changed)
}

pub(super) async fn mark_existing_session_reactivated(
    store: &SqliteStore,
    project: &ProjectConfig,
    issue: &LinearIssue,
) -> anyhow::Result<()> {
    let Some(mut session) = latest_session_for_issue(store, &project.id, &issue.id).await? else {
        return Ok(());
    };
    terminate_current_session_process(project, issue, &mut session).await?;
    session.process_id = None;
    session.lifecycle_stage = LifecycleStage::Running;
    session.stage = OpenCodeStage::Running;
    session.lifecycle_marker = Some("existing_session_reactivated".into());
    if !session
        .last_event
        .as_deref()
        .is_some_and(|event| event.starts_with("stale_killed:"))
    {
        session.last_event = Some("existing_session_reactivated".into());
    }
    session.silence_observed = false;
    store.upsert_opencode_session(&session).await?;
    Ok(())
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
            session.lifecycle_stage == LifecycleStage::Running && !terminal_opencode_stage(session)
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
    matches!(
        session.lifecycle_stage,
        LifecycleStage::Running | LifecycleStage::Queued | LifecycleStage::Blocked
    ) && !terminal_opencode_stage(session)
}

fn terminal_opencode_stage(session: &OpenCodeSessionRecord) -> bool {
    matches!(
        session.stage,
        OpenCodeStage::Failed | OpenCodeStage::Completed
    )
}

pub(super) async fn session_requires_resume(session: &OpenCodeSessionRecord) -> bool {
    if session.lifecycle_stage != LifecycleStage::Running {
        return false;
    }
    let Some(process_id) = session.process_id else {
        return true;
    };
    !opencode_process_is_alive(process_id).await
}

pub(super) async fn session_has_live_process(session: &OpenCodeSessionRecord) -> bool {
    let Some(process_id) = session.process_id else {
        return false;
    };
    opencode_process_is_alive(process_id).await
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
    let output = tokio::process::Command::new("ps")
        .args(["-o", "etimes=", "-p", &process_id.to_string()])
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<u64>()
        .ok()
}

pub(super) async fn terminate_current_session_process(
    project: &ProjectConfig,
    issue: &LinearIssue,
    session: &mut OpenCodeSessionRecord,
) -> anyhow::Result<()> {
    let Some(process_id) = session.process_id else {
        return Ok(());
    };
    if !opencode_process_is_alive(process_id).await {
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
