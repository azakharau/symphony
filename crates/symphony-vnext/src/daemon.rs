mod cleanup;
mod handoff;
mod http;
mod policy;
mod records;

use std::path::PathBuf;

use anyhow::Context;
use tracing::{debug, info, warn};

use crate::{
    config::{ProjectConfig, RootConfig},
    linear::{EmptyLinearClient, LinearClient, LinearTransition},
    opencode::{
        DeterministicOpenCodeLauncher, OpenCodeLauncher, StdioOpenCodeLauncher,
        build_acp_launch_spec, new_session_record,
    },
    state::{BlockerRecord, CleanupStatus, LifecycleStage},
    storage::SqliteStore,
};
use handoff::{park_need_owner_input, process_in_progress_handoff};
use http::run_continuous;
use policy::{
    blocker_record, compare_issues_for_dispatch, has_existing_session, has_new_owner_response,
    is_terminal_state, nonterminal_blocker,
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
    let config = RootConfig::from_yaml_str(&input)?;
    info!(
        config_path = %options.config_path.display(),
        database_path = %options.database_path.display(),
        projects = config.projects().len(),
        once = options.once,
        "symphony vNext daemon starting"
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
        reconcile_project(project, store, linear, opencode, &mut report)
            .await
            .with_context(|| format!("orchestrate project `{}`", project.id))?;
    }

    Ok(report)
}

async fn reconcile_project(
    project: &ProjectConfig,
    store: &SqliteStore,
    linear: &impl LinearClient,
    opencode: &impl OpenCodeLauncher,
    report: &mut OrchestrationReport,
) -> anyhow::Result<()> {
    let mut eligible = Vec::new();
    let mut issues = linear.fetch_candidate_issues(project).await?;
    debug!(
        project_id = %project.id,
        issues = issues.len(),
        "fetched Linear candidate issues"
    );
    issues.sort_by(compare_issues_for_dispatch);

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
                        "Backlog",
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
                    state,
                    LifecycleStage::Completed,
                    None,
                    CleanupStatus::Pending,
                );
                if let Some(existing) = existing {
                    record.git_ref = existing.git_ref;
                    if existing.cleanup_status == CleanupStatus::Complete {
                        record.cleanup_status = CleanupStatus::Complete;
                    } else if let Some(git_ref) = &record.git_ref
                        && !tokio::fs::try_exists(&git_ref.worktree_path).await?
                    {
                        record.cleanup_status = CleanupStatus::Complete;
                    }
                }
                store.upsert_issue(&record).await?;
                info!(
                    project_id = %project.id,
                    issue = %issue.identifier,
                    state,
                    cleanup = ?record.cleanup_status,
                    "terminal issue reconciled"
                );
                report.terminal_reconciled.push(issue.identifier);
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
                        LinearTransition::Todo.state_name(),
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
                        "Need Owner Input",
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
                debug!(
                    project_id = %project.id,
                    issue = %issue.identifier,
                    "checking in-progress OpenCode handoff"
                );
                let existing = store.issue(&project.id, &issue.id).await?;
                let mut record = issue_record(
                    project,
                    &issue,
                    "In Progress",
                    LifecycleStage::Running,
                    None,
                    CleanupStatus::Clean,
                );
                if let Some(existing) = &existing {
                    record.failure = existing.failure.clone();
                    record.git_ref = existing.git_ref.clone().or(record.git_ref);
                    record.cleanup_status = existing.cleanup_status;
                }
                store.upsert_issue(&record).await?;
                process_in_progress_handoff(project, store, linear, opencode, &issue, existing)
                    .await?;
            }
            "Preparing" | "In Review" | "RCA Required" => {
                warn!(
                    project_id = %project.id,
                    issue = %issue.identifier,
                    state = %issue.state,
                    "legacy state observed in Rust vNext state machine; parking"
                );
                park_need_owner_input(
                    project,
                    store,
                    linear,
                    &issue,
                    "legacy_runtime_state",
                    format!(
                        "Rust vNext does not preserve `{}` as a runtime state; OpenCode must repair or close inside its handoff lifecycle",
                        issue.state
                    ),
                    None,
                )
                .await?;
            }
            "Todo" => {
                if let Some(blocker) = nonterminal_blocker(&issue.blocked_by) {
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
                        "Todo",
                        LifecycleStage::Blocked,
                        Some(blocker_record(blocker)),
                        CleanupStatus::Clean,
                    );
                    store.upsert_issue(&record).await?;
                    report.blocked.push(issue.identifier);
                } else if has_existing_session(store, &project.id, &issue.id).await? {
                    info!(
                        project_id = %project.id,
                        issue = %issue.identifier,
                        "existing OpenCode session found; resuming issue without new session"
                    );
                    linear
                        .transition_issue(&issue.id, LinearTransition::InProgress)
                        .await?;
                    let record = issue_record(
                        project,
                        &issue,
                        "In Progress",
                        LifecycleStage::Running,
                        None,
                        CleanupStatus::Clean,
                    );
                    store.upsert_issue(&record).await?;
                } else {
                    debug!(
                        project_id = %project.id,
                        issue = %issue.identifier,
                        "Todo issue is eligible for dispatch"
                    );
                    eligible.push(issue);
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
                    &issue.state,
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
    info!(
        project_id = %project.id,
        running,
        capacity,
        eligible = eligible.len(),
        "project dispatch capacity evaluated"
    );

    for issue in eligible.into_iter().take(capacity) {
        info!(
            project_id = %project.id,
            issue = %issue.identifier,
            "dispatching issue to OpenCode"
        );
        linear
            .transition_issue(&issue.id, LinearTransition::InProgress)
            .await?;
        let launch_spec = build_acp_launch_spec(project, &issue);
        let started = opencode.launch(&launch_spec).await?;
        let record = issue_record(
            project,
            &issue,
            LinearTransition::InProgress.state_name(),
            LifecycleStage::Running,
            None,
            CleanupStatus::Clean,
        );
        store.upsert_issue(&record).await?;
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

    Ok(())
}
