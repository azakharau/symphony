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
mod stage_dispatch;
mod task_selection;

use std::{collections::HashSet, path::PathBuf};

use anyhow::Context;
use tracing::{debug, error, info, warn};

use crate::{
    config::{ProjectConfig, RootConfig, RunnerArchiveConfig, WorkflowStage},
    linear::{
        EmptyLinearClient, LinearClient, LinearGraphqlClient, LinearIssue, ReqwestGraphqlTransport,
    },
    runner::{
        DeterministicRunnerLauncher, RunnerLauncher, StdioRunnerLauncher,
        apply_session_tree_metrics_preserving_marker, read_latest_session_tree_error,
        read_session_tree_metrics,
    },
    state::{
        BlockerRecord, CleanupStatus, FailureRecord, IssueStateRecord, LifecycleStage,
        RuntimeLivenessStatus, SelfDefectResolutionState,
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
    blocker_record, compare_issues_for_dispatch, has_new_owner_response, unaccepted_blocker,
};
use records::issue_record;
use session::{
    latest_running_session_for_issue, mark_existing_session_blocked,
    mark_existing_session_failed_for_unresolved_runtime_defect,
    mark_existing_session_waiting_for_project_owner_input, mark_historical_sessions_ignored,
    mark_issue_sessions_stage_left, mark_issue_sessions_stage_reentered,
    mark_issue_sessions_terminal, resume_stale_runner_session, session_has_live_process,
    unresolved_runtime_defect,
};
use stage_dispatch::{
    DispatchCandidate, blockers_hash, dispatch_candidate, labels_hash, stage_invocation_is_open,
};
use task_selection::{
    DispatchSelection, compare_dispatch_selections, is_managed_self_defect_issue,
    partition_ambiguous_milestone_promotions, self_bug_default_suppression,
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
    let config = RootConfig::from_toml_file(&options.config_path)?;
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
        run_once_with_clients(&config, &store, &EmptyLinearClient, &StdioRunnerLauncher).await?;
        return Ok(());
    }

    run_continuous(config, options.database_path).await?;

    Ok(())
}

pub async fn record_acceptance_self_defect(
    options: AcceptanceSelfDefectOptions,
) -> anyhow::Result<()> {
    let config = RootConfig::from_toml_file(&options.config_path)?;
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

pub async fn run_once_with_linear_client(
    config: &RootConfig,
    store: &SqliteStore,
    linear: &impl LinearClient,
) -> anyhow::Result<OrchestrationReport> {
    run_once_with_clients(config, store, linear, &DeterministicRunnerLauncher).await
}

pub async fn run_once_with_clients(
    config: &RootConfig,
    store: &SqliteStore,
    linear: &impl LinearClient,
    runner: &impl RunnerLauncher,
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
                runner_archive: config.runner_archive.as_ref(),
                store,
                linear,
                runner,
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
                runner,
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

    for session in store.active_runner_sessions().await? {
        if session.project_id == project_id && session_has_live_process(&session).await {
            issue_ids.insert(session.issue_id);
        }
    }

    Ok(issue_ids.len() as u32)
}

struct ReconcileContext<'a, L, O> {
    self_defect_project: &'a ProjectConfig,
    runner_archive: Option<&'a RunnerArchiveConfig>,
    store: &'a SqliteStore,
    linear: &'a L,
    runner: &'a O,
}

async fn reconcile_project(
    project_index: usize,
    project: &ProjectConfig,
    context: ReconcileContext<'_, impl LinearClient, impl RunnerLauncher>,
    report: &mut OrchestrationReport,
    dispatch_queue: &mut Vec<DispatchSelection>,
) -> anyhow::Result<()> {
    let ReconcileContext {
        self_defect_project,
        runner_archive,
        store,
        linear,
        runner,
    } = context;
    validate_configured_linear_states(linear, project).await?;
    let mut stage_entries = Vec::new();
    let mut promotions = Vec::new();
    let mut issues = linear.fetch_candidate_issues(project).await?;
    issues.sort_by(compare_issues_for_dispatch);
    reconcile_missing_candidate_issues(project, store, &issues, report).await?;
    let has_unanswered_owner_input = project.workflow.block_project_dispatch_for_owner_input()
        && issues.iter().any(|issue| {
            project
                .workflow
                .is_stage(&issue.state, WorkflowStage::NeedOwnerInput)
                && !issue.has_new_owner_answer
        });
    debug!(
        project_id = %project.id,
        issues = issues.len(),
        "fetched Linear candidate issues"
    );
    for issue in issues {
        match project.workflow.stage_for_linear_state(&issue.state) {
            Some(WorkflowStage::Backlog) => {
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
                store
                    .mark_latest_stage_invocation_status(&project.id, &issue.id, "left_stage")
                    .await?;
            }
            Some(stage) if project.workflow.is_terminal_stage(stage) => {
                if let Some(resolution) = self_defect_resolution_for_workflow_stage(stage) {
                    store
                        .mark_self_defect_managed_issue_resolved(&issue.id, resolution)
                        .await?;
                }
                let terminal_lifecycle_stage = lifecycle_stage_for_workflow_stage(stage);
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
                let labels_hash = labels_hash(&issue.labels);
                let blockers_hash = blockers_hash(&issue);
                store
                    .update_latest_stage_invocation_observation(
                        &project.id,
                        &issue.id,
                        issue.updated_at.as_deref(),
                        &labels_hash,
                        &blockers_hash,
                    )
                    .await?;
                store
                    .mark_latest_stage_invocation_status(
                        &project.id,
                        &issue.id,
                        terminal_lifecycle_stage.as_str(),
                    )
                    .await?;
                if issue_changed || sessions_changed {
                    info!(
                        project_id = %project.id,
                        issue = %issue.identifier,
                        state = %issue.state,
                        cleanup = ?record.cleanup_status,
                        "terminal issue reconciled"
                    );
                    report.terminal_reconciled.push(issue.identifier);
                }
            }
            Some(WorkflowStage::NeedOwnerInput) => {
                let existing = store.issue(&project.id, &issue.id).await?;
                store
                    .mark_latest_stage_invocation_status(&project.id, &issue.id, "left_stage")
                    .await?;
                if has_new_owner_response(existing.as_ref(), &issue) {
                    info!(
                        project_id = %project.id,
                        issue = %issue.identifier,
                        "new owner response observed; returning issue to configured workflow stage"
                    );
                    transition_issue_to_stage(
                        linear,
                        project,
                        &issue.id,
                        project.workflow.owner_input_return_stage(),
                    )
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
            Some(WorkflowStage::InProgress) => {
                if has_unanswered_owner_input {
                    info!(
                        project_id = %project.id,
                        issue = %issue.identifier,
                        "pausing in-progress issue because project has unanswered Need Owner Input"
                    );
                    transition_issue_to_stage(linear, project, &issue.id, WorkflowStage::Todo)
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
                    transition_issue_to_stage(linear, project, &issue.id, WorkflowStage::Todo)
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
                    "checking in-progress runner handoff"
                );
                let existing = store.issue(&project.id, &issue.id).await?;
                let should_requeue_retained_blocker = existing
                    .as_ref()
                    .and_then(|record| record.blocker.as_ref())
                    .is_some_and(|blocker| blocker.kind != "runtime_defect");
                if retain_typed_non_owner_blocker(project, store, &issue, existing.as_ref()).await?
                {
                    if should_requeue_retained_blocker {
                        transition_issue_to_stage(linear, project, &issue.id, WorkflowStage::Todo)
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
                let labels_hash = labels_hash(&issue.labels);
                let blockers_hash = blockers_hash(&issue);
                store
                    .update_latest_stage_invocation_observation(
                        &project.id,
                        &issue.id,
                        issue.updated_at.as_deref(),
                        &labels_hash,
                        &blockers_hash,
                    )
                    .await?;
                let latest_invocation = store
                    .latest_stage_invocation_for_issue(&project.id, &issue.id)
                    .await?;
                let has_running_session =
                    latest_running_session_for_issue(store, &project.id, &issue.id)
                        .await?
                        .is_some();
                let latest_invocation_closed = latest_invocation
                    .as_ref()
                    .is_some_and(|invocation| !stage_invocation_is_open(invocation));
                if !has_running_session || latest_invocation_closed {
                    info!(
                        project_id = %project.id,
                        issue = %issue.identifier,
                        reason = if has_running_session { "stage_reentered" } else { "missing_active_session" },
                        "In Progress issue queued for stage-entry dispatch"
                    );
                    mark_issue_sessions_stage_reentered(store, project, &issue).await?;
                    let mut record = issue_record(
                        project,
                        &issue,
                        LifecycleStage::Queued,
                        None,
                        CleanupStatus::Clean,
                    );
                    if let Some(existing) = &existing {
                        record.git_ref.clone_from(&existing.git_ref);
                        record.cleanup_status = existing.cleanup_status;
                    }
                    store.upsert_issue(&record).await?;
                    stage_entries.push(DispatchCandidate::StageEntry(issue));
                    continue;
                }
                if let Some(storage) = runner_archive
                    && park_runner_provider_error_if_present(
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
                    runner_archive,
                    store,
                    linear,
                    runner,
                    &issue,
                    existing,
                )
                .await?
                {
                    continue;
                }
                resume_stale_runner_session(project, store, runner, &issue).await?;
                if let Some(storage) = runner_archive
                    && let Err(error) =
                        refresh_runner_session_metrics(storage, store, project, &issue).await
                {
                    warn!(
                        project_id = %project.id,
                        issue = %issue.identifier,
                        error = %error,
                        "runner persisted session metric refresh failed"
                    );
                }
                store.upsert_issue(&record).await?;
            }
            Some(WorkflowStage::InReview) => {
                debug!(
                    project_id = %project.id,
                    issue = %issue.identifier,
                    "checking in-review runner handoff"
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
                    record.git_ref.clone_from(&existing.git_ref);
                    record.cleanup_status = existing.cleanup_status;
                }
                let labels_hash = labels_hash(&issue.labels);
                let blockers_hash = blockers_hash(&issue);
                store
                    .update_latest_stage_invocation_observation(
                        &project.id,
                        &issue.id,
                        issue.updated_at.as_deref(),
                        &labels_hash,
                        &blockers_hash,
                    )
                    .await?;
                let latest_invocation = store
                    .latest_stage_invocation_for_issue(&project.id, &issue.id)
                    .await?;
                let has_running_session =
                    latest_running_session_for_issue(store, &project.id, &issue.id)
                        .await?
                        .is_some();
                let latest_invocation_closed = latest_invocation
                    .as_ref()
                    .is_some_and(|invocation| !stage_invocation_is_open(invocation));
                if !has_running_session || latest_invocation_closed {
                    info!(
                        project_id = %project.id,
                        issue = %issue.identifier,
                        reason = if has_running_session { "stage_reentered" } else { "missing_active_session" },
                        "In Review issue queued for stage-entry dispatch"
                    );
                    mark_issue_sessions_stage_reentered(store, project, &issue).await?;
                    let mut queued = issue_record(
                        project,
                        &issue,
                        LifecycleStage::Queued,
                        None,
                        CleanupStatus::Clean,
                    );
                    if let Some(existing) = &existing {
                        queued.git_ref.clone_from(&existing.git_ref);
                        queued.cleanup_status = existing.cleanup_status;
                    }
                    store.upsert_issue(&queued).await?;
                    stage_entries.push(DispatchCandidate::StageEntry(issue));
                    continue;
                }
                if process_in_progress_handoff(
                    project,
                    self_defect_project,
                    runner_archive,
                    store,
                    linear,
                    runner,
                    &issue,
                    existing,
                )
                .await?
                {
                    continue;
                }
                resume_stale_runner_session(project, store, runner, &issue).await?;
                store.upsert_issue(&record).await?;
            }
            Some(WorkflowStage::Todo) => {
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
                    continue;
                }
                if let Some(blocker) = self_bug_default_suppression(project, &issue) {
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
                if issue.project_milestone.is_none() && !managed_self_defect {
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
                if process_recoverable_failed_handoff(
                    project,
                    self_defect_project,
                    store,
                    linear,
                    runner,
                    &issue,
                    existing.clone(),
                )
                .await?
                {
                    continue;
                }
                store
                    .mark_latest_stage_invocation_status(&project.id, &issue.id, "left_stage")
                    .await?;
                mark_issue_sessions_stage_left(store, project, &issue).await?;
                if let Some(failure) = unresolved_runtime_defect(store, project, &issue).await? {
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
                } else {
                    debug!(
                        project_id = %project.id,
                        issue = %issue.identifier,
                        "Todo issue is eligible for capacity-gated promotion"
                    );
                    promotions.push(DispatchCandidate::Promote(issue));
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
                store
                    .mark_latest_stage_invocation_status(&project.id, &issue.id, "left_stage")
                    .await?;
            }
        }
    }

    let (allowed_promotions, suppressed_promotions, runnable_milestone_count) =
        partition_ambiguous_milestone_promotions(promotions);
    promotions = allowed_promotions;
    for candidate in suppressed_promotions {
        let issue = candidate.issue();
        info!(
            project_id = %project.id,
            issue = %issue.identifier,
            runnable_milestones = runnable_milestone_count,
            "Todo promotion suppressed because unblocked candidates span multiple milestones"
        );
        let record = issue_record(
                project,
                issue,
                LifecycleStage::Blocked,
                Some(BlockerRecord {
                    kind: "ambiguous_runnable_milestones".into(),
                    message: "unblocked Todo candidates span multiple milestones; repair the Linear blocker graph before dispatch".into(),
                    observed_at: issue.updated_at.clone(),
                }),
                CleanupStatus::Clean,
            );
        store.upsert_issue(&record).await?;
        report.blocked.push(issue.identifier.clone());
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
    let eligible_count = stage_entries.len() + promotions.len();
    let (liveness, liveness_reason) = project_liveness_projection(
        store,
        project,
        running,
        eligible_count,
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
        stage_entries = stage_entries.len(),
        promotions = promotions.len(),
        liveness = %liveness,
        "project dispatch capacity evaluated"
    );

    for candidate in stage_entries.into_iter().chain(promotions).take(capacity) {
        dispatch_queue.push(DispatchSelection::new(
            project_index,
            project,
            &self_defect_project.id,
            candidate,
        ));
    }

    Ok(())
}

async fn validate_configured_linear_states(
    linear: &impl LinearClient,
    project: &ProjectConfig,
) -> Result<(), crate::linear::LinearClientError> {
    let actual_states = linear.fetch_workflow_state_names(project).await?;
    let mut missing_states = Vec::new();
    for configured_state in project.workflow.processed_state_names() {
        if !actual_states.iter().any(|state| state == configured_state) {
            missing_states.push(configured_state.to_owned());
        }
    }
    if missing_states.is_empty() {
        Ok(())
    } else {
        Err(
            crate::linear::LinearClientError::MissingConfiguredWorkflowStates {
                project_id: project.id.clone(),
                missing_states,
            },
        )
    }
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
            state: project
                .workflow
                .required_linear_state(WorkflowStage::Canceled)
                .into(),
            state_id: None,
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
        if project.workflow.is_stage(&issue.state, WorkflowStage::Todo) {
            return Ok(false);
        }
    }
    if project.workflow.is_stage(&issue.state, WorkflowStage::Todo)
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

async fn refresh_runner_session_metrics(
    storage: &RunnerArchiveConfig,
    store: &SqliteStore,
    project: &ProjectConfig,
    issue: &LinearIssue,
) -> anyhow::Result<()> {
    let Some(mut session) = store
        .runner_sessions_for_issue(&project.id, &issue.id)
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
            "runner persisted session tree was not found during metric refresh"
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
            "runner persisted session metrics refreshed"
        );
    }
    store.upsert_runner_session(&session).await?;
    Ok(())
}

async fn park_runner_provider_error_if_present(
    storage: &RunnerArchiveConfig,
    project: &ProjectConfig,
    store: &SqliteStore,
    linear: &impl LinearClient,
    issue: &LinearIssue,
) -> anyhow::Result<bool> {
    let Some(session) = store
        .runner_sessions_for_issue(&project.id, &issue.id)
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
    if !runner_error_is_provider_blocker(&error.name, &error.message) {
        return Ok(false);
    }
    if let Some(metrics) =
        read_session_tree_metrics(&storage.database_path, &session.session_id).await?
        && runner_provider_error_is_stale(&error, &metrics)
    {
        debug!(
            project_id = %project.id,
            issue = %issue.identifier,
            session_id = %session.session_id,
            message_id = %error.message_id,
            error_updated_ms = error.time_updated_ms,
            last_updated_ms = metrics.last_updated_ms,
            tokens = metrics.tokens_total,
            "ignoring stale runner provider error after newer session activity"
        );
        return Ok(false);
    }

    let provider = error.provider_id.as_deref().unwrap_or("unknown");
    let message = format!(
        "runner provider error `{name}` from provider `{provider}`: {detail}",
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
        "runner provider error parked issue"
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
            "{message}\n\nsession_id: {session_id}\nmessage_id: {message_id}\ntime_updated_ms: {time_updated_ms}\n\nThis is a provider/runtime configuration blocker, not active implementation work. Symphony killed the runner ACP process tree and freed project capacity.",
            session_id = session.session_id,
            message_id = error.message_id,
            time_updated_ms = error.time_updated_ms,
        ),
        Some(FailureRecord {
            kind: "provider_blocker".into(),
            message,
            fingerprint: Some(runner_provider_error_fingerprint(&error.name, &error.message)),
            occurrence_count: 1,
        }),
    )
    .await?;
    Ok(true)
}

fn runner_error_is_provider_blocker(name: &str, message: &str) -> bool {
    matches!(name, "ProviderAuthError" | "ProviderError")
        || message.contains("API key is missing")
        || message.contains("Rate limit exceeded")
}

fn runner_provider_error_fingerprint(name: &str, message: &str) -> String {
    let detail = if message.contains("API key is missing") {
        "api_key_missing"
    } else if message.contains("Rate limit exceeded") {
        "rate_limit_exceeded"
    } else {
        "provider_error"
    };
    format!("runner_{}_{}", name.to_ascii_lowercase(), detail)
}

fn runner_provider_error_is_stale(
    error: &crate::runner::RunnerSessionMessageError,
    metrics: &crate::runner::RunnerSessionTreeMetrics,
) -> bool {
    metrics
        .last_updated_ms
        .is_some_and(|last_updated| last_updated > error.time_updated_ms)
        && metrics.tokens_total > 0
}

pub(super) async fn transition_issue_to_stage(
    linear: &impl LinearClient,
    project: &ProjectConfig,
    issue_id: &str,
    stage: WorkflowStage,
) -> Result<(), crate::linear::LinearClientError> {
    linear
        .transition_issue_to_state(issue_id, project.workflow.required_linear_state(stage))
        .await
}

fn self_defect_resolution_for_workflow_stage(
    stage: WorkflowStage,
) -> Option<SelfDefectResolutionState> {
    match stage {
        WorkflowStage::Done => Some(SelfDefectResolutionState::Done),
        WorkflowStage::Canceled => Some(SelfDefectResolutionState::Canceled),
        _ => None,
    }
}

fn lifecycle_stage_for_workflow_stage(stage: WorkflowStage) -> LifecycleStage {
    match stage {
        WorkflowStage::Canceled => LifecycleStage::Canceled,
        _ => LifecycleStage::Completed,
    }
}

fn preserves_need_owner_input_blocker_kind(kind: &str) -> bool {
    !matches!(
        kind,
        "owner_input" | "project_owner_input" | "linear_blocker"
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::{
        config::{BranchPolicy, ConcurrencyConfig, EvalDefaults, ProjectWorkflow},
        linear::{LinearBlocker, LinearMilestone, LinearProjectConfig},
        runner::{PermissionPolicy, RunnerRuntimeConfig, RunnerStartedSession},
        state::RunnerSessionRecord,
    };
    use tokio::sync::Mutex;

    #[derive(Debug)]
    struct StateListLinearClient {
        states: Vec<String>,
    }

    #[async_trait::async_trait]
    impl LinearClient for StateListLinearClient {
        async fn fetch_candidate_issues(
            &self,
            _project: &ProjectConfig,
        ) -> Result<Vec<LinearIssue>, crate::linear::LinearClientError> {
            panic!("candidate issues must not be fetched before configured states validate");
        }

        async fn fetch_workflow_state_names(
            &self,
            _project: &ProjectConfig,
        ) -> Result<Vec<String>, crate::linear::LinearClientError> {
            Ok(self.states.clone())
        }

        async fn transition_issue(
            &self,
            _issue_id: &str,
            _transition: crate::linear::LinearTransition,
        ) -> Result<(), crate::linear::LinearClientError> {
            Ok(())
        }
    }

    fn test_project() -> ProjectConfig {
        let mut workflow = ProjectWorkflow::default();
        workflow.states.in_review = "Code Review".into();
        workflow.processed_states = vec!["Accepted".into()];

        ProjectConfig {
            id: "project".into(),
            name: "Project".into(),
            enabled: true,
            workflow_path: PathBuf::from("workflow.toml"),
            repo_path: PathBuf::from("/repo"),
            branch: BranchPolicy {
                base: "main".into(),
                worktree_root: PathBuf::from("/worktrees"),
            },
            linear: LinearProjectConfig {
                team_key: "SYM".into(),
                project_id: Some("linear-project".into()),
            },
            runner: RunnerRuntimeConfig {
                provider_mode: crate::state::RuntimeProviderMode::Acp,
                command: PathBuf::from("runner"),
                args: Vec::new(),
                agent: "build".into(),
                model: None,
                effort: None,
                permission_policy: PermissionPolicy::Reject,
            },
            omp_acp_providers: Vec::new(),
            eval: EvalDefaults {
                default_suite: "suite".into(),
                max_identical_failure_fingerprints: 3,
            },
            concurrency: ConcurrencyConfig { max_sessions: 1 },
            workflow,
        }
    }

    fn ledger_config() -> RootConfig {
        RootConfig::from_toml_str(
            r#"
[[projects]]
id = "project"
name = "Project"
enabled = true
workflow_path = "/tmp/project.workflow.toml"
repo_path = "/tmp/project"

[projects.branch]
base = "main"
worktree_root = "/tmp/worktrees"

[projects.linear]
team_key = "SYM"
project_id = "linear-project"

[projects.runner]
command = "/usr/bin/runner"
args = []
agent = "build"
permission_policy = "reject"

[projects.eval]
default_suite = "suite"

[projects.concurrency]
max_sessions = 4
"#,
        )
        .expect("config")
    }

    fn ledger_issue(state: &str, updated_at: &str, labels: &[&str]) -> LinearIssue {
        LinearIssue {
            id: "issue-id".into(),
            identifier: "SYM-141".into(),
            title: "Ledger test".into(),
            description: Some("description must not affect invocation identity".into()),
            state: state.into(),
            state_id: Some(format!(
                "state-{}",
                state.replace(' ', "-").to_ascii_lowercase()
            )),
            priority: Some(1),
            branch_name: None,
            url: None,
            labels: labels.iter().map(|label| (*label).into()).collect(),
            project_milestone: Some(LinearMilestone {
                id: "milestone".into(),
                name: "Milestone".into(),
            }),
            blocked_by: Vec::new(),
            upstream_context: Vec::new(),
            has_new_owner_answer: false,
            owner_answer_created_at: None,
            created_at: Some("2026-01-01T00:00:00Z".into()),
            updated_at: Some(updated_at.into()),
        }
    }

    fn issue_with_milestone(
        id: &str,
        identifier: &str,
        state: &str,
        priority: Option<i64>,
        milestone_id: Option<&str>,
        blockers: Vec<LinearBlocker>,
    ) -> LinearIssue {
        let mut issue = ledger_issue(state, "2026-01-01T00:00:00Z", &[]);
        issue.id = id.into();
        issue.identifier = identifier.into();
        issue.priority = priority;
        issue.project_milestone = milestone_id.map(|milestone_id| LinearMilestone {
            id: milestone_id.into(),
            name: format!("Milestone {milestone_id}"),
        });
        issue.blocked_by = blockers;
        issue
    }

    fn blocker(identifier: &str, state: &str) -> LinearBlocker {
        LinearBlocker {
            id: Some(identifier.to_ascii_lowercase()),
            identifier: Some(identifier.into()),
            state: Some(state.into()),
        }
    }

    #[derive(Debug, Default)]
    struct LedgerLinearClient {
        issues: Mutex<Vec<LinearIssue>>,
        transitions: Mutex<Vec<(String, String)>>,
    }

    impl LedgerLinearClient {
        async fn set_issues(&self, issues: Vec<LinearIssue>) {
            *self.issues.lock().await = issues;
        }

        async fn transitions(&self) -> Vec<(String, String)> {
            self.transitions.lock().await.clone()
        }
    }

    #[async_trait::async_trait]
    impl LinearClient for LedgerLinearClient {
        async fn fetch_candidate_issues(
            &self,
            _project: &ProjectConfig,
        ) -> Result<Vec<LinearIssue>, crate::linear::LinearClientError> {
            Ok(self.issues.lock().await.clone())
        }

        async fn fetch_workflow_state_names(
            &self,
            project: &ProjectConfig,
        ) -> Result<Vec<String>, crate::linear::LinearClientError> {
            Ok(project
                .workflow
                .processed_state_names()
                .into_iter()
                .map(str::to_owned)
                .collect())
        }

        async fn transition_issue(
            &self,
            issue_id: &str,
            transition: crate::linear::LinearTransition,
        ) -> Result<(), crate::linear::LinearClientError> {
            self.transitions
                .lock()
                .await
                .push((issue_id.into(), transition.state_name().into()));
            Ok(())
        }
    }

    #[derive(Debug, Default)]
    struct CountingRunner {
        launches: Mutex<Vec<String>>,
        continuations: Mutex<Vec<String>>,
    }

    impl CountingRunner {
        async fn launch_count(&self) -> usize {
            self.launches.lock().await.len()
        }

        async fn continuation_count(&self) -> usize {
            self.continuations.lock().await.len()
        }
    }

    #[async_trait::async_trait]
    impl RunnerLauncher for CountingRunner {
        async fn launch(
            &self,
            spec: &crate::runner::RunnerLaunchSpec,
        ) -> Result<RunnerStartedSession, crate::runner::RunnerError> {
            let mut launches = self.launches.lock().await;
            launches.push(spec.issue_identifier.clone());
            Ok(RunnerStartedSession {
                session_id: format!("session-{}", launches.len()),
                process_id: None,
                acp_frame_count: 0,
                session_evidence_refs: Vec::new(),
            })
        }

        async fn continue_session(
            &self,
            _spec: &crate::runner::RunnerLaunchSpec,
            session: &RunnerSessionRecord,
            _continuation_message: &str,
        ) -> Result<RunnerStartedSession, crate::runner::RunnerError> {
            self.continuations
                .lock()
                .await
                .push(session.session_id.clone());
            Ok(RunnerStartedSession {
                session_id: session.session_id.clone(),
                process_id: None,
                acp_frame_count: session.acp_frame_count,
                session_evidence_refs: session.session_evidence_refs.clone(),
            })
        }
    }

    async fn ledger_store(path: &std::path::Path) -> SqliteStore {
        let store = SqliteStore::open(path).await.expect("open sqlite");
        store.migrate().await.expect("migrate");
        store
    }

    #[tokio::test]
    async fn todo_promotes_to_in_progress_without_runner_launch() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime.sqlite3");
        let store = ledger_store(&db_path).await;
        let config = ledger_config();
        let linear = LedgerLinearClient::default();
        let runner = CountingRunner::default();
        linear
            .set_issues(vec![ledger_issue("Todo", "2026-01-01T00:00:00Z", &[])])
            .await;

        let report = run_once_with_clients(&config, &store, &linear, &runner)
            .await
            .expect("Todo promotion");

        assert!(report.dispatched.is_empty());
        assert_eq!(runner.launch_count().await, 0);
        assert_eq!(
            linear.transitions().await,
            vec![("issue-id".into(), "In Progress".into())]
        );
        let invocations = store
            .stage_invocations_for_issue("project", "issue-id")
            .await
            .expect("invocations");
        assert!(invocations.is_empty());
    }

    #[tokio::test]
    async fn in_progress_stage_entry_launches_once_across_restart() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime.sqlite3");
        let config = ledger_config();
        let linear = LedgerLinearClient::default();
        let runner = CountingRunner::default();
        linear
            .set_issues(vec![ledger_issue("Todo", "2026-01-01T00:00:00Z", &[])])
            .await;

        {
            let store = ledger_store(&db_path).await;
            let report = run_once_with_clients(&config, &store, &linear, &runner)
                .await
                .expect("Todo promotion");
            assert!(report.dispatched.is_empty());
        }

        linear
            .set_issues(vec![ledger_issue(
                "In Progress",
                "2026-01-01T00:01:00Z",
                &[],
            )])
            .await;
        {
            let store = ledger_store(&db_path).await;
            let report = run_once_with_clients(&config, &store, &linear, &runner)
                .await
                .expect("In Progress launch");
            assert_eq!(report.dispatched, vec!["SYM-141"]);
        }

        {
            let store = ledger_store(&db_path).await;
            let report = run_once_with_clients(&config, &store, &linear, &runner)
                .await
                .expect("duplicate In Progress observation");
            assert!(report.dispatched.is_empty());
            let invocations = store
                .stage_invocations_for_issue("project", "issue-id")
                .await
                .expect("invocations");
            assert_eq!(invocations.len(), 1);
            assert_eq!(invocations[0].state_name, "In Progress");
        }

        assert_eq!(runner.launch_count().await, 1);
        assert_eq!(runner.continuation_count().await, 0);
    }

    #[tokio::test]
    async fn status_away_and_back_creates_exactly_one_new_stage_invocation() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime.sqlite3");
        let store = ledger_store(&db_path).await;
        let config = ledger_config();
        let linear = LedgerLinearClient::default();
        let runner = CountingRunner::default();

        linear
            .set_issues(vec![ledger_issue("Todo", "2026-01-01T00:00:00Z", &[])])
            .await;
        let report = run_once_with_clients(&config, &store, &linear, &runner)
            .await
            .expect("first Todo promotion");
        assert!(report.dispatched.is_empty());

        linear
            .set_issues(vec![ledger_issue(
                "In Progress",
                "2026-01-01T00:01:00Z",
                &[],
            )])
            .await;
        let report = run_once_with_clients(&config, &store, &linear, &runner)
            .await
            .expect("first In Progress launch");
        assert_eq!(report.dispatched, vec!["SYM-141"]);

        linear
            .set_issues(vec![ledger_issue("Backlog", "2026-01-01T00:02:00Z", &[])])
            .await;
        run_once_with_clients(&config, &store, &linear, &runner)
            .await
            .expect("away state");

        linear
            .set_issues(vec![ledger_issue("Todo", "2026-01-01T00:03:00Z", &[])])
            .await;
        let report = run_once_with_clients(&config, &store, &linear, &runner)
            .await
            .expect("second Todo promotion");
        assert!(report.dispatched.is_empty());

        linear
            .set_issues(vec![ledger_issue(
                "In Progress",
                "2026-01-01T00:04:00Z",
                &[],
            )])
            .await;
        let report = run_once_with_clients(&config, &store, &linear, &runner)
            .await
            .expect("second In Progress launch");
        assert_eq!(report.dispatched, vec!["SYM-141"]);

        let report = run_once_with_clients(&config, &store, &linear, &runner)
            .await
            .expect("duplicate second In Progress");
        assert!(report.dispatched.is_empty());

        let invocations = store
            .stage_invocations_for_issue("project", "issue-id")
            .await
            .expect("invocations");
        assert_eq!(invocations.len(), 2);
        let sessions = store
            .runner_sessions_for_issue("project", "issue-id")
            .await
            .expect("sessions");
        assert_eq!(
            sessions
                .iter()
                .filter(|session| session.lifecycle_stage == crate::state::LifecycleStage::Running)
                .count(),
            1
        );
        assert!(
            sessions.iter().any(
                |session| session.lifecycle_marker.as_deref() == Some("linear_stage_reentered")
            )
        );
        assert_eq!(runner.launch_count().await, 2);
    }

    #[tokio::test]
    async fn blocked_future_milestone_todo_does_not_suppress_current_runnable_milestone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime.sqlite3");
        let store = ledger_store(&db_path).await;
        let config = ledger_config();
        let linear = LedgerLinearClient::default();
        let runner = CountingRunner::default();
        let current = issue_with_milestone(
            "issue-current",
            "SYM-142",
            "Todo",
            Some(1),
            Some("m1"),
            Vec::new(),
        );
        let future = issue_with_milestone(
            "issue-future",
            "SYM-143",
            "Todo",
            Some(2),
            Some("m2"),
            vec![blocker("SYM-142", "Todo")],
        );
        linear.set_issues(vec![current, future]).await;

        let report = run_once_with_clients(&config, &store, &linear, &runner)
            .await
            .expect("runnable milestone promotion");

        assert!(report.dispatched.is_empty());
        assert_eq!(report.blocked, vec!["SYM-143"]);
        assert_eq!(
            linear.transitions().await,
            vec![("issue-current".into(), "In Progress".into())]
        );
        assert_eq!(runner.launch_count().await, 0);
    }

    #[tokio::test]
    async fn owner_input_project_blocking_respects_workflow_config() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime.sqlite3");
        let store = ledger_store(&db_path).await;
        let mut config = ledger_config();
        config
            .project_mut_for_test("project")
            .workflow
            .owner_input
            .block_project_dispatch = false;
        let linear = LedgerLinearClient::default();
        let runner = CountingRunner::default();
        linear
            .set_issues(vec![
                issue_with_milestone(
                    "issue-owner",
                    "SYM-140",
                    "Need Owner Input",
                    Some(1),
                    Some("m1"),
                    Vec::new(),
                ),
                issue_with_milestone(
                    "issue-work",
                    "SYM-142",
                    "Todo",
                    Some(1),
                    Some("m1"),
                    Vec::new(),
                ),
            ])
            .await;

        let report = run_once_with_clients(&config, &store, &linear, &runner)
            .await
            .expect("owner input config");

        assert_eq!(report.parked_owner_input, vec!["SYM-140"]);
        assert_eq!(
            linear.transitions().await,
            vec![("issue-work".into(), "In Progress".into())]
        );
        assert_eq!(runner.launch_count().await, 0);
    }

    #[tokio::test]
    async fn label_changes_update_active_invocation_without_relaunching() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("runtime.sqlite3");
        let store = ledger_store(&db_path).await;
        let config = ledger_config();
        let linear = LedgerLinearClient::default();
        let runner = CountingRunner::default();

        linear
            .set_issues(vec![ledger_issue(
                "In Progress",
                "2026-01-01T00:00:00Z",
                &["alpha"],
            )])
            .await;
        let report = run_once_with_clients(&config, &store, &linear, &runner)
            .await
            .expect("first In Progress launch");
        assert_eq!(report.dispatched, vec!["SYM-141"]);
        let before = store
            .stage_invocations_for_issue("project", "issue-id")
            .await
            .expect("before");
        let first_labels_hash = before[0].labels_hash.clone();

        linear
            .set_issues(vec![ledger_issue(
                "In Progress",
                "2026-01-01T00:01:00Z",
                &["alpha", "beta"],
            )])
            .await;
        run_once_with_clients(&config, &store, &linear, &runner)
            .await
            .expect("active label update");

        let after = store
            .stage_invocations_for_issue("project", "issue-id")
            .await
            .expect("after");
        assert_eq!(after.len(), 1);
        assert_ne!(after[0].labels_hash, first_labels_hash);
        assert_eq!(
            after[0].issue_updated_at.as_deref(),
            Some("2026-01-01T00:01:00Z")
        );
        assert_eq!(runner.launch_count().await, 1);
        assert_eq!(runner.continuation_count().await, 0);
    }

    #[tokio::test]
    async fn configured_linear_states_are_validated_before_candidate_dispatch() {
        let project = test_project();
        let linear = StateListLinearClient {
            states: vec![
                "Backlog".into(),
                "Todo".into(),
                "In Progress".into(),
                "Need Owner Input".into(),
                "Done".into(),
                "Canceled".into(),
            ],
        };

        let err = validate_configured_linear_states(&linear, &project)
            .await
            .expect_err("missing configured states must fail");

        assert!(matches!(
            err,
            crate::linear::LinearClientError::MissingConfiguredWorkflowStates { .. }
        ));
        assert_eq!(
            err.to_string(),
            "configured Linear workflow states are missing for project `project`: Code Review, Accepted"
        );
    }
}
