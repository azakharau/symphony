use std::{
    cmp::Ordering,
    fs,
    io::{BufRead, BufReader, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    thread,
    time::Duration,
};

use anyhow::Context;

use crate::{
    api::runtime_api_json_response,
    config::{ProjectConfig, RootConfig},
    linear::{
        EmptyLinearClient, LinearBlocker, LinearClient, LinearGraphqlClient, LinearIssue,
        LinearIssueEvidence, LinearTransition,
    },
    opencode::{
        DeterministicOpenCodeLauncher, OpenCodeHandoff, OpenCodeLauncher, OpenCodeStopReason,
        StdioOpenCodeLauncher, build_acp_launch_spec, new_session_record,
    },
    state::{
        BlockerRecord, CleanupStatus, FailureRecord, GitRefRecord, IssueStateRecord, LifecycleStage,
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
    let input = fs::read_to_string(&options.config_path)
        .with_context(|| format!("read config {}", options.config_path.display()))?;
    let config = RootConfig::from_yaml_str(&input)?;
    let store = SqliteStore::open(&options.database_path)
        .with_context(|| format!("open sqlite database {}", options.database_path.display()))?;
    store.migrate()?;
    store.reconcile_projects(&config)?;

    if options.once {
        run_once_with_clients(&config, &store, &EmptyLinearClient, &StdioOpenCodeLauncher)?;
        return Ok(());
    }

    run_continuous(config, options.database_path)?;

    Ok(())
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OrchestrationReport {
    pub dispatched: Vec<String>,
    pub blocked: Vec<String>,
    pub parked_owner_input: Vec<String>,
    pub terminal_reconciled: Vec<String>,
}

fn run_continuous(config: RootConfig, database_path: PathBuf) -> anyhow::Result<()> {
    let server = config
        .server
        .clone()
        .context("continuous daemon mode requires server.host and server.port")?;
    let bind_addr = format!("{}:{}", server.host, server.port);
    let listener =
        TcpListener::bind(&bind_addr).with_context(|| format!("bind dashboard API {bind_addr}"))?;
    let poll_config = config.clone();
    let poll_database_path = database_path.clone();
    let linear = LinearGraphqlClient::from_env()?;

    thread::spawn(move || {
        loop {
            if let Ok(store) = SqliteStore::open(&poll_database_path)
                && store.migrate().is_ok()
            {
                let _ =
                    run_once_with_clients(&poll_config, &store, &linear, &StdioOpenCodeLauncher);
            }
            thread::sleep(Duration::from_secs(30));
        }
    });

    for stream in listener.incoming() {
        let stream = stream?;
        handle_http_stream(&config, &database_path, stream)?;
    }

    Ok(())
}

fn handle_http_stream(
    config: &RootConfig,
    database_path: &PathBuf,
    mut stream: TcpStream,
) -> anyhow::Result<()> {
    let mut first_line = String::new();
    {
        let mut reader = BufReader::new(&mut stream);
        reader.read_line(&mut first_line)?;
    }
    let mut parts = first_line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let path = parts.next().unwrap_or("/");

    if method != "GET" {
        write_http_response(&mut stream, 405, r#"{"error":"method_not_allowed"}"#)?;
        return Ok(());
    }

    let store = SqliteStore::open(database_path)?;
    store.migrate()?;
    let response = runtime_api_json_response(config, &store, path)?;
    write_http_response(&mut stream, response.status, &response.body)?;
    Ok(())
}

fn write_http_response(stream: &mut TcpStream, status: u16, body: &str) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Internal Server Error",
    };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

pub fn run_once_with_linear_client(
    config: &RootConfig,
    store: &SqliteStore,
    linear: &impl LinearClient,
) -> anyhow::Result<OrchestrationReport> {
    run_once_with_clients(config, store, linear, &DeterministicOpenCodeLauncher)
}

pub fn run_once_with_clients(
    config: &RootConfig,
    store: &SqliteStore,
    linear: &impl LinearClient,
    opencode: &impl OpenCodeLauncher,
) -> anyhow::Result<OrchestrationReport> {
    store.reconcile_projects(config)?;

    let mut report = OrchestrationReport::default();
    for project in config.projects().iter().filter(|project| project.enabled) {
        reconcile_project(project, store, linear, opencode, &mut report)
            .with_context(|| format!("orchestrate project `{}`", project.id))?;
    }

    Ok(report)
}

fn reconcile_project(
    project: &ProjectConfig,
    store: &SqliteStore,
    linear: &impl LinearClient,
    opencode: &impl OpenCodeLauncher,
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
                let existing = store.issue(&project.id, &issue.id)?;
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
                store.upsert_issue(record)?;
                process_in_progress_handoff(project, store, linear, opencode, &issue, existing)?;
            }
            "Preparing" | "In Review" | "RCA Required" => {
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
                )?;
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
                    linear.transition_issue(&issue.id, LinearTransition::InProgress)?;
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
        let launch_spec = build_acp_launch_spec(project, &issue);
        let started = opencode.launch(&launch_spec)?;
        store.upsert_issue(issue_record(
            project,
            &issue,
            LinearTransition::InProgress.state_name(),
            LifecycleStage::Running,
            None,
            CleanupStatus::Clean,
        ))?;
        store.upsert_opencode_session(new_session_record(
            project,
            &issue,
            started,
            &launch_spec,
        ))?;
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
            pr_url: None,
        }),
        cleanup_status,
    }
}

fn process_in_progress_handoff(
    project: &ProjectConfig,
    store: &SqliteStore,
    linear: &impl LinearClient,
    opencode: &impl OpenCodeLauncher,
    issue: &LinearIssue,
    existing_issue: Option<IssueStateRecord>,
) -> anyhow::Result<()> {
    let Some(session) = latest_session(store, &project.id, &issue.id)? else {
        return Ok(());
    };
    let Some(handoff) = opencode.latest_handoff(&session)? else {
        return Ok(());
    };

    if handoff.session_id != session.session_id {
        park_need_owner_input(
            project,
            store,
            linear,
            issue,
            "malformed_handoff",
            format!(
                "handoff session `{}` did not match active session `{}`",
                handoff.session_id, session.session_id
            ),
            Some(FailureRecord {
                kind: "malformed_handoff".into(),
                message: "session id mismatch".into(),
                fingerprint: Some("session_id_mismatch".into()),
                occurrence_count: 1,
            }),
        )?;
        return Ok(());
    }

    match &handoff.stop_reason {
        OpenCodeStopReason::Success => {
            if let Some(message) = successful_handoff_error(&handoff) {
                park_need_owner_input(
                    project,
                    store,
                    linear,
                    issue,
                    "malformed_handoff",
                    message.clone(),
                    Some(FailureRecord {
                        kind: "malformed_handoff".into(),
                        message,
                        fingerprint: Some("incomplete_success_handoff".into()),
                        occurrence_count: 1,
                    }),
                )?;
                return Ok(());
            }

            let git = handoff.git.as_ref().expect("validated git evidence");
            linear.record_issue_evidence(
                &issue.id,
                LinearIssueEvidence {
                    kind: "opencode_git_closure".into(),
                    body: git_closure_evidence_body(&handoff),
                },
            )?;
            linear.transition_issue(&issue.id, LinearTransition::Done)?;
            cleanup_worktree(&git.worktree_path)?;
            store.upsert_issue(IssueStateRecord {
                project_id: project.id.clone(),
                issue_id: issue.id.clone(),
                identifier: issue.identifier.clone(),
                title: issue.title.clone(),
                state: LinearTransition::Done.state_name().into(),
                lifecycle_stage: LifecycleStage::Completed,
                blocker: None,
                failure: None,
                git_ref: Some(GitRefRecord {
                    branch: git.branch.clone(),
                    worktree_path: git.worktree_path.clone(),
                    head_sha: git.head_sha.clone(),
                    pr_url: git.pr_url.clone(),
                }),
                cleanup_status: CleanupStatus::Complete,
            })?;
        }
        OpenCodeStopReason::EvalFailed {
            failure_fingerprint,
        } => {
            let previous_count = matching_failure_count(
                existing_issue
                    .as_ref()
                    .and_then(|issue| issue.failure.as_ref()),
                failure_fingerprint,
            );
            let occurrence_count = previous_count.saturating_add(1);
            let max_identical = project.eval.max_identical_failure_fingerprints.max(1);
            if occurrence_count >= max_identical {
                park_need_owner_input(
                    project,
                    store,
                    linear,
                    issue,
                    "repeated_eval_failure",
                    format!("OpenCode reported `{failure_fingerprint}` {occurrence_count} times"),
                    Some(FailureRecord {
                        kind: "eval_failure".into(),
                        message: failure_fingerprint.clone(),
                        fingerprint: Some(failure_fingerprint.clone()),
                        occurrence_count,
                    }),
                )?;
            } else {
                opencode.continue_repair(&session, failure_fingerprint)?;
                let mut record = issue_record(
                    project,
                    issue,
                    "In Progress",
                    LifecycleStage::Running,
                    None,
                    CleanupStatus::Clean,
                );
                record.failure = Some(FailureRecord {
                    kind: "eval_failure".into(),
                    message: failure_fingerprint.clone(),
                    fingerprint: Some(failure_fingerprint.clone()),
                    occurrence_count,
                });
                store.upsert_issue(record)?;
            }
        }
        OpenCodeStopReason::ProviderBlocker { message } => {
            park_need_owner_input(
                project,
                store,
                linear,
                issue,
                "provider_blocker",
                message.clone(),
                Some(FailureRecord {
                    kind: "provider_blocker".into(),
                    message: message.clone(),
                    fingerprint: Some(stable_fingerprint(message)),
                    occurrence_count: 1,
                }),
            )?;
        }
        OpenCodeStopReason::OwnerQuestion { question } => {
            park_need_owner_input(
                project,
                store,
                linear,
                issue,
                "owner_question",
                question.clone(),
                None,
            )?;
        }
    }

    Ok(())
}

fn latest_session(
    store: &SqliteStore,
    project_id: &str,
    issue_id: &str,
) -> anyhow::Result<Option<crate::state::OpenCodeSessionRecord>> {
    let mut sessions = store.opencode_sessions_for_issue(project_id, issue_id)?;
    Ok(sessions.pop())
}

fn successful_handoff_error(handoff: &OpenCodeHandoff) -> Option<String> {
    if handoff.eval_results.is_empty() {
        return Some("successful handoff did not include eval results".into());
    }
    if let Some(eval) = handoff.eval_results.iter().find(|eval| !eval.passed) {
        return Some(format!("eval `{}` did not pass", eval.suite));
    }
    if handoff.changed_files.is_empty() {
        return Some("successful handoff did not include changed files".into());
    }
    let Some(git) = &handoff.git else {
        return Some("successful handoff did not include git closure evidence".into());
    };
    if git.branch.trim().is_empty() {
        return Some("git closure evidence did not include a branch".into());
    }
    if git
        .head_sha
        .as_deref()
        .is_none_or(|head_sha| head_sha.trim().is_empty())
    {
        return Some("git closure evidence did not include a commit SHA".into());
    }
    if git.worktree_path.trim().is_empty() {
        return Some("git closure evidence did not include a worktree path".into());
    }

    None
}

fn park_need_owner_input(
    project: &ProjectConfig,
    store: &SqliteStore,
    linear: &impl LinearClient,
    issue: &LinearIssue,
    blocker_kind: &str,
    message: String,
    failure: Option<FailureRecord>,
) -> anyhow::Result<()> {
    linear.record_issue_evidence(
        &issue.id,
        LinearIssueEvidence {
            kind: blocker_kind.into(),
            body: message.clone(),
        },
    )?;
    linear.transition_issue(&issue.id, LinearTransition::NeedOwnerInput)?;
    store.upsert_issue(IssueStateRecord {
        project_id: project.id.clone(),
        issue_id: issue.id.clone(),
        identifier: issue.identifier.clone(),
        title: issue.title.clone(),
        state: LinearTransition::NeedOwnerInput.state_name().into(),
        lifecycle_stage: LifecycleStage::Blocked,
        blocker: Some(BlockerRecord {
            kind: blocker_kind.into(),
            message,
        }),
        failure,
        git_ref: None,
        cleanup_status: CleanupStatus::Clean,
    })?;
    Ok(())
}

fn git_closure_evidence_body(handoff: &OpenCodeHandoff) -> String {
    let git = handoff.git.as_ref().expect("validated git evidence");
    format!(
        "session_id: {}\nbranch: {}\nhead_sha: {}\npr_url: {}\nchanged_files: {}\nevals: {}\nrisks: {}",
        handoff.session_id,
        git.branch,
        git.head_sha.as_deref().unwrap_or(""),
        git.pr_url.as_deref().unwrap_or("none"),
        handoff.changed_files.join(", "),
        handoff
            .eval_results
            .iter()
            .map(|eval| format!(
                "{}={}",
                eval.suite,
                if eval.passed { "passed" } else { "failed" }
            ))
            .collect::<Vec<_>>()
            .join(", "),
        if handoff.risks.is_empty() {
            "none".into()
        } else {
            handoff.risks.join(", ")
        },
    )
}

fn cleanup_worktree(worktree_path: &str) -> anyhow::Result<()> {
    let path = PathBuf::from(worktree_path);
    if path.exists() {
        fs::remove_dir_all(&path)
            .with_context(|| format!("remove accepted worktree {}", path.display()))?;
    }
    Ok(())
}

fn matching_failure_count(failure: Option<&FailureRecord>, fingerprint: &str) -> u32 {
    failure
        .filter(|failure| {
            failure.kind == "eval_failure"
                && failure.fingerprint.as_deref().unwrap_or(&failure.message) == fingerprint
        })
        .map(|failure| failure.occurrence_count.max(1))
        .unwrap_or(0)
}

fn stable_fingerprint(input: &str) -> String {
    input
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect()
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
