mod acceptance_self_defect;
mod cleanup;
mod git_closure;
mod handoff;
mod http;
mod liveness;
mod policy;
mod records;
mod self_defects;
mod session;
mod task_selection;

use std::{collections::HashSet, error::Error as StdError, path::PathBuf};

use anyhow::Context;
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

use crate::{
    config::{OpenCodeStorageConfig, ProjectConfig, RootConfig},
    linear::{
        EmptyLinearClient, LinearClient, LinearGraphqlClient, LinearIssue, LinearIssueEvidence,
        LinearTransition, ReqwestGraphqlTransport,
    },
    opencode::{
        DeterministicOpenCodeLauncher, OpenCodeLaunchObserver, OpenCodeLauncher,
        OpenCodeProcessStarted, OpenCodeSessionCreated, OpenCodeStartedSession,
        ProcessTreeTerminationEvidence, StdioOpenCodeLauncher,
        apply_session_tree_metrics_preserving_marker, build_acp_launch_spec, new_session_record,
        read_latest_session_tree_error, read_session_tree_metrics,
    },
    state::{
        BlockerRecord, CleanupStatus, FailureRecord, IssueStateRecord, LifecycleStage,
        OpenCodeSessionRecord, OpenCodeStage, RuntimeLivenessStatus, SelfDefectResolutionState,
    },
    storage::SqliteStore,
};
use acceptance_self_defect::{
    AcceptanceSelfDefectInput, record_acceptance_self_defect_with_linear_client,
};
use handoff::{
    park_typed_blocker, process_in_progress_handoff, process_recoverable_failed_handoff,
};
use http::run_continuous;
use liveness::project_liveness_projection;
use policy::{
    blocker_record, compare_issues_for_dispatch, has_new_owner_response, is_terminal_state,
    unaccepted_blocker,
};
use records::issue_record;
use self_defects::{RuntimeSelfDefectInput, record_runtime_self_defect};
use session::{
    has_reusable_existing_session, latest_running_session_for_issue, mark_existing_session_blocked,
    mark_existing_session_failed_for_unresolved_runtime_defect, mark_existing_session_queued,
    mark_existing_session_resume_failed, mark_existing_session_waiting_for_project_owner_input,
    mark_historical_sessions_ignored, mark_issue_sessions_terminal, resume_stale_opencode_session,
    session_has_live_process, unresolved_runtime_defect,
};
use task_selection::{
    DispatchSelection, compare_dispatch_selections, is_managed_self_defect_issue,
    self_bug_default_suppression,
};

#[derive(Debug)]
pub struct DaemonOptions {
    pub config_path: PathBuf,
    pub database_path: PathBuf,
    pub once: bool,
}

#[derive(Debug)]
pub struct AcceptanceSelfDefectOptions {
    pub config_path: PathBuf,
    pub database_path: PathBuf,
    pub source_project_id: String,
    pub source_issue_identifier: String,
    pub session_id: String,
    pub fingerprint: String,
    pub message: String,
    pub process_id: Option<u32>,
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

pub async fn record_acceptance_self_defect(
    options: AcceptanceSelfDefectOptions,
) -> anyhow::Result<()> {
    let input = tokio::fs::read_to_string(&options.config_path)
        .await
        .with_context(|| format!("read config {}", options.config_path.display()))?;
    let config = RootConfig::from_toml_str(&input)?;
    let store = SqliteStore::open(&options.database_path)
        .await
        .with_context(|| format!("open sqlite database {}", options.database_path.display()))?;
    store.migrate().await?;
    store.reconcile_projects(&config).await?;

    let linear = LinearGraphqlClient::<ReqwestGraphqlTransport>::from_env()?;
    record_acceptance_self_defect_with_linear_client(
        &config,
        &store,
        &linear,
        AcceptanceSelfDefectInput {
            source_project_id: &options.source_project_id,
            source_issue_identifier: &options.source_issue_identifier,
            session_id: &options.session_id,
            fingerprint: &options.fingerprint,
            message: &options.message,
            process_id: options.process_id,
        },
    )
    .await?;
    Ok(())
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OrchestrationReport {
    pub dispatched: Vec<String>,
    pub blocked: Vec<String>,
    pub parked_owner_input: Vec<String>,
    pub terminal_reconciled: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum DispatchCandidate {
    New(LinearIssue),
    ExistingSession(LinearIssue),
}

impl DispatchCandidate {
    pub(super) const fn issue(&self) -> &LinearIssue {
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
    let self_defect_project = config.project("symphony");
    let self_defect_project = self_defect_project.unwrap_or_else(|| {
        config
            .projects()
            .first()
            .expect("at least one configured project")
    });
    let mut dispatch_queue = Vec::new();
    for (project_index, project) in config
        .projects()
        .iter()
        .enumerate()
        .filter(|(_, project)| project.enabled)
    {
        if let Err(error) = reconcile_project(
            project_index,
            project,
            ReconcileContext {
                self_defect_project,
                opencode_storage: config.opencode_storage.as_ref(),
                store,
                linear,
                opencode,
            },
            &mut report,
            &mut dispatch_queue,
        )
        .await
        {
            record_project_orchestration_error(store, project, &error).await?;
        }
    }

    dispatch_queue.sort_by(compare_dispatch_selections);
    for selection in dispatch_queue {
        if let Some(project) = config.project(&selection.project_id) {
            dispatch_candidate(
                project,
                self_defect_project,
                store,
                linear,
                opencode,
                selection.candidate,
                &mut report,
            )
            .await?;
        }
    }

    Ok(report)
}

async fn record_project_orchestration_error(
    store: &SqliteStore,
    project: &ProjectConfig,
    error: &anyhow::Error,
) -> anyhow::Result<()> {
    let error_chain = error
        .chain()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(": ");
    error!(
        project_id = %project.id,
        error = %error,
        error_chain = %error_chain,
        "project orchestration failed without aborting global poll"
    );
    let running = running_execution_count(store, &project.id).await?;
    store
        .mark_project_liveness_poll(
            &project.id,
            RuntimeLivenessStatus::RunnerSetupFailed,
            &format!("project orchestration failed: {error_chain}"),
            project.concurrency.max_sessions,
            running,
            false,
        )
        .await?;
    Ok(())
}

async fn running_execution_count(store: &SqliteStore, project_id: &str) -> anyhow::Result<u32> {
    let mut issue_ids = store
        .issues_for_project(project_id)
        .await?
        .into_iter()
        .filter(|issue| issue.lifecycle_stage == LifecycleStage::Running)
        .map(|issue| issue.issue_id)
        .collect::<HashSet<_>>();

    for session in store.active_opencode_sessions().await? {
        if session.project_id == project_id && session_has_live_process(&session).await {
            issue_ids.insert(session.issue_id);
        }
    }

    Ok(issue_ids.len() as u32)
}

struct ReconcileContext<'a, L, O> {
    self_defect_project: &'a ProjectConfig,
    opencode_storage: Option<&'a OpenCodeStorageConfig>,
    store: &'a SqliteStore,
    linear: &'a L,
    opencode: &'a O,
}

async fn reconcile_project(
    project_index: usize,
    project: &ProjectConfig,
    context: ReconcileContext<'_, impl LinearClient, impl OpenCodeLauncher>,
    report: &mut OrchestrationReport,
    dispatch_queue: &mut Vec<DispatchSelection>,
) -> anyhow::Result<()> {
    let ReconcileContext {
        self_defect_project,
        opencode_storage,
        store,
        linear,
        opencode,
    } = context;
    let mut eligible = Vec::new();
    let mut issues = linear.fetch_candidate_issues(project).await?;
    issues.sort_by(compare_issues_for_dispatch);
    reconcile_missing_candidate_issues(project, store, &issues, report).await?;
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
                if let Some(resolution) = self_defect_resolution_for_linear_state(state) {
                    store
                        .mark_self_defect_managed_issue_resolved(&issue.id, resolution)
                        .await?;
                }
                let terminal_lifecycle_stage = lifecycle_stage_for_terminal_linear_state(state);
                let existing = store.issue(&project.id, &issue.id).await?;
                let mut record = issue_record(
                    project,
                    &issue,
                    terminal_lifecycle_stage,
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
                let sessions_changed =
                    mark_issue_sessions_terminal(store, project, &issue, terminal_lifecycle_stage)
                        .await?;
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
                    let blocker = existing
                        .as_ref()
                        .and_then(|record| record.blocker.clone())
                        .filter(|blocker| preserves_need_owner_input_blocker_kind(&blocker.kind))
                        .unwrap_or_else(|| BlockerRecord {
                            kind: "owner_input".into(),
                            message: "waiting for owner-visible answer".into(),
                            observed_at: issue.updated_at.clone(),
                        });
                    let failure = existing.as_ref().and_then(|record| record.failure.clone());
                    let record = issue_record(
                        project,
                        &issue,
                        LifecycleStage::Blocked,
                        Some(blocker),
                        CleanupStatus::Clean,
                    );
                    let record = IssueStateRecord { failure, ..record };
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
                    mark_historical_sessions_ignored(store, project, &issue).await?;
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
                let should_requeue_retained_blocker = existing
                    .as_ref()
                    .and_then(|record| record.blocker.as_ref())
                    .is_some_and(|blocker| blocker.kind != "runtime_defect");
                if retain_typed_non_owner_blocker(project, store, &issue, existing.as_ref()).await?
                {
                    if should_requeue_retained_blocker {
                        linear
                            .transition_issue(&issue.id, LinearTransition::Todo)
                            .await?;
                    }
                    report.blocked.push(issue.identifier);
                    continue;
                }
                let mut record = issue_record(
                    project,
                    &issue,
                    LifecycleStage::Running,
                    None,
                    CleanupStatus::Clean,
                );
                if let Some(existing) = &existing {
                    record.git_ref = existing.git_ref.clone().or(record.git_ref);
                    record.cleanup_status = existing.cleanup_status;
                }
                if latest_running_session_for_issue(store, &project.id, &issue.id)
                    .await?
                    .is_none()
                {
                    info!(
                        project_id = %project.id,
                        issue = %issue.identifier,
                        reason = "missing_active_session",
                        "In Progress issue has no active OpenCode session; returning to Todo for fresh dispatch"
                    );
                    linear
                        .transition_issue(&issue.id, LinearTransition::Todo)
                        .await?;
                    let mut record = issue_record(
                        project,
                        &issue,
                        LifecycleStage::Queued,
                        None,
                        CleanupStatus::Clean,
                    );
                    record.failure = None;
                    store.upsert_issue(&record).await?;
                    mark_historical_sessions_ignored(store, project, &issue).await?;
                    continue;
                }
                if let Some(storage) = opencode_storage
                    && park_opencode_provider_error_if_present(
                        storage, project, store, linear, &issue,
                    )
                    .await?
                {
                    report.blocked.push(issue.identifier);
                    continue;
                }
                if process_in_progress_handoff(
                    project,
                    self_defect_project,
                    opencode_storage,
                    store,
                    linear,
                    opencode,
                    &issue,
                    existing,
                )
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
                let existing = store.issue(&project.id, &issue.id).await?;
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
                if let Some(blocker) = self_bug_default_suppression(&issue) {
                    info!(
                        project_id = %project.id,
                        issue = %issue.identifier,
                        blocker_kind = %blocker.kind,
                        "Todo managed self-bug suppressed by task-selection policy"
                    );
                    let record = issue_record(
                        project,
                        &issue,
                        LifecycleStage::Blocked,
                        Some(blocker),
                        CleanupStatus::Clean,
                    );
                    store.upsert_issue(&record).await?;
                    report.blocked.push(issue.identifier);
                    continue;
                }
                let managed_self_defect = is_managed_self_defect_issue(&issue);
                let issue_milestone = match issue.project_milestone.as_ref() {
                    Some(milestone) => Some(milestone),
                    None if managed_self_defect => None,
                    None => {
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
                    }
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
                    mark_existing_session_blocked(store, project, &issue).await?;
                    report.blocked.push(issue.identifier);
                } else if process_recoverable_failed_handoff(
                    project,
                    self_defect_project,
                    store,
                    linear,
                    opencode,
                    &issue,
                    existing.clone(),
                )
                .await?
                {
                    continue;
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
                } else if retain_typed_non_owner_blocker(project, store, &issue, existing.as_ref())
                    .await?
                {
                    report.blocked.push(issue.identifier);
                } else if issue_milestone.is_some() && runnable_todo_milestone_count > 1 {
                    let record = issue_record(
                        project,
                        &issue,
                        LifecycleStage::Queued,
                        None,
                        CleanupStatus::Clean,
                    );
                    store.upsert_issue(&record).await?;
                } else if let Some(issue_milestone) = issue_milestone
                    && active_runnable_todo_milestone.as_deref()
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
                    let mut record = issue_record(
                        project,
                        &issue,
                        LifecycleStage::Queued,
                        None,
                        CleanupStatus::Clean,
                    );
                    if let Some(existing) = &existing {
                        record.failure.clone_from(&existing.failure);
                        record.git_ref.clone_from(&existing.git_ref);
                    }
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
        dispatch_queue.push(DispatchSelection::new(
            project_index,
            project,
            &self_defect_project.id,
            candidate,
        ));
    }

    Ok(())
}

async fn reconcile_missing_candidate_issues(
    project: &ProjectConfig,
    store: &SqliteStore,
    fetched_issues: &[LinearIssue],
    report: &mut OrchestrationReport,
) -> anyhow::Result<()> {
    let fetched_ids = fetched_issues
        .iter()
        .map(|issue| issue.id.as_str())
        .collect::<HashSet<_>>();
    for existing in store.issues_for_project(&project.id).await? {
        if fetched_ids.contains(existing.issue_id.as_str())
            || !missing_candidate_runtime_is_stale(project, store, &existing).await?
        {
            continue;
        }
        let issue = LinearIssue {
            id: existing.issue_id.clone(),
            identifier: existing.identifier.clone(),
            title: existing.title.clone(),
            description: None,
            state: "Canceled".into(),
            priority: None,
            branch_name: None,
            url: None,
            labels: Vec::new(),
            project_milestone: None,
            blocked_by: Vec::new(),
            upstream_context: Vec::new(),
            has_new_owner_answer: false,
            owner_answer_created_at: None,
            created_at: None,
            updated_at: None,
        };
        mark_issue_sessions_terminal(store, project, &issue, LifecycleStage::Canceled).await?;
        let record = IssueStateRecord {
            lifecycle_stage: LifecycleStage::Canceled,
            blocker: None,
            failure: existing.failure,
            git_ref: existing.git_ref,
            cleanup_status: existing.cleanup_status,
            ..existing
        };
        store.upsert_issue(&record).await?;
        info!(
            project_id = %project.id,
            issue = %record.identifier,
            "runtime issue is absent from Linear candidate query; local execution state canceled"
        );
        report.terminal_reconciled.push(record.identifier);
    }
    Ok(())
}

async fn missing_candidate_runtime_is_stale(
    project: &ProjectConfig,
    store: &SqliteStore,
    existing: &IssueStateRecord,
) -> anyhow::Result<bool> {
    if matches!(
        existing.lifecycle_stage,
        LifecycleStage::Queued | LifecycleStage::Blocked
    ) {
        return Ok(true);
    }
    if existing.lifecycle_stage != LifecycleStage::Running {
        return Ok(false);
    }
    let Some(session) =
        latest_running_session_for_issue(store, &project.id, &existing.issue_id).await?
    else {
        return Ok(false);
    };
    if session
        .last_event
        .as_deref()
        .is_some_and(|event| event.starts_with("stale_killed:"))
    {
        return Ok(false);
    }
    Ok(!session_has_live_process(&session).await)
}

async fn dispatch_candidate(
    project: &ProjectConfig,
    self_defect_project: &ProjectConfig,
    store: &SqliteStore,
    linear: &impl LinearClient,
    opencode: &impl OpenCodeLauncher,
    candidate: DispatchCandidate,
    report: &mut OrchestrationReport,
) -> anyhow::Result<()> {
    let issue = candidate.issue();
    info!(
        project_id = %project.id,
        issue = %issue.identifier,
        "dispatching issue to OpenCode"
    );
    linear
        .transition_issue(&issue.id, LinearTransition::InProgress)
        .await?;
    let launch_spec = build_acp_launch_spec(project, issue);
    let existing_record = store.issue(&project.id, &issue.id).await?;
    let mut record = issue_record(
        project,
        issue,
        LifecycleStage::Running,
        None,
        CleanupStatus::Clean,
    );
    if let Some(existing) = &existing_record {
        record.failure.clone_from(&existing.failure);
        record.git_ref.clone_from(&existing.git_ref);
        record.cleanup_status = existing.cleanup_status;
    }
    store.upsert_issue(&record).await?;
    match candidate {
        DispatchCandidate::New(issue) => {
            let observer = RuntimeLaunchObserver::new(project, &issue, &launch_spec, store);
            match opencode.launch_observed(&launch_spec, &observer).await {
                Ok(started) => {
                    let session = new_session_record(project, &issue, started, &launch_spec);
                    info!(
                        project_id = %project.id,
                        issue = %issue.identifier,
                        session_id = %session.session_id,
                        worktree_path = %session.worktree_path,
                        "OpenCode session recorded"
                    );
                    upsert_observed_launch_session(store, session).await?;
                    report.dispatched.push(issue.identifier);
                }
                Err(error) => {
                    handle_launch_failure(
                        project,
                        self_defect_project,
                        store,
                        linear,
                        &issue,
                        &launch_spec,
                        error,
                    )
                    .await?;
                }
            }
        }
        DispatchCandidate::ExistingSession(issue) => {
            if let Err(error) = resume_stale_opencode_session(project, store, opencode, &issue)
                .await
                .context("continue existing OpenCode session")
            {
                let failure_reason = error_chain(error.as_ref());
                mark_existing_session_resume_failed(store, project, &issue, &failure_reason)
                    .await?;
                let observer = RuntimeLaunchObserver::new(project, &issue, &launch_spec, store);
                match opencode.launch_observed(&launch_spec, &observer).await {
                    Ok(started) => {
                        let session = new_session_record(project, &issue, started, &launch_spec);
                        info!(
                            project_id = %project.id,
                            issue = %issue.identifier,
                            session_id = %session.session_id,
                            worktree_path = %session.worktree_path,
                            previous_failure_reason = %failure_reason,
                            "fresh OpenCode session recorded after failed resume"
                        );
                        upsert_observed_launch_session(store, session).await?;
                        report.dispatched.push(issue.identifier);
                    }
                    Err(error) => {
                        handle_launch_failure(
                            project,
                            self_defect_project,
                            store,
                            linear,
                            &issue,
                            &launch_spec,
                            crate::opencode::OpenCodeError::InvalidWorktree(format!(
                                "{failure_reason}; fresh launch also failed: {error}"
                            )),
                        )
                        .await?;
                    }
                }
            } else {
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

async fn upsert_observed_launch_session(
    store: &SqliteStore,
    mut session: OpenCodeSessionRecord,
) -> anyhow::Result<()> {
    if let Some(existing) = store
        .opencode_session(&session.project_id, &session.issue_id, &session.session_id)
        .await?
        && is_observed_launch_marker(existing.lifecycle_marker.as_deref())
    {
        session.lifecycle_marker = existing.lifecycle_marker;
        session.last_event = existing.last_event;
    }

    store.upsert_opencode_session(&session).await?;
    Ok(())
}

fn is_observed_launch_marker(marker: Option<&str>) -> bool {
    matches!(marker, Some("acp_process_started" | "acp_session_attached"))
}

struct RuntimeLaunchObserver<'a> {
    project: &'a ProjectConfig,
    issue: &'a LinearIssue,
    launch_spec: &'a crate::opencode::OpenCodeLaunchSpec,
    store: &'a SqliteStore,
    provisional_session_id: Mutex<Option<String>>,
}

impl<'a> RuntimeLaunchObserver<'a> {
    fn new(
        project: &'a ProjectConfig,
        issue: &'a LinearIssue,
        launch_spec: &'a crate::opencode::OpenCodeLaunchSpec,
        store: &'a SqliteStore,
    ) -> Self {
        Self {
            project,
            issue,
            launch_spec,
            store,
            provisional_session_id: Mutex::new(None),
        }
    }
}

#[async_trait::async_trait]
impl OpenCodeLaunchObserver for RuntimeLaunchObserver<'_> {
    async fn process_started(
        &self,
        event: OpenCodeProcessStarted,
    ) -> Result<(), crate::opencode::OpenCodeError> {
        let session_id = provisional_session_id(self.issue, event.process_id);
        {
            let mut provisional_session_id = self.provisional_session_id.lock().await;
            *provisional_session_id = Some(session_id.clone());
        }

        let mut session = new_session_record(
            self.project,
            self.issue,
            OpenCodeStartedSession {
                session_id,
                process_id: event.process_id,
                acp_frame_count: 0,
                session_evidence_refs: Vec::new(),
            },
            self.launch_spec,
        );
        session.lifecycle_marker = Some("acp_process_started".into());
        session.last_event = Some(
            event
                .process_id
                .map(|process_id| format!("acp_process_started:{process_id}"))
                .unwrap_or_else(|| "acp_process_started:no_pid".into()),
        );
        self.store
            .upsert_opencode_session(&session)
            .await
            .map_err(|error| crate::opencode::OpenCodeError::LaunchObserver(error.to_string()))
    }

    async fn session_created(
        &self,
        event: OpenCodeSessionCreated,
    ) -> Result<(), crate::opencode::OpenCodeError> {
        let provisional_session_id = {
            let mut provisional_session_id = self.provisional_session_id.lock().await;
            provisional_session_id.take()
        };
        if let Some(session_id) = provisional_session_id {
            self.store
                .delete_opencode_session(&self.project.id, &self.issue.id, &session_id)
                .await
                .map_err(|error| {
                    crate::opencode::OpenCodeError::LaunchObserver(error.to_string())
                })?;
        }

        let mut session = new_session_record(
            self.project,
            self.issue,
            OpenCodeStartedSession {
                session_id: event.session_id,
                process_id: event.process_id,
                acp_frame_count: 0,
                session_evidence_refs: Vec::new(),
            },
            self.launch_spec,
        );
        session.lifecycle_marker = Some("acp_session_attached".into());
        session.last_event = Some(
            event
                .process_id
                .map(|process_id| format!("acp_session_attached:{process_id}"))
                .unwrap_or_else(|| "acp_session_attached:no_pid".into()),
        );
        self.store
            .upsert_opencode_session(&session)
            .await
            .map_err(|error| crate::opencode::OpenCodeError::LaunchObserver(error.to_string()))
    }
}

fn provisional_session_id(issue: &LinearIssue, process_id: Option<u32>) -> String {
    process_id
        .map(|process_id| format!("starting:{}:{process_id}", issue.identifier))
        .unwrap_or_else(|| format!("starting:{}:no_pid", issue.identifier))
}

async fn retain_typed_non_owner_blocker(
    project: &ProjectConfig,
    store: &SqliteStore,
    issue: &LinearIssue,
    existing: Option<&crate::state::IssueStateRecord>,
) -> anyhow::Result<bool> {
    let Some(existing) = existing else {
        return Ok(false);
    };
    let Some(blocker) = existing.blocker.as_ref() else {
        return Ok(false);
    };
    if !is_typed_non_owner_blocker_kind(&blocker.kind) {
        return Ok(false);
    }
    if blocker.kind == "runtime_defect" && unaccepted_blocker(&issue.blocked_by).is_none() {
        if let Some(managed_blocker) =
            open_managed_runtime_defect_blocker(store, issue, existing).await?
        {
            let mut record = issue_record(
                project,
                issue,
                LifecycleStage::Failed,
                Some(managed_blocker),
                CleanupStatus::Clean,
            );
            record.failure.clone_from(&existing.failure);
            record.git_ref.clone_from(&existing.git_ref);
            store.upsert_issue(&record).await?;
            return Ok(true);
        }
        if issue.state == "Todo" {
            return Ok(false);
        }
    }
    if issue.state == "Todo"
        && unaccepted_blocker(&issue.blocked_by).is_none()
        && retryable_todo_blocker_kind(&blocker.kind)
    {
        return Ok(false);
    }

    let lifecycle_stage = if existing.lifecycle_stage == LifecycleStage::Failed {
        LifecycleStage::Failed
    } else {
        LifecycleStage::Blocked
    };
    let mut record = issue_record(
        project,
        issue,
        lifecycle_stage,
        Some(blocker.clone()),
        CleanupStatus::Clean,
    );
    record.failure.clone_from(&existing.failure);
    record.git_ref.clone_from(&existing.git_ref);
    store.upsert_issue(&record).await?;
    Ok(true)
}

async fn open_managed_runtime_defect_blocker(
    store: &SqliteStore,
    issue: &LinearIssue,
    existing: &crate::state::IssueStateRecord,
) -> anyhow::Result<Option<BlockerRecord>> {
    let Some(failure) = existing.failure.as_ref() else {
        return Ok(None);
    };
    let fingerprint = failure
        .fingerprint
        .as_deref()
        .unwrap_or(failure.kind.as_str());
    let Some(managed) = store.open_self_defect_by_fingerprint(fingerprint).await? else {
        return Ok(None);
    };
    if managed.managed_issue_id == issue.id || managed.managed_issue_identifier == issue.identifier
    {
        return Ok(None);
    }
    Ok(Some(BlockerRecord {
        kind: "runtime_defect".into(),
        message: format!(
            "unresolved runtime defect: {fingerprint} (managed by {})",
            managed.managed_issue_identifier
        ),
        observed_at: issue.updated_at.clone(),
    }))
}

fn is_typed_non_owner_blocker_kind(kind: &str) -> bool {
    matches!(
        kind,
        "provider_blocker" | "repeated_eval_failure" | "runtime_defect"
    )
}

fn retryable_todo_blocker_kind(kind: &str) -> bool {
    matches!(kind, "provider_blocker")
}

async fn handle_launch_failure(
    project: &ProjectConfig,
    self_defect_project: &ProjectConfig,
    store: &SqliteStore,
    linear: &impl LinearClient,
    issue: &LinearIssue,
    launch_spec: &crate::opencode::OpenCodeLaunchSpec,
    error: crate::opencode::OpenCodeError,
) -> anyhow::Result<()> {
    let failure_reason = error_chain(&error);
    let occurrence_count = launch_failure_occurrence_count(store, project, issue).await?;
    warn!(
        project_id = %project.id,
        issue_id = %issue.id,
        issue = %issue.identifier,
        worktree_path = %launch_spec.cwd.display(),
        expected_branch = %launch_spec.branch_name,
        failure_reason = %failure_reason,
        "OpenCode launch failed after Linear transition"
    );
    linear
        .record_issue_evidence(
            &issue.id,
            LinearIssueEvidence {
                kind: "runtime_defect".into(),
                body: launch_failure_evidence_body(issue, launch_spec, &failure_reason),
            },
        )
        .await?;

    let failure = FailureRecord {
        kind: "runtime_defect".into(),
        message: failure_reason,
        fingerprint: Some("launch_failed".into()),
        occurrence_count,
    };
    let mut record = issue_record(
        project,
        issue,
        LifecycleStage::Failed,
        Some(BlockerRecord {
            kind: "runtime_defect".into(),
            message: "OpenCode launch failed after Linear transition".into(),
            observed_at: issue.updated_at.clone(),
        }),
        CleanupStatus::Clean,
    );
    record.failure = Some(failure.clone());
    store.upsert_issue(&record).await?;
    let session = launch_failure_session(project, issue, launch_spec, &error);
    record_runtime_self_defect(
        project,
        self_defect_project,
        store,
        linear,
        RuntimeSelfDefectInput {
            issue,
            evidence_kind: "runtime_defect",
            message: "OpenCode launch failed after Linear transition",
            failure: &failure,
            session: &session,
        },
    )
    .await?;
    linear
        .transition_issue(&issue.id, LinearTransition::Todo)
        .await?;
    if matches!(
        launch_spec.provider_mode,
        crate::state::RuntimeProviderMode::OmpAcp
    ) || matches!(error, crate::opencode::OpenCodeError::AcpSetupFailed { .. })
    {
        store.upsert_opencode_session(&session).await?;
    }
    Ok(())
}

async fn launch_failure_occurrence_count(
    store: &SqliteStore,
    project: &ProjectConfig,
    issue: &LinearIssue,
) -> anyhow::Result<u32> {
    let previous_count = store
        .issue(&project.id, &issue.id)
        .await?
        .and_then(|record| record.failure)
        .filter(|failure| failure.fingerprint.as_deref() == Some("launch_failed"))
        .map(|failure| failure.occurrence_count.max(1))
        .unwrap_or(0);
    Ok(previous_count.saturating_add(1))
}

fn launch_failure_session(
    project: &ProjectConfig,
    issue: &LinearIssue,
    launch_spec: &crate::opencode::OpenCodeLaunchSpec,
    error: &crate::opencode::OpenCodeError,
) -> OpenCodeSessionRecord {
    setup_failure_session(project, issue, launch_spec, error).unwrap_or_else(|| {
        OpenCodeSessionRecord {
            project_id: project.id.clone(),
            issue_id: issue.id.clone(),
            session_id: format!("launch-failed:{}", issue.identifier),
            provider_mode: launch_spec.provider_mode,
            provider_id: launch_spec.provider_id.clone(),
            agent: launch_spec.agent.clone(),
            model: launch_spec.model.clone(),
            worktree_path: launch_spec.cwd.display().to_string(),
            process_id: None,
            lifecycle_stage: LifecycleStage::Failed,
            stage: OpenCodeStage::Failed,
            active_agent: Some(launch_spec.agent.clone()),
            active_model: launch_spec.model.clone(),
            message_count: 0,
            todo_count: 0,
            part_count: 0,
            token_count: 0,
            cost_micros: 0,
            subagent_count: 0,
            eval_stage: None,
            lifecycle_marker: Some("launch_failed".into()),
            last_event: Some("launch_failed".into()),
            runtime_failure_kind: launch_failure_kind(error),
            acp_frame_count: 0,
            session_evidence_refs: Vec::new(),
            silence_observed: false,
        }
    })
}

fn setup_failure_session(
    project: &ProjectConfig,
    issue: &LinearIssue,
    launch_spec: &crate::opencode::OpenCodeLaunchSpec,
    error: &crate::opencode::OpenCodeError,
) -> Option<OpenCodeSessionRecord> {
    let crate::opencode::OpenCodeError::AcpSetupFailed {
        process_id,
        session_id,
        reason,
        termination,
        ..
    } = error
    else {
        return None;
    };
    let session_id = session_id
        .clone()
        .unwrap_or_else(|| format!("setup-failed:{}", issue.identifier));
    Some(OpenCodeSessionRecord {
        project_id: project.id.clone(),
        issue_id: issue.id.clone(),
        session_id,
        provider_mode: launch_spec.provider_mode,
        provider_id: launch_spec.provider_id.clone(),
        agent: launch_spec.agent.clone(),
        model: launch_spec.model.clone(),
        worktree_path: launch_spec.cwd.display().to_string(),
        process_id: *process_id,
        lifecycle_stage: LifecycleStage::Failed,
        stage: OpenCodeStage::Failed,
        active_agent: Some(launch_spec.agent.clone()),
        active_model: launch_spec.model.clone(),
        message_count: 0,
        todo_count: 0,
        part_count: 0,
        token_count: 0,
        cost_micros: 0,
        subagent_count: 0,
        eval_stage: Some(project.eval.default_suite.clone()),
        lifecycle_marker: Some(format!("setup_failed:{reason}")),
        last_event: Some(setup_failure_last_event(*process_id, termination)),
        runtime_failure_kind: (launch_spec.provider_mode
            == crate::state::RuntimeProviderMode::OmpAcp)
            .then(|| crate::opencode::classify_omp_acp_failure_kind(reason)),
        acp_frame_count: 0,
        session_evidence_refs: Vec::new(),
        silence_observed: false,
    })
}

fn launch_failure_kind(
    error: &crate::opencode::OpenCodeError,
) -> Option<crate::state::RuntimeFailureKind> {
    match error {
        crate::opencode::OpenCodeError::RuntimeFailure { kind, .. } => Some(kind.clone()),
        crate::opencode::OpenCodeError::Io(io) if io.kind() == std::io::ErrorKind::NotFound => {
            Some(crate::state::RuntimeFailureKind::MissingBinary)
        }
        _ => None,
    }
}

fn setup_failure_last_event(
    process_id: Option<u32>,
    termination: &ProcessTreeTerminationEvidence,
) -> String {
    let process = process_id
        .map(|pid| pid.to_string())
        .unwrap_or_else(|| "no_pid".into());
    format!(
        "setup_failed:{process}:term={}:kill={}:alive={}",
        termination.term_signal_sent, termination.kill_signal_sent, termination.still_alive
    )
}

fn launch_failure_evidence_body(
    issue: &LinearIssue,
    launch_spec: &crate::opencode::OpenCodeLaunchSpec,
    failure_reason: &str,
) -> String {
    format!(
        "runtime_defect: launch_failed\nissue_id: {}\nissue_identifier: {}\nattempted_worktree_path: {}\nexpected_branch: {}\nelapsed_seconds: unknown\nfailure_reason: {}",
        issue.id,
        issue.identifier,
        launch_spec.cwd.display(),
        launch_spec.branch_name,
        failure_reason
    )
}

fn error_chain(error: &(dyn StdError + 'static)) -> String {
    let mut parts = vec![error.to_string()];
    let mut source = error.source();
    while let Some(error) = source {
        parts.push(error.to_string());
        source = error.source();
    }
    parts.join(": ")
}

async fn refresh_opencode_session_metrics(
    storage: &OpenCodeStorageConfig,
    store: &SqliteStore,
    project: &ProjectConfig,
    issue: &LinearIssue,
) -> anyhow::Result<()> {
    let Some(mut session) = store
        .opencode_sessions_for_issue(&project.id, &issue.id)
        .await?
        .pop()
    else {
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

async fn park_opencode_provider_error_if_present(
    storage: &OpenCodeStorageConfig,
    project: &ProjectConfig,
    store: &SqliteStore,
    linear: &impl LinearClient,
    issue: &LinearIssue,
) -> anyhow::Result<bool> {
    let Some(session) = store
        .opencode_sessions_for_issue(&project.id, &issue.id)
        .await?
        .pop()
    else {
        return Ok(false);
    };
    if session.lifecycle_stage != LifecycleStage::Running {
        return Ok(false);
    }
    let Some(error) =
        read_latest_session_tree_error(&storage.database_path, &session.session_id).await?
    else {
        return Ok(false);
    };
    if !opencode_error_is_provider_blocker(&error.name, &error.message) {
        return Ok(false);
    }
    if let Some(metrics) =
        read_session_tree_metrics(&storage.database_path, &session.session_id).await?
        && opencode_provider_error_is_stale(&error, &metrics)
    {
        debug!(
            project_id = %project.id,
            issue = %issue.identifier,
            session_id = %session.session_id,
            message_id = %error.message_id,
            error_updated_ms = error.time_updated_ms,
            last_updated_ms = metrics.last_updated_ms,
            tokens = metrics.tokens_total,
            "ignoring stale OpenCode provider error after newer session activity"
        );
        return Ok(false);
    }

    let provider = error.provider_id.as_deref().unwrap_or("unknown");
    let message = format!(
        "OpenCode provider error `{name}` from provider `{provider}`: {detail}",
        name = error.name,
        detail = error.message
    );
    warn!(
        project_id = %project.id,
        issue = %issue.identifier,
        session_id = %session.session_id,
        message_id = %error.message_id,
        error_name = %error.name,
        provider = provider,
        "OpenCode provider error parked issue"
    );
    park_typed_blocker(
        project,
        store,
        linear,
        issue,
        Some(&session),
        false,
        "provider_blocker",
        format!(
            "{message}\n\nsession_id: {session_id}\nmessage_id: {message_id}\ntime_updated_ms: {time_updated_ms}\n\nThis is a provider/runtime configuration blocker, not active implementation work. Symphony killed the OpenCode ACP process tree and freed project capacity.",
            session_id = session.session_id,
            message_id = error.message_id,
            time_updated_ms = error.time_updated_ms,
        ),
        Some(FailureRecord {
            kind: "provider_blocker".into(),
            message,
            fingerprint: Some(opencode_provider_error_fingerprint(&error.name, &error.message)),
            occurrence_count: 1,
        }),
    )
    .await?;
    Ok(true)
}

fn opencode_error_is_provider_blocker(name: &str, message: &str) -> bool {
    matches!(name, "ProviderAuthError" | "ProviderError")
        || message.contains("API key is missing")
        || message.contains("Rate limit exceeded")
}

fn opencode_provider_error_fingerprint(name: &str, message: &str) -> String {
    let detail = if message.contains("API key is missing") {
        "api_key_missing"
    } else if message.contains("Rate limit exceeded") {
        "rate_limit_exceeded"
    } else {
        "provider_error"
    };
    format!("opencode_{}_{}", name.to_ascii_lowercase(), detail)
}

fn opencode_provider_error_is_stale(
    error: &crate::opencode::OpenCodeSessionMessageError,
    metrics: &crate::opencode::OpenCodeSessionTreeMetrics,
) -> bool {
    metrics
        .last_updated_ms
        .is_some_and(|last_updated| last_updated > error.time_updated_ms)
        && metrics.tokens_total > 0
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

fn self_defect_resolution_for_linear_state(state: &str) -> Option<SelfDefectResolutionState> {
    match state {
        "Done" => Some(SelfDefectResolutionState::Done),
        "Canceled" => Some(SelfDefectResolutionState::Canceled),
        _ => None,
    }
}

fn lifecycle_stage_for_terminal_linear_state(state: &str) -> LifecycleStage {
    match state {
        "Canceled" => LifecycleStage::Canceled,
        _ => LifecycleStage::Completed,
    }
}

fn preserves_need_owner_input_blocker_kind(kind: &str) -> bool {
    !matches!(
        kind,
        "owner_input" | "project_owner_input" | "linear_blocker"
    )
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
