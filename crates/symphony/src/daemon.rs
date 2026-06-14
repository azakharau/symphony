mod cleanup;
mod git_closure;
mod handoff;
mod http;
mod liveness;
mod policy;
mod records;

use std::path::PathBuf;

use anyhow::Context;
use tokio::time::{Duration, sleep};
use tracing::{debug, info, warn};

use crate::{
    config::{OpenCodeStorageConfig, ProjectConfig, RootConfig},
    linear::{EmptyLinearClient, LinearClient, LinearIssue, LinearIssueEvidence, LinearTransition},
    opencode::{
        DeterministicOpenCodeLauncher, OpenCodeLauncher, OpenCodeStartedSession,
        StdioOpenCodeLauncher, apply_session_tree_metrics_preserving_marker, build_acp_launch_spec,
        new_session_record, read_session_tree_metrics,
    },
    state::{
        BlockerRecord, CleanupStatus, FailureRecord, LifecycleStage, OpenCodeSessionRecord,
        OpenCodeStage,
    },
    storage::SqliteStore,
};
use handoff::process_in_progress_handoff;
use http::run_continuous;
use liveness::project_liveness_projection;
use policy::{
    blocker_record, compare_issues_for_dispatch, has_existing_session, has_new_owner_response,
    is_terminal_state, recoverable_opencode_failure, unaccepted_blocker,
};
use records::issue_record;

#[derive(Debug)]
pub struct DaemonOptions {
    pub config_path: PathBuf,
    pub database_path: PathBuf,
    pub once: bool,
}

pub async fn run(options: DaemonOptions) -> anyhow::Result<()> {
    let input = tokio::fs::read_to_string(&options.config_path)
        .await
        .with_context(|| format!("read config {}", options.config_path.display()))?;
    let config = RootConfig::from_toml_str(&input)?;
    info!(
        config_path = %options.config_path.display(),
        database_path = %options.database_path.display(),
        projects = config.projects().len(),
        once = options.once,
        "Symphony daemon starting"
    );
    let store = SqliteStore::open(&options.database_path)
        .await
        .with_context(|| format!("open sqlite database {}", options.database_path.display()))?;
    store.migrate().await?;
    store.reconcile_projects(&config).await?;

    if options.once {
        run_once_with_clients(&config, &store, &EmptyLinearClient, &StdioOpenCodeLauncher).await?;
        return Ok(());
    }

    run_continuous(config, options.database_path).await?;

    Ok(())
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OrchestrationReport {
    pub dispatched: Vec<String>,
    pub blocked: Vec<String>,
    pub parked_owner_input: Vec<String>,
    pub terminal_reconciled: Vec<String>,
}

#[derive(Debug)]
enum DispatchCandidate {
    New(LinearIssue),
    ExistingSession(LinearIssue),
}

impl DispatchCandidate {
    fn issue(&self) -> &LinearIssue {
        match self {
            Self::New(issue) | Self::ExistingSession(issue) => issue,
        }
    }
}

pub async fn run_once_with_linear_client(
    config: &RootConfig,
    store: &SqliteStore,
    linear: &impl LinearClient,
) -> anyhow::Result<OrchestrationReport> {
    run_once_with_clients(config, store, linear, &DeterministicOpenCodeLauncher).await
}

pub async fn run_once_with_clients(
    config: &RootConfig,
    store: &SqliteStore,
    linear: &impl LinearClient,
    opencode: &impl OpenCodeLauncher,
) -> anyhow::Result<OrchestrationReport> {
    store.reconcile_projects(config).await?;

    let mut report = OrchestrationReport::default();
    for project in config.projects().iter().filter(|project| project.enabled) {
        reconcile_project(
            project,
            config.opencode_storage.as_ref(),
            store,
            linear,
            opencode,
            &mut report,
        )
        .await
        .with_context(|| format!("orchestrate project `{}`", project.id))?;
    }

    Ok(report)
}

async fn reconcile_project(
    project: &ProjectConfig,
    opencode_storage: Option<&OpenCodeStorageConfig>,
    store: &SqliteStore,
    linear: &impl LinearClient,
    opencode: &impl OpenCodeLauncher,
    report: &mut OrchestrationReport,
) -> anyhow::Result<()> {
    let mut eligible = Vec::new();
    let mut issues = linear.fetch_candidate_issues(project).await?;
    issues.sort_by(compare_issues_for_dispatch);
    let has_unanswered_owner_input = issues
        .iter()
        .any(|issue| issue.state == "Need Owner Input" && !issue.has_new_owner_answer);
    let active_runnable_todo_milestone = active_runnable_todo_milestone(&issues);
    let runnable_todo_milestone_count = runnable_todo_milestone_count(&issues);
    debug!(
        project_id = %project.id,
        active_runnable_todo_milestone = active_runnable_todo_milestone.as_deref().unwrap_or("none"),
        runnable_todo_milestone_count,
        issues = issues.len(),
        "fetched Linear candidate issues"
    );
    if has_unanswered_owner_input {
        info!(
            project_id = %project.id,
            "unanswered Need Owner Input blocks project dispatch"
        );
    }
    if runnable_todo_milestone_count > 1 {
        info!(
            project_id = %project.id,
            runnable_todo_milestone_count,
            "Runnable Todo queue spans multiple Linear milestones; dispatch is suppressed until unblocked Todo contains one active milestone"
        );
    }
    for issue in issues {
        match issue.state.as_str() {
            "Backlog" => {
                if store.issue(&project.id, &issue.id).await?.is_some() {
                    debug!(
                        project_id = %project.id,
                        issue = %issue.identifier,
                        "recording known issue returned to Backlog"
                    );
                    let record = issue_record(
                        project,
                        &issue,
                        LifecycleStage::Queued,
                        None,
                        CleanupStatus::Clean,
                    );
                    store.upsert_issue(&record).await?;
                }
            }
            state if is_terminal_state(state) => {
                let existing = store.issue(&project.id, &issue.id).await?;
                let mut record = issue_record(
                    project,
                    &issue,
                    LifecycleStage::Completed,
                    None,
                    CleanupStatus::Pending,
                );
                if let Some(existing) = &existing {
                    record.git_ref.clone_from(&existing.git_ref);
                    if existing.cleanup_status == CleanupStatus::Complete {
                        record.cleanup_status = CleanupStatus::Complete;
                    } else if let Some(git_ref) = &record.git_ref
                        && !tokio::fs::try_exists(&git_ref.worktree_path).await?
                    {
                        record.cleanup_status = CleanupStatus::Complete;
                    }
                }
                let issue_changed = existing.as_ref() != Some(&record);
                if issue_changed {
                    store.upsert_issue(&record).await?;
                }
                let sessions_changed = mark_issue_sessions_terminal(store, project, &issue).await?;
                if issue_changed || sessions_changed {
                    info!(
                        project_id = %project.id,
                        issue = %issue.identifier,
                        state,
                        cleanup = ?record.cleanup_status,
                        "terminal issue reconciled"
                    );
                    report.terminal_reconciled.push(issue.identifier);
                }
            }
            "Need Owner Input" => {
                let existing = store.issue(&project.id, &issue.id).await?;
                if has_new_owner_response(existing.as_ref(), &issue) {
                    info!(
                        project_id = %project.id,
                        issue = %issue.identifier,
                        "new owner response observed; returning issue to Todo"
                    );
                    linear
                        .transition_issue(&issue.id, LinearTransition::Todo)
                        .await?;
                    let record = issue_record(
                        project,
                        &issue,
                        LifecycleStage::Queued,
                        None,
                        CleanupStatus::Clean,
                    );
                    store.upsert_issue(&record).await?;
                } else {
                    debug!(
                        project_id = %project.id,
                        issue = %issue.identifier,
                        "issue remains parked waiting for owner input"
                    );
                    let record = issue_record(
                        project,
                        &issue,
                        LifecycleStage::Blocked,
                        Some(BlockerRecord {
                            kind: "owner_input".into(),
                            message: "waiting for owner-visible answer".into(),
                            observed_at: issue.updated_at.clone(),
                        }),
                        CleanupStatus::Clean,
                    );
                    store.upsert_issue(&record).await?;
                    report.parked_owner_input.push(issue.identifier);
                }
            }
            "In Progress" => {
                if has_unanswered_owner_input {
                    info!(
                        project_id = %project.id,
                        issue = %issue.identifier,
                        "pausing in-progress issue because project has unanswered Need Owner Input"
                    );
                    linear
                        .transition_issue(&issue.id, LinearTransition::Todo)
                        .await?;
                    let record = issue_record(
                        project,
                        &issue,
                        LifecycleStage::Queued,
                        Some(BlockerRecord {
                            kind: "project_owner_input".into(),
                            message: "project has an unanswered Need Owner Input issue".into(),
                            observed_at: issue.updated_at.clone(),
                        }),
                        CleanupStatus::Clean,
                    );
                    store.upsert_issue(&record).await?;
                    continue;
                }
                if let Some(blocker) = unaccepted_blocker(&issue.blocked_by) {
                    info!(
                        project_id = %project.id,
                        issue = %issue.identifier,
                        blocker_id = blocker.id.as_deref().unwrap_or("unknown"),
                        blocker_state = blocker.state.as_deref().unwrap_or("unknown"),
                        "pausing in-progress issue because Linear blocker is not accepted"
                    );
                    linear
                        .transition_issue(&issue.id, LinearTransition::Todo)
                        .await?;
                    let record = issue_record(
                        project,
                        &issue,
                        LifecycleStage::Blocked,
                        Some(blocker_record(blocker)),
                        CleanupStatus::Clean,
                    );
                    store.upsert_issue(&record).await?;
                    mark_existing_session_blocked(store, project, &issue).await?;
                    report.blocked.push(issue.identifier);
                    continue;
                }
                debug!(
                    project_id = %project.id,
                    issue = %issue.identifier,
                    "checking in-progress OpenCode handoff"
                );
                let existing = store.issue(&project.id, &issue.id).await?;
                let mut record = issue_record(
                    project,
                    &issue,
                    LifecycleStage::Running,
                    None,
                    CleanupStatus::Clean,
                );
                if let Some(existing) = &existing {
                    record.failure = existing.failure.clone();
                    record.git_ref = existing.git_ref.clone().or(record.git_ref);
                    record.cleanup_status = existing.cleanup_status;
                }
                if !has_existing_session(store, &project.id, &issue.id).await? {
                    info!(
                        project_id = %project.id,
                        issue = %issue.identifier,
                        "In Progress issue has no recorded OpenCode session; returning to Todo"
                    );
                    linear
                        .transition_issue(&issue.id, LinearTransition::Todo)
                        .await?;
                    let record = issue_record(
                        project,
                        &issue,
                        LifecycleStage::Queued,
                        None,
                        CleanupStatus::Clean,
                    );
                    store.upsert_issue(&record).await?;
                    mark_existing_session_waiting_for_project_owner_input(store, project, &issue)
                        .await?;
                    continue;
                }
                if process_in_progress_handoff(project, store, linear, opencode, &issue, existing)
                    .await?
                {
                    continue;
                }
                resume_stale_opencode_session(project, store, opencode, &issue).await?;
                if let Some(storage) = opencode_storage
                    && let Err(error) =
                        refresh_opencode_session_metrics(storage, store, project, &issue).await
                {
                    warn!(
                        project_id = %project.id,
                        issue = %issue.identifier,
                        error = %error,
                        "OpenCode persisted session metric refresh failed"
                    );
                }
                store.upsert_issue(&record).await?;
            }
            "Todo" => {
                if has_unanswered_owner_input {
                    debug!(
                        project_id = %project.id,
                        issue = %issue.identifier,
                        "Todo issue queued because project has unanswered Need Owner Input"
                    );
                    let record = issue_record(
                        project,
                        &issue,
                        LifecycleStage::Queued,
                        Some(BlockerRecord {
                            kind: "project_owner_input".into(),
                            message: "project has an unanswered Need Owner Input issue".into(),
                            observed_at: issue.updated_at.clone(),
                        }),
                        CleanupStatus::Clean,
                    );
                    store.upsert_issue(&record).await?;
                    mark_existing_session_waiting_for_project_owner_input(store, project, &issue)
                        .await?;
                    continue;
                }
                let Some(issue_milestone) = issue.project_milestone.as_ref() else {
                    info!(
                        project_id = %project.id,
                        issue = %issue.identifier,
                        "Todo issue suppressed because it has no Linear milestone"
                    );
                    let record = issue_record(
                        project,
                        &issue,
                        LifecycleStage::Blocked,
                        Some(BlockerRecord {
                            kind: "missing_todo_milestone".into(),
                            message: "Todo issue has no Linear milestone; Symphony cannot infer the active milestone".into(),
                            observed_at: issue.updated_at.clone(),
                        }),
                        CleanupStatus::Clean,
                    );
                    store.upsert_issue(&record).await?;
                    report.blocked.push(issue.identifier);
                    continue;
                };
                if let Some(blocker) = unaccepted_blocker(&issue.blocked_by) {
                    info!(
                        project_id = %project.id,
                        issue = %issue.identifier,
                        blocker_id = blocker.id.as_deref().unwrap_or("unknown"),
                        blocker_state = blocker.state.as_deref().unwrap_or("unknown"),
                        "Todo issue suppressed by nonterminal blocker"
                    );
                    let record = issue_record(
                        project,
                        &issue,
                        LifecycleStage::Blocked,
                        Some(blocker_record(blocker)),
                        CleanupStatus::Clean,
                    );
                    store.upsert_issue(&record).await?;
                    report.blocked.push(issue.identifier);
                } else if let Some(failure) =
                    unresolved_runtime_defect(store, project, &issue).await?
                {
                    info!(
                        project_id = %project.id,
                        issue = %issue.identifier,
                        failure_kind = %failure.kind,
                        failure_fingerprint = failure.fingerprint.as_deref().unwrap_or(&failure.message),
                        "Todo issue suppressed by unresolved runtime defect"
                    );
                    let mut record = issue_record(
                        project,
                        &issue,
                        LifecycleStage::Failed,
                        Some(BlockerRecord {
                            kind: "runtime_defect".into(),
                            message: format!(
                                "unresolved runtime defect: {}",
                                failure.fingerprint.as_deref().unwrap_or(&failure.message)
                            ),
                            observed_at: issue.updated_at.clone(),
                        }),
                        CleanupStatus::Clean,
                    );
                    record.failure = Some(failure);
                    store.upsert_issue(&record).await?;
                    mark_existing_session_failed_for_unresolved_runtime_defect(
                        store, project, &issue,
                    )
                    .await?;
                    report.blocked.push(issue.identifier);
                } else if runnable_todo_milestone_count > 1 {
                    let record = issue_record(
                        project,
                        &issue,
                        LifecycleStage::Queued,
                        None,
                        CleanupStatus::Clean,
                    );
                    store.upsert_issue(&record).await?;
                } else if active_runnable_todo_milestone.as_deref()
                    != Some(issue_milestone.id.as_str())
                {
                    debug!(
                        project_id = %project.id,
                        issue = %issue.identifier,
                        issue_milestone = %issue_milestone.id,
                        active_runnable_todo_milestone = active_runnable_todo_milestone.as_deref().unwrap_or("none"),
                        "Todo issue is outside the active runnable Todo milestone; leaving queued"
                    );
                    let record = issue_record(
                        project,
                        &issue,
                        LifecycleStage::Queued,
                        None,
                        CleanupStatus::Clean,
                    );
                    store.upsert_issue(&record).await?;
                } else if has_reusable_existing_session(store, &project.id, &issue.id).await? {
                    info!(
                        project_id = %project.id,
                        issue = %issue.identifier,
                        "existing OpenCode session found; queued for capacity-gated resume"
                    );
                    let record = issue_record(
                        project,
                        &issue,
                        LifecycleStage::Queued,
                        None,
                        CleanupStatus::Clean,
                    );
                    store.upsert_issue(&record).await?;
                    mark_existing_session_queued(store, project, &issue).await?;
                    eligible.push(DispatchCandidate::ExistingSession(issue));
                } else {
                    debug!(
                        project_id = %project.id,
                        issue = %issue.identifier,
                        "Todo issue is eligible for dispatch"
                    );
                    eligible.push(DispatchCandidate::New(issue));
                }
            }
            _ => {
                debug!(
                    project_id = %project.id,
                    issue = %issue.identifier,
                    state = %issue.state,
                    "recording non-executable issue state"
                );
                let record = issue_record(
                    project,
                    &issue,
                    LifecycleStage::Queued,
                    None,
                    CleanupStatus::Clean,
                );
                store.upsert_issue(&record).await?;
            }
        }
    }

    let running = store
        .issues_for_project(&project.id)
        .await?
        .into_iter()
        .filter(|issue| issue.lifecycle_stage == LifecycleStage::Running)
        .count() as u32;
    let capacity = project.concurrency.max_sessions.saturating_sub(running) as usize;
    let blocked_count = store
        .issues_for_project(&project.id)
        .await?
        .into_iter()
        .filter(|issue| issue.lifecycle_stage == LifecycleStage::Blocked)
        .count();
    let (liveness, liveness_reason) = project_liveness_projection(
        store,
        project,
        running,
        eligible.len(),
        blocked_count,
        capacity,
    )
    .await?;
    store
        .mark_project_liveness_poll(
            &project.id,
            liveness,
            &liveness_reason,
            project.concurrency.max_sessions,
            running,
            true,
        )
        .await?;
    info!(
        project_id = %project.id,
        running,
        capacity,
        eligible = eligible.len(),
        liveness = %liveness,
        "project dispatch capacity evaluated"
    );

    for candidate in eligible.into_iter().take(capacity) {
        let issue = candidate.issue();
        if let Some(reason) = missing_mnemesh_workspace_reason(project) {
            warn!(
                project_id = %project.id,
                issue = %issue.identifier,
                reason = %reason,
                "parking issue because Mnemesh workspace is not configured"
            );
            park_missing_mnemesh_workspace(project, store, linear, issue, reason).await?;
            report.parked_owner_input.push(issue.identifier.clone());
            continue;
        }
        info!(
            project_id = %project.id,
            issue = %issue.identifier,
            "dispatching issue to OpenCode"
        );
        linear
            .transition_issue(&issue.id, LinearTransition::InProgress)
            .await?;
        let launch_spec = build_acp_launch_spec(project, issue);
        let record = issue_record(
            project,
            issue,
            LifecycleStage::Running,
            None,
            CleanupStatus::Clean,
        );
        store.upsert_issue(&record).await?;
        match candidate {
            DispatchCandidate::New(issue) => {
                let started = opencode.launch(&launch_spec).await?;
                let session = new_session_record(project, &issue, started, &launch_spec);
                info!(
                    project_id = %project.id,
                    issue = %issue.identifier,
                    session_id = %session.session_id,
                    worktree_path = %session.worktree_path,
                    "OpenCode session recorded"
                );
                store.upsert_opencode_session(&session).await?;
                report.dispatched.push(issue.identifier);
            }
            DispatchCandidate::ExistingSession(issue) => {
                mark_existing_session_reactivated(store, project, &issue).await?;
                resume_stale_opencode_session(project, store, opencode, &issue).await?;
                info!(
                    project_id = %project.id,
                    issue = %issue.identifier,
                    "existing OpenCode session continued without duplicate launch"
                );
            }
        }
    }

    Ok(())
}

fn missing_mnemesh_workspace_reason(project: &ProjectConfig) -> Option<String> {
    let Some(mnemesh) = project.mnemesh.as_ref() else {
        return Some("mnemesh workspace_root is not configured".into());
    };
    let workspace_root = mnemesh.workspace_root.as_path();
    if workspace_root.as_os_str().is_empty() {
        return Some("mnemesh workspace_root is empty".into());
    }
    if !workspace_root.is_absolute() {
        return Some(format!(
            "mnemesh workspace_root is not absolute: {}",
            workspace_root.display()
        ));
    }
    None
}

async fn park_missing_mnemesh_workspace(
    project: &ProjectConfig,
    store: &SqliteStore,
    linear: &impl LinearClient,
    issue: &LinearIssue,
    reason: String,
) -> anyhow::Result<()> {
    let body = format!(
        "mnemesh_workspace_missing: {reason}\n\nConfigure `[projects.mnemesh].workspace_root` with the canonical project root Mnemesh workspace, for example `{}`. The OpenCode runner will not start until the global project workspace is passed explicitly.",
        project.repo_path.display()
    );
    linear
        .record_issue_evidence(
            &issue.id,
            LinearIssueEvidence {
                kind: "provider_blocker".into(),
                body,
            },
        )
        .await?;
    linear
        .transition_issue(&issue.id, LinearTransition::NeedOwnerInput)
        .await?;
    let record = issue_record(
        project,
        issue,
        LifecycleStage::Blocked,
        Some(BlockerRecord {
            kind: "mnemesh_workspace_missing".into(),
            message: reason,
            observed_at: issue.updated_at.clone(),
        }),
        CleanupStatus::Clean,
    );
    store.upsert_issue(&record).await?;
    Ok(())
}

async fn mark_existing_session_queued(
    store: &SqliteStore,
    project: &ProjectConfig,
    issue: &LinearIssue,
) -> anyhow::Result<()> {
    let Some(mut session) = latest_session_for_issue(store, &project.id, &issue.id).await? else {
        return Ok(());
    };
    session.process_id = None;
    session.lifecycle_stage = LifecycleStage::Queued;
    session.stage = OpenCodeStage::Silent;
    session.lifecycle_marker = Some("waiting_for_capacity".into());
    session.last_event = Some("existing_session_waiting_for_capacity".into());
    session.silence_observed = false;
    store.upsert_opencode_session(&session).await?;
    Ok(())
}

async fn unresolved_runtime_defect(
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

async fn mark_existing_session_blocked(
    store: &SqliteStore,
    project: &ProjectConfig,
    issue: &LinearIssue,
) -> anyhow::Result<()> {
    let Some(mut session) = latest_session_for_issue(store, &project.id, &issue.id).await? else {
        return Ok(());
    };
    terminate_current_session_process(project, issue, &session).await?;
    session.process_id = None;
    session.lifecycle_stage = LifecycleStage::Queued;
    session.stage = OpenCodeStage::Silent;
    session.lifecycle_marker = Some("waiting_for_blocker".into());
    session.last_event = Some("existing_session_waiting_for_blocker".into());
    session.silence_observed = false;
    store.upsert_opencode_session(&session).await?;
    Ok(())
}

async fn mark_existing_session_waiting_for_project_owner_input(
    store: &SqliteStore,
    project: &ProjectConfig,
    issue: &LinearIssue,
) -> anyhow::Result<()> {
    let Some(mut session) = latest_session_for_issue(store, &project.id, &issue.id).await? else {
        return Ok(());
    };
    terminate_current_session_process(project, issue, &session).await?;
    session.process_id = None;
    session.lifecycle_stage = LifecycleStage::Queued;
    session.stage = OpenCodeStage::Silent;
    session.lifecycle_marker = Some("waiting_for_project_owner_input".into());
    session.last_event = Some("existing_session_waiting_for_project_owner_input".into());
    session.silence_observed = false;
    store.upsert_opencode_session(&session).await?;
    Ok(())
}

async fn mark_existing_session_failed_for_unresolved_runtime_defect(
    store: &SqliteStore,
    project: &ProjectConfig,
    issue: &LinearIssue,
) -> anyhow::Result<()> {
    let Some(mut session) = latest_session_for_issue(store, &project.id, &issue.id).await? else {
        return Ok(());
    };
    terminate_current_session_process(project, issue, &session).await?;
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

async fn mark_issue_sessions_terminal(
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

async fn mark_existing_session_reactivated(
    store: &SqliteStore,
    project: &ProjectConfig,
    issue: &LinearIssue,
) -> anyhow::Result<()> {
    let Some(mut session) = latest_session_for_issue(store, &project.id, &issue.id).await? else {
        return Ok(());
    };
    session.process_id = None;
    session.lifecycle_stage = LifecycleStage::Running;
    session.stage = OpenCodeStage::Running;
    session.lifecycle_marker = Some("existing_session_reactivated".into());
    session.last_event = Some("existing_session_reactivated".into());
    session.silence_observed = false;
    store.upsert_opencode_session(&session).await?;
    Ok(())
}

async fn resume_stale_opencode_session(
    project: &ProjectConfig,
    store: &SqliteStore,
    opencode: &impl OpenCodeLauncher,
    issue: &LinearIssue,
) -> anyhow::Result<()> {
    let Some(mut session) = latest_session_for_issue(store, &project.id, &issue.id).await? else {
        return Ok(());
    };
    if !session_requires_resume(&session).await {
        return Ok(());
    }
    let launch_spec = build_acp_launch_spec(project, issue);
    let existing_issue = store.issue(&project.id, &issue.id).await?;
    if let Some(failure) = existing_issue
        .as_ref()
        .and_then(|record| record.failure.as_ref())
        .filter(|failure| recoverable_opencode_failure(failure))
    {
        terminate_current_session_process(project, issue, &session).await?;
        let started =
            continue_failed_session_repair(opencode, &launch_spec, &session, failure).await?;
        apply_repair_process(&mut session, started, failure);
    } else {
        terminate_current_session_process(project, issue, &session).await?;
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
    let mut sessions = store
        .opencode_sessions_for_issue(project_id, issue_id)
        .await?;
    sessions.sort_by(|left, right| left.session_id.cmp(&right.session_id));
    Ok(sessions.into_iter().next_back())
}

async fn has_reusable_existing_session(
    store: &SqliteStore,
    project_id: &str,
    issue_id: &str,
) -> anyhow::Result<bool> {
    let Some(session) = latest_session_for_issue(store, project_id, issue_id).await? else {
        return Ok(false);
    };
    Ok(!matches!(
        session.lifecycle_stage,
        LifecycleStage::Failed | LifecycleStage::Completed
    ))
}

async fn session_requires_resume(session: &OpenCodeSessionRecord) -> bool {
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
    session: &OpenCodeSessionRecord,
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
    let mut targets = descendant_process_ids(process_id).await?;
    targets.reverse();
    targets.push(process_id);
    terminate_processes(&targets, "-TERM").await?;
    for _ in 0..20 {
        if !process_exists(process_id).await {
            return Ok(());
        }
        sleep(Duration::from_millis(100)).await;
    }
    terminate_processes(&targets, "-KILL").await?;
    for _ in 0..10 {
        if !process_exists(process_id).await {
            return Ok(());
        }
        sleep(Duration::from_millis(100)).await;
    }
    warn!(
        project_id = %project.id,
        issue = %issue.identifier,
        session_id = %session.session_id,
        process_id,
        "stale OpenCode ACP process was still alive after SIGTERM"
    );
    Ok(())
}

async fn terminate_processes(process_ids: &[u32], signal: &str) -> anyhow::Result<()> {
    for process_id in process_ids {
        if !process_exists(*process_id).await {
            continue;
        }
        let status = tokio::process::Command::new("kill")
            .arg(signal)
            .arg(process_id.to_string())
            .status()
            .await?;
        if !status.success() && process_exists(*process_id).await {
            warn!(
                process_id,
                signal,
                status = %status,
                "failed to signal OpenCode process tree member"
            );
        }
    }
    Ok(())
}

async fn process_exists(process_id: u32) -> bool {
    tokio::fs::try_exists(format!("/proc/{process_id}"))
        .await
        .unwrap_or(false)
}

async fn descendant_process_ids(root_process_id: u32) -> anyhow::Result<Vec<u32>> {
    let mut children = std::collections::BTreeMap::<u32, Vec<u32>>::new();
    let mut entries = tokio::fs::read_dir("/proc").await?;
    while let Some(entry) = entries.next_entry().await? {
        let file_name = entry.file_name();
        let Some(pid) = file_name.to_str().and_then(|name| name.parse::<u32>().ok()) else {
            continue;
        };
        let Ok(parent_pid) = read_parent_process_id(pid).await else {
            continue;
        };
        children.entry(parent_pid).or_default().push(pid);
    }

    let mut descendants = Vec::new();
    let mut stack = children.remove(&root_process_id).unwrap_or_default();
    while let Some(pid) = stack.pop() {
        if let Some(grandchildren) = children.remove(&pid) {
            stack.extend(grandchildren);
        }
        descendants.push(pid);
    }
    Ok(descendants)
}

async fn read_parent_process_id(process_id: u32) -> anyhow::Result<u32> {
    let stat = tokio::fs::read_to_string(format!("/proc/{process_id}/stat")).await?;
    let Some(after_command) = stat.rsplit_once(") ") else {
        anyhow::bail!("invalid proc stat for pid {process_id}");
    };
    let mut fields = after_command.1.split_whitespace();
    let _state = fields.next();
    let Some(parent_pid) = fields.next() else {
        anyhow::bail!("missing parent pid for pid {process_id}");
    };
    Ok(parent_pid.parse::<u32>()?)
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
    session.last_event = started
        .process_id
        .map(|process_id| format!("continuation_prompted:{process_id}"))
        .or_else(|| Some("continuation_prompted".into()));
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

async fn refresh_opencode_session_metrics(
    storage: &OpenCodeStorageConfig,
    store: &SqliteStore,
    project: &ProjectConfig,
    issue: &LinearIssue,
) -> anyhow::Result<()> {
    let mut sessions = store
        .opencode_sessions_for_issue(&project.id, &issue.id)
        .await?;
    sessions.sort_by(|left, right| left.session_id.cmp(&right.session_id));
    let Some(mut session) = sessions.into_iter().next_back() else {
        return Ok(());
    };
    let Some(metrics) =
        read_session_tree_metrics(&storage.database_path, &session.session_id).await?
    else {
        debug!(
            project_id = %project.id,
            issue = %issue.identifier,
            session_id = %session.session_id,
            "OpenCode persisted session tree was not found during metric refresh"
        );
        return Ok(());
    };
    let previous_last_event = session.last_event.clone();
    let previous_marker = session.lifecycle_marker.clone();
    apply_session_tree_metrics_preserving_marker(
        &mut session,
        &metrics,
        previous_last_event.as_deref(),
        previous_marker.as_deref(),
    );
    if session.last_event != previous_last_event {
        info!(
            project_id = %project.id,
            issue = %issue.identifier,
            session_id = %session.session_id,
            sessions = metrics.session_count,
            subagents = metrics.subagent_count,
            messages = metrics.message_count,
            parts = metrics.part_count,
            todos = metrics.todo_count,
            tokens = metrics.tokens_total,
            cost_micros = metrics.cost_micros,
            active_agent = metrics.active_agent.as_deref().unwrap_or("unknown"),
            active_model = metrics.active_model.as_deref().unwrap_or("unknown"),
            "OpenCode persisted session metrics refreshed"
        );
    }
    store.upsert_opencode_session(&session).await?;
    Ok(())
}

fn active_runnable_todo_milestone(issues: &[LinearIssue]) -> Option<String> {
    issues
        .iter()
        .filter(|issue| issue.state == "Todo" && unaccepted_blocker(&issue.blocked_by).is_none())
        .find_map(|issue| {
            issue
                .project_milestone
                .as_ref()
                .map(|milestone| milestone.id.clone())
        })
}

fn runnable_todo_milestone_count(issues: &[LinearIssue]) -> usize {
    let mut milestones = Vec::<&str>::new();
    for issue in issues
        .iter()
        .filter(|issue| issue.state == "Todo" && unaccepted_blocker(&issue.blocked_by).is_none())
    {
        let Some(milestone) = issue.project_milestone.as_ref() else {
            continue;
        };
        if !milestones.contains(&milestone.id.as_str()) {
            milestones.push(milestone.id.as_str());
        }
    }
    milestones.len()
}
