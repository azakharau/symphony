use std::{cmp::Ordering, fs, path::PathBuf};

use anyhow::{Context, bail};

use crate::{
    config::{ProjectConfig, RootConfig},
    linear::{EmptyLinearClient, LinearBlocker, LinearClient, LinearIssue, LinearTransition},
    state::{
        BlockerRecord, CleanupStatus, GitRefRecord, IssueStateRecord, LifecycleStage,
        OpenCodeSessionRecord,
    },
    storage::SqliteStore,
};

#[derive(Debug)]
pub struct DaemonOptions {
    pub config_path: PathBuf,
    pub database_path: PathBuf,
    pub once: bool,
}

pub fn run(options: DaemonOptions) -> anyhow::Result<()> {
    if !options.once {
        bail!(
            "continuous daemon mode is not implemented yet; pass --once for bootstrap validation"
        );
    }

    let input = fs::read_to_string(&options.config_path)
        .with_context(|| format!("read config {}", options.config_path.display()))?;
    let config = RootConfig::from_yaml_str(&input)?;
    let store = SqliteStore::open(&options.database_path)
        .with_context(|| format!("open sqlite database {}", options.database_path.display()))?;
    store.migrate()?;
    store.reconcile_projects(&config)?;
    run_once_with_linear_client(&config, &store, &EmptyLinearClient)?;

    Ok(())
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OrchestrationReport {
    pub dispatched: Vec<String>,
    pub blocked: Vec<String>,
    pub parked_owner_input: Vec<String>,
    pub terminal_reconciled: Vec<String>,
}

pub fn run_once_with_linear_client(
    config: &RootConfig,
    store: &SqliteStore,
    linear: &impl LinearClient,
) -> anyhow::Result<OrchestrationReport> {
    store.reconcile_projects(config)?;

    let mut report = OrchestrationReport::default();
    for project in config.projects().iter().filter(|project| project.enabled) {
        reconcile_project(project, store, linear, &mut report)
            .with_context(|| format!("orchestrate project `{}`", project.id))?;
    }

    Ok(report)
}

fn reconcile_project(
    project: &ProjectConfig,
    store: &SqliteStore,
    linear: &impl LinearClient,
    report: &mut OrchestrationReport,
) -> anyhow::Result<()> {
    let mut eligible = Vec::new();
    let mut issues = linear.fetch_candidate_issues(project)?;
    issues.sort_by(compare_issues_for_dispatch);

    for issue in issues {
        match issue.state.as_str() {
            "Backlog" => {
                if store.issue(&project.id, &issue.id)?.is_some() {
                    store.upsert_issue(issue_record(
                        project,
                        &issue,
                        "Backlog",
                        LifecycleStage::Queued,
                        None,
                        CleanupStatus::Clean,
                    ))?;
                }
            }
            state if is_terminal_state(state) => {
                store.upsert_issue(issue_record(
                    project,
                    &issue,
                    state,
                    LifecycleStage::Completed,
                    None,
                    CleanupStatus::Pending,
                ))?;
                report.terminal_reconciled.push(issue.identifier);
            }
            "Need Owner Input" => {
                if issue.has_new_owner_answer {
                    linear.transition_issue(&issue.id, LinearTransition::Todo)?;
                    store.upsert_issue(issue_record(
                        project,
                        &issue,
                        LinearTransition::Todo.state_name(),
                        LifecycleStage::Queued,
                        None,
                        CleanupStatus::Clean,
                    ))?;
                } else {
                    store.upsert_issue(issue_record(
                        project,
                        &issue,
                        "Need Owner Input",
                        LifecycleStage::Blocked,
                        Some(BlockerRecord {
                            kind: "owner_input".into(),
                            message: "waiting for owner-visible answer".into(),
                        }),
                        CleanupStatus::Clean,
                    ))?;
                    report.parked_owner_input.push(issue.identifier);
                }
            }
            "In Progress" => {
                store.upsert_issue(issue_record(
                    project,
                    &issue,
                    "In Progress",
                    LifecycleStage::Running,
                    None,
                    CleanupStatus::Clean,
                ))?;
            }
            "Todo" => {
                if let Some(blocker) = nonterminal_blocker(&issue.blocked_by) {
                    store.upsert_issue(issue_record(
                        project,
                        &issue,
                        "Todo",
                        LifecycleStage::Blocked,
                        Some(blocker_record(blocker)),
                        CleanupStatus::Clean,
                    ))?;
                    report.blocked.push(issue.identifier);
                } else if has_existing_session(store, &project.id, &issue.id)? {
                    store.upsert_issue(issue_record(
                        project,
                        &issue,
                        "In Progress",
                        LifecycleStage::Running,
                        None,
                        CleanupStatus::Clean,
                    ))?;
                } else {
                    eligible.push(issue);
                }
            }
            _ => {
                store.upsert_issue(issue_record(
                    project,
                    &issue,
                    &issue.state,
                    LifecycleStage::Queued,
                    None,
                    CleanupStatus::Clean,
                ))?;
            }
        }
    }

    let running = store
        .issues_for_project(&project.id)?
        .into_iter()
        .filter(|issue| issue.lifecycle_stage == LifecycleStage::Running)
        .count() as u32;
    let capacity = project.concurrency.max_sessions.saturating_sub(running) as usize;

    for issue in eligible.into_iter().take(capacity) {
        linear.transition_issue(&issue.id, LinearTransition::InProgress)?;
        store.upsert_issue(issue_record(
            project,
            &issue,
            LinearTransition::InProgress.state_name(),
            LifecycleStage::Running,
            None,
            CleanupStatus::Clean,
        ))?;
        store.upsert_opencode_session(OpenCodeSessionRecord {
            project_id: project.id.clone(),
            issue_id: issue.id.clone(),
            session_id: deterministic_session_id(&issue.id),
            agent: project.opencode.agent.clone(),
            model: project.opencode.model.clone(),
            lifecycle_stage: LifecycleStage::Running,
            last_event: Some("linear_dispatch".into()),
        })?;
        report.dispatched.push(issue.identifier);
    }

    Ok(())
}

fn issue_record(
    project: &ProjectConfig,
    issue: &LinearIssue,
    state: &str,
    lifecycle_stage: LifecycleStage,
    blocker: Option<BlockerRecord>,
    cleanup_status: CleanupStatus,
) -> IssueStateRecord {
    IssueStateRecord {
        project_id: project.id.clone(),
        issue_id: issue.id.clone(),
        identifier: issue.identifier.clone(),
        title: issue.title.clone(),
        state: state.into(),
        lifecycle_stage,
        blocker,
        failure: None,
        git_ref: issue.branch_name.as_ref().map(|branch| GitRefRecord {
            branch: branch.clone(),
            worktree_path: project
                .branch
                .worktree_root
                .join(&issue.identifier)
                .display()
                .to_string(),
            head_sha: None,
        }),
        cleanup_status,
    }
}

fn compare_issues_for_dispatch(left: &LinearIssue, right: &LinearIssue) -> Ordering {
    priority_order(left.priority)
        .cmp(&priority_order(right.priority))
        .then_with(|| left.identifier.cmp(&right.identifier))
        .then_with(|| left.id.cmp(&right.id))
}

fn priority_order(priority: Option<i64>) -> (i64, i64) {
    priority.map_or((1, i64::MAX), |priority| (0, priority))
}

fn is_terminal_state(state: &str) -> bool {
    matches!(
        state,
        "Done" | "Canceled" | "Cancelled" | "Closed" | "Duplicate"
    )
}

fn nonterminal_blocker(blockers: &[LinearBlocker]) -> Option<&LinearBlocker> {
    blockers
        .iter()
        .find(|blocker| !blocker.state.as_deref().is_some_and(is_terminal_state))
}

fn blocker_record(blocker: &LinearBlocker) -> BlockerRecord {
    let label = blocker
        .identifier
        .as_deref()
        .or(blocker.id.as_deref())
        .unwrap_or("unknown issue");
    let state = blocker.state.as_deref().unwrap_or("unknown state");
    BlockerRecord {
        kind: "linear_blocker".into(),
        message: format!("{label} is {state}"),
    }
}

fn has_existing_session(
    store: &SqliteStore,
    project_id: &str,
    issue_id: &str,
) -> anyhow::Result<bool> {
    Ok(!store
        .opencode_sessions_for_issue(project_id, issue_id)?
        .is_empty())
}

fn deterministic_session_id(issue_id: &str) -> String {
    format!("opencode:{issue_id}")
}
