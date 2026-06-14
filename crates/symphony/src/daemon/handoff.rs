use std::path::{Path, PathBuf};

use tokio::process::Command;
use tracing::{debug, info, warn};

use super::{process_elapsed_seconds, session_has_live_process, terminate_current_session_process};
use crate::{
    config::ProjectConfig,
    linear::{LinearClient, LinearIssue, LinearIssueEvidence, LinearTransition},
    opencode::{
        OpenCodeError, OpenCodeHandoff, OpenCodeLauncher, OpenCodeStopReason,
        build_acp_launch_spec, worktree_path_allowed,
    },
    state::{
        BlockerRecord, CleanupStatus, FailureRecord, GitRefRecord, IssueStateRecord,
        LifecycleStage, OpenCodeStage,
    },
    storage::SqliteStore,
};

use super::{
    cleanup::cleanup_worktree,
    git_closure::{GitClosureResult, verify_and_integrate_git_closure},
    policy::{matching_failure_count, stable_fingerprint},
    records::{git_closure_evidence_body, issue_record},
};

pub(super) async fn process_in_progress_handoff(
    project: &ProjectConfig,
    store: &SqliteStore,
    linear: &impl LinearClient,
    opencode: &impl OpenCodeLauncher,
    issue: &LinearIssue,
    existing_issue: Option<IssueStateRecord>,
) -> anyhow::Result<bool> {
    let Some(session) = latest_session(store, &project.id, &issue.id).await? else {
        warn!(
            project_id = %project.id,
            issue = %issue.identifier,
            reason = "missing_active_session",
            "in-progress issue has no active OpenCode session yet"
        );
        return Ok(false);
    };
    let handoff = match opencode.latest_handoff(&session).await {
        Ok(Some(handoff)) => handoff,
        Ok(None) => {
            if session_has_live_process(&session).await {
                debug!(
                    project_id = %project.id,
                    issue = %issue.identifier,
                    session_id = %session.session_id,
                    "OpenCode handoff not available yet"
                );
                return Ok(false);
            }

            let message = ".symphony/opencode-handoff.json was not produced before the OpenCode ACP process ended".to_string();
            warn!(
                project_id = %project.id,
                issue = %issue.identifier,
                session_id = %session.session_id,
                process_id = session.process_id,
                message,
                "OpenCode session ended without handoff sidecar"
            );
            fail_runtime_defect(
                project,
                store,
                linear,
                issue,
                "malformed_handoff",
                message.clone(),
                FailureRecord {
                    kind: "malformed_handoff".into(),
                    message,
                    fingerprint: Some("missing_handoff_sidecar".into()),
                    occurrence_count: 1,
                },
                &session,
            )
            .await?;
            return Ok(true);
        }
        Err(OpenCodeError::MalformedHandoff(message)) => {
            warn!(
                project_id = %project.id,
                issue = %issue.identifier,
                session_id = %session.session_id,
                message,
                "OpenCode handoff sidecar failed validation"
            );
            fail_runtime_defect(
                project,
                store,
                linear,
                issue,
                "malformed_handoff",
                message.clone(),
                FailureRecord {
                    kind: "malformed_handoff".into(),
                    message,
                    fingerprint: Some("malformed_handoff_sidecar".into()),
                    occurrence_count: 1,
                },
                &session,
            )
            .await?;
            return Ok(true);
        }
        Err(error) => return Err(error.into()),
    };
    info!(
        project_id = %project.id,
        issue = %issue.identifier,
        session_id = %session.session_id,
        stop_reason = ?handoff.stop_reason,
        "OpenCode handoff observed"
    );

    if handoff.session_id != session.session_id {
        warn!(
            project_id = %project.id,
            issue = %issue.identifier,
            expected_session_id = %session.session_id,
            handoff_session_id = %handoff.session_id,
            "malformed OpenCode handoff session mismatch"
        );
        request_opencode_repair(
            project,
            store,
            opencode,
            linear,
            issue,
            "malformed_handoff",
            format!(
                "handoff session `{}` did not match active session `{}`",
                handoff.session_id, session.session_id
            ),
            FailureRecord {
                kind: "malformed_handoff".into(),
                message: "session id mismatch".into(),
                fingerprint: Some("session_id_mismatch".into()),
                occurrence_count: 1,
            },
            &session,
        )
        .await?;
        return Ok(true);
    }

    match &handoff.stop_reason {
        OpenCodeStopReason::Success => {
            close_successful_handoff(project, store, opencode, linear, issue, &session, &handoff)
                .await?;
        }
        OpenCodeStopReason::EvalFailed {
            failure_fingerprint,
        } => {
            handle_eval_failure(
                project,
                store,
                opencode,
                linear,
                issue,
                &session,
                existing_issue.as_ref(),
                failure_fingerprint,
            )
            .await?;
        }
        OpenCodeStopReason::ProviderBlocker { message } => {
            warn!(
                project_id = %project.id,
                issue = %issue.identifier,
                session_id = %session.session_id,
                "OpenCode provider blocker parked issue"
            );
            park_need_owner_input(
                project,
                store,
                linear,
                issue,
                Some(&session),
                "provider_blocker",
                message.clone(),
                Some(FailureRecord {
                    kind: "provider_blocker".into(),
                    message: message.clone(),
                    fingerprint: Some(stable_fingerprint(message)),
                    occurrence_count: 1,
                }),
            )
            .await?;
        }
        OpenCodeStopReason::OwnerQuestion { question } => {
            warn!(
                project_id = %project.id,
                issue = %issue.identifier,
                session_id = %session.session_id,
                "OpenCode owner question parked issue"
            );
            park_need_owner_input(
                project,
                store,
                linear,
                issue,
                Some(&session),
                "owner_question",
                question.clone(),
                None,
            )
            .await?;
        }
    }

    Ok(true)
}

async fn close_successful_handoff(
    project: &ProjectConfig,
    store: &SqliteStore,
    _opencode: &impl OpenCodeLauncher,
    linear: &impl LinearClient,
    issue: &LinearIssue,
    session: &crate::state::OpenCodeSessionRecord,
    handoff: &OpenCodeHandoff,
) -> anyhow::Result<()> {
    if let Some(message) = successful_handoff_error(handoff) {
        warn!(
            project_id = %project.id,
            issue = %issue.identifier,
            session_id = %session.session_id,
            message,
            "successful OpenCode handoff failed validation"
        );
        fail_runtime_defect(
            project,
            store,
            linear,
            issue,
            "malformed_handoff",
            message.clone(),
            FailureRecord {
                kind: "malformed_handoff".into(),
                message,
                fingerprint: Some("incomplete_success_handoff".into()),
                occurrence_count: 1,
            },
            session,
        )
        .await?;
        return Ok(());
    }

    let Some(git) = handoff.git.as_ref() else {
        warn!(
            project_id = %project.id,
            issue = %issue.identifier,
            session_id = %session.session_id,
            "successful OpenCode handoff missing git evidence"
        );
        fail_runtime_defect(
            project,
            store,
            linear,
            issue,
            "malformed_handoff",
            "successful handoff did not include git closure evidence".into(),
            FailureRecord {
                kind: "malformed_handoff".into(),
                message: "missing git closure evidence".into(),
                fingerprint: Some("missing_git_closure".into()),
                occurrence_count: 1,
            },
            session,
        )
        .await?;
        return Ok(());
    };
    if let Some(message) = successful_handoff_worktree_error(project, session, git) {
        warn!(
            project_id = %project.id,
            issue = %issue.identifier,
            session_id = %session.session_id,
            message,
            "successful OpenCode handoff has unsafe worktree evidence"
        );
        fail_runtime_defect(
            project,
            store,
            linear,
            issue,
            "malformed_handoff",
            message.clone(),
            FailureRecord {
                kind: "malformed_handoff".into(),
                message,
                fingerprint: Some("unsafe_worktree_path".into()),
                occurrence_count: 1,
            },
            session,
        )
        .await?;
        return Ok(());
    }

    let integration =
        match verify_and_integrate_git_closure(project, git, &handoff.changed_files).await {
            Ok(integration) => integration,
            Err(error) => {
                let message = error.to_string();
                warn!(
                    project_id = %project.id,
                    issue = %issue.identifier,
                    session_id = %session.session_id,
                    message,
                    "successful OpenCode handoff failed git closure verification"
                );
                fail_runtime_defect(
                    project,
                    store,
                    linear,
                    issue,
                    "malformed_handoff",
                    message.clone(),
                    FailureRecord {
                        kind: "malformed_handoff".into(),
                        message,
                        fingerprint: Some("git_closure_unverified".into()),
                        occurrence_count: 1,
                    },
                    session,
                )
                .await?;
                return Ok(());
            }
        };

    let integrated_base = match &integration {
        GitClosureResult::NoGitChanges => None,
        GitClosureResult::Integrated { base_branch } => Some(base_branch.as_str()),
    };
    let evidence_body = git_closure_evidence_body(handoff, git, integrated_base);
    linear
        .record_issue_evidence(
            &issue.id,
            LinearIssueEvidence {
                kind: "opencode_git_closure".into(),
                body: evidence_body,
            },
        )
        .await?;
    let mut terminating_session = session.clone();
    terminate_current_session_process(project, issue, &mut terminating_session).await?;
    linear
        .transition_issue(&issue.id, LinearTransition::Done)
        .await?;
    let cleanup_status = match cleanup_worktree(&project.repo_path, &git.worktree_path).await {
        Ok(()) => CleanupStatus::Complete,
        Err(error) => {
            warn!(
                project_id = %project.id,
                issue = %issue.identifier,
                session_id = %session.session_id,
                worktree_path = %git.worktree_path,
                error = %error,
                "accepted OpenCode handoff cleanup failed after Done transition"
            );
            CleanupStatus::Failed
        }
    };
    info!(
        project_id = %project.id,
        issue = %issue.identifier,
        session_id = %session.session_id,
        branch = %git.branch,
        head_sha = git.head_sha.as_deref().unwrap_or(""),
        cleanup = %cleanup_status,
        "OpenCode handoff accepted and issue closed"
    );
    let record = IssueStateRecord {
        project_id: project.id.clone(),
        issue_id: issue.id.clone(),
        identifier: issue.identifier.clone(),
        title: issue.title.clone(),
        lifecycle_stage: LifecycleStage::Completed,
        blocker: None,
        failure: None,
        git_ref: Some(GitRefRecord {
            branch: git.branch.clone(),
            worktree_path: git.worktree_path.clone(),
            head_sha: git.head_sha.clone(),
            pr_url: git.pr_url.clone(),
        }),
        cleanup_status,
    };
    store.upsert_issue(&record).await?;
    let mut completed_session = session.clone();
    completed_session.process_id = None;
    completed_session.lifecycle_stage = LifecycleStage::Completed;
    completed_session.stage = crate::state::OpenCodeStage::Completed;
    completed_session.lifecycle_marker = Some("handoff_accepted".into());
    completed_session.last_event = Some(match cleanup_status {
        CleanupStatus::Complete => "issue_closed".into(),
        CleanupStatus::Failed => "issue_closed_cleanup_failed".into(),
        _ => "issue_closed_cleanup_unknown".into(),
    });
    completed_session.silence_observed = false;
    store.upsert_opencode_session(&completed_session).await?;
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "handoff repair decision needs project, adapters, issue, session, and current failure evidence"
)]
async fn handle_eval_failure(
    project: &ProjectConfig,
    store: &SqliteStore,
    opencode: &impl OpenCodeLauncher,
    linear: &impl LinearClient,
    issue: &LinearIssue,
    session: &crate::state::OpenCodeSessionRecord,
    existing_issue: Option<&IssueStateRecord>,
    failure_fingerprint: &str,
) -> anyhow::Result<()> {
    let previous_count = matching_failure_count(
        existing_issue.and_then(|issue| issue.failure.as_ref()),
        failure_fingerprint,
    );
    let occurrence_count = previous_count.saturating_add(1);
    let max_identical = project.eval.max_identical_failure_fingerprints.max(1);
    if occurrence_count >= max_identical {
        warn!(
            project_id = %project.id,
            issue = %issue.identifier,
            session_id = %session.session_id,
            failure_fingerprint,
            occurrence_count,
            max_identical,
            "OpenCode repeated eval failure reached parking threshold"
        );
        park_need_owner_input(
            project,
            store,
            linear,
            issue,
            Some(session),
            "repeated_eval_failure",
            format!("OpenCode reported `{failure_fingerprint}` {occurrence_count} times"),
            Some(FailureRecord {
                kind: "eval_failure".into(),
                message: failure_fingerprint.into(),
                fingerprint: Some(failure_fingerprint.into()),
                occurrence_count,
            }),
        )
        .await?;
    } else {
        info!(
            project_id = %project.id,
            issue = %issue.identifier,
            session_id = %session.session_id,
            failure_fingerprint,
            occurrence_count,
            max_identical,
            "continuing OpenCode repair after eval failure"
        );
        let spec = build_acp_launch_spec(project, issue);
        let mut terminating_session = session.clone();
        terminate_current_session_process(project, issue, &mut terminating_session).await?;
        let started = opencode
            .continue_repair(&spec, session, failure_fingerprint, failure_fingerprint)
            .await?;
        let mut record = issue_record(
            project,
            issue,
            LifecycleStage::Running,
            None,
            CleanupStatus::Clean,
        );
        record.failure = Some(FailureRecord {
            kind: "eval_failure".into(),
            message: failure_fingerprint.into(),
            fingerprint: Some(failure_fingerprint.into()),
            occurrence_count,
        });
        store.upsert_issue(&record).await?;
        let mut repair_session = session.clone();
        repair_session.process_id = started.process_id;
        repair_session.lifecycle_stage = LifecycleStage::Running;
        repair_session.stage = crate::state::OpenCodeStage::Running;
        repair_session.lifecycle_marker = Some("repair_prompted".into());
        repair_session.last_event = Some(format!("repair_prompted:{failure_fingerprint}"));
        repair_session.silence_observed = false;
        store.upsert_opencode_session(&repair_session).await?;
    }
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "repair request needs project, adapters, issue, session, and failure evidence"
)]
async fn request_opencode_repair(
    project: &ProjectConfig,
    store: &SqliteStore,
    opencode: &impl OpenCodeLauncher,
    linear: &impl LinearClient,
    issue: &LinearIssue,
    evidence_kind: &str,
    message: String,
    failure: FailureRecord,
    session: &crate::state::OpenCodeSessionRecord,
) -> anyhow::Result<()> {
    linear
        .record_issue_evidence(
            &issue.id,
            LinearIssueEvidence {
                kind: evidence_kind.into(),
                body: message.clone(),
            },
        )
        .await?;
    let fingerprint = failure
        .fingerprint
        .as_deref()
        .unwrap_or(evidence_kind)
        .to_string();
    let spec = build_acp_launch_spec(project, issue);
    let mut terminating_session = session.clone();
    terminate_current_session_process(project, issue, &mut terminating_session).await?;
    let started = opencode
        .continue_repair(&spec, session, &fingerprint, &message)
        .await?;
    let mut record = issue_record(
        project,
        issue,
        LifecycleStage::Running,
        None,
        CleanupStatus::Clean,
    );
    record.failure = Some(failure);
    store.upsert_issue(&record).await?;

    let mut repair_session = session.clone();
    repair_session.process_id = started.process_id;
    repair_session.lifecycle_stage = LifecycleStage::Running;
    repair_session.stage = crate::state::OpenCodeStage::Running;
    repair_session.lifecycle_marker = Some("repair_prompted".into());
    repair_session.last_event = Some(format!("repair_prompted:{fingerprint}"));
    repair_session.silence_observed = false;
    store.upsert_opencode_session(&repair_session).await?;
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "runtime defect closure needs project, adapters, issue, failure evidence, and active session"
)]
async fn fail_runtime_defect(
    project: &ProjectConfig,
    store: &SqliteStore,
    linear: &impl LinearClient,
    issue: &LinearIssue,
    evidence_kind: &str,
    message: String,
    failure: FailureRecord,
    session: &crate::state::OpenCodeSessionRecord,
) -> anyhow::Result<()> {
    let elapsed_seconds = match session.process_id {
        Some(process_id) => process_elapsed_seconds(process_id).await,
        None => None,
    };
    let git_snapshot = RuntimeDefectGitSnapshot::capture(session).await;
    linear
        .record_issue_evidence(
            &issue.id,
            LinearIssueEvidence {
                kind: evidence_kind.into(),
                body: runtime_defect_evidence_body(
                    &message,
                    session,
                    elapsed_seconds,
                    git_snapshot.as_ref(),
                ),
            },
        )
        .await?;
    let mut terminating_session = session.clone();
    terminate_current_session_process(project, issue, &mut terminating_session).await?;
    linear
        .transition_issue(&issue.id, LinearTransition::Todo)
        .await?;

    let mut record = issue_record(
        project,
        issue,
        LifecycleStage::Failed,
        None,
        CleanupStatus::Clean,
    );
    record.failure = Some(failure.clone());
    record.git_ref = git_snapshot
        .as_ref()
        .and_then(RuntimeDefectGitSnapshot::git_ref);
    store.upsert_issue(&record).await?;

    let mut failed_session = session.clone();
    failed_session.lifecycle_stage = LifecycleStage::Failed;
    failed_session.stage = crate::state::OpenCodeStage::Failed;
    failed_session.lifecycle_marker = Some(format!("failed:{}", failure.kind));
    let failure_event = format!(
        "failed:{}",
        failure
            .fingerprint
            .as_deref()
            .unwrap_or(failure.kind.as_str())
    );
    failed_session.last_event = Some(git_snapshot.as_ref().map_or_else(
        || failure_event.clone(),
        |snapshot| snapshot.failure_event(&failure_event),
    ));
    failed_session.silence_observed = false;
    store.upsert_opencode_session(&failed_session).await?;
    Ok(())
}

fn runtime_defect_evidence_body(
    message: &str,
    session: &crate::state::OpenCodeSessionRecord,
    elapsed_seconds: Option<u64>,
    git_snapshot: Option<&RuntimeDefectGitSnapshot>,
) -> String {
    let git_snapshot = git_snapshot
        .map(RuntimeDefectGitSnapshot::evidence_body)
        .unwrap_or_else(|| "git_snapshot: unavailable".into());

    format!(
        "Symphony runtime defect: {message}\n\nsession_id: {session_id}\nprocess_id: {process_id}\nelapsed_seconds: {elapsed_seconds}\n\n{git_snapshot}\n\nThe issue was moved back to Todo and the OpenCode session row was marked failed. This is not owner input; fix the runner/tooling defect before retrying.",
        session_id = session.session_id,
        process_id = session
            .process_id
            .map(|process_id| process_id.to_string())
            .unwrap_or_else(|| "none".into()),
        elapsed_seconds = elapsed_seconds
            .map(|seconds| seconds.to_string())
            .unwrap_or_else(|| "unknown".into()),
    )
}

#[derive(Debug)]
struct RuntimeDefectGitSnapshot {
    worktree_path: String,
    branch: Option<String>,
    head_sha: Option<String>,
    status_short: Option<String>,
    head_changed_files: Option<String>,
    upstream: Option<String>,
    unpushed_commits: Option<u64>,
}

impl RuntimeDefectGitSnapshot {
    async fn capture(session: &crate::state::OpenCodeSessionRecord) -> Option<Self> {
        let worktree_path = session.worktree_path.trim();
        if worktree_path.is_empty() {
            return None;
        }

        let path = Path::new(worktree_path);
        if !tokio::fs::try_exists(path).await.ok()? {
            return None;
        }

        let is_worktree = git_output(path, ["rev-parse", "--is-inside-work-tree"])
            .await
            .is_some_and(|output| output == "true");
        if !is_worktree {
            return None;
        }

        let upstream = git_output(
            path,
            ["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
        )
        .await;
        let unpushed_commits = if upstream.is_some() {
            git_output(path, ["rev-list", "--count", "@{u}..HEAD"])
                .await
                .and_then(|output| output.parse::<u64>().ok())
        } else {
            None
        };

        Some(Self {
            worktree_path: worktree_path.into(),
            branch: git_output(path, ["branch", "--show-current"]).await,
            head_sha: git_output(path, ["rev-parse", "HEAD"]).await,
            status_short: git_output(path, ["status", "--short", "--branch"]).await,
            head_changed_files: git_output(
                path,
                ["diff-tree", "--no-commit-id", "--name-only", "-r", "HEAD"],
            )
            .await,
            upstream,
            unpushed_commits,
        })
    }

    fn git_ref(&self) -> Option<GitRefRecord> {
        let branch = self.branch.as_deref()?.trim();
        if branch.is_empty() {
            return None;
        }

        Some(GitRefRecord {
            branch: branch.into(),
            worktree_path: self.worktree_path.clone(),
            head_sha: self.head_sha.clone(),
            pr_url: None,
        })
    }

    fn evidence_body(&self) -> String {
        let mut body = format!(
            "git_snapshot:\nworktree_path: {worktree_path}\nbranch: {branch}\nhead_sha: {head_sha}\nupstream: {upstream}\nunpushed_commits: {unpushed_commits}",
            worktree_path = self.worktree_path,
            branch = self.branch.as_deref().unwrap_or("unknown"),
            head_sha = self.head_sha.as_deref().unwrap_or("unknown"),
            upstream = self.upstream.as_deref().unwrap_or("none"),
            unpushed_commits = self
                .unpushed_commits
                .map(|count| count.to_string())
                .unwrap_or_else(|| "unknown".into()),
        );

        if let Some(status) = &self.status_short {
            body.push_str("\nstatus_short:\n");
            body.push_str(status);
        }
        if let Some(files) = &self.head_changed_files {
            body.push_str("\nhead_changed_files:\n");
            body.push_str(files);
        }

        body
    }

    fn failure_event(&self, default_event: &str) -> String {
        let Some(head_sha) = self.head_sha.as_deref() else {
            return default_event.into();
        };
        let short_sha = head_sha.get(..12).unwrap_or(head_sha);
        format!("{default_event}:git_head:{short_sha}")
    }
}

async fn git_output<const N: usize>(worktree_path: &Path, args: [&str; N]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(worktree_path)
        .args(args)
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let output = String::from_utf8(output.stdout).ok()?;
    let output = output.trim().to_string();
    if output.is_empty() {
        None
    } else {
        Some(output)
    }
}

async fn latest_session(
    store: &SqliteStore,
    project_id: &str,
    issue_id: &str,
) -> anyhow::Result<Option<crate::state::OpenCodeSessionRecord>> {
    let mut sessions: Vec<_> = store
        .opencode_sessions_for_issue(project_id, issue_id)
        .await?
        .into_iter()
        .filter(|session| {
            session.lifecycle_stage == LifecycleStage::Running
                && !matches!(
                    session.stage,
                    OpenCodeStage::Failed | OpenCodeStage::Completed
                )
        })
        .collect();
    Ok(sessions.pop())
}

fn successful_handoff_error(handoff: &OpenCodeHandoff) -> Option<String> {
    if handoff.eval_results.is_empty() {
        return Some("successful handoff did not include eval results".into());
    }
    if let Some(eval) = handoff.eval_results.iter().find(|eval| !eval.passed) {
        return Some(format!("eval `{}` did not pass", eval.suite));
    }
    let Some(git) = &handoff.git else {
        return Some("successful handoff did not include git closure evidence".into());
    };
    if git.branch.trim().is_empty() {
        return Some("git closure evidence did not include a branch".into());
    }
    if !handoff.changed_files.is_empty()
        && git
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

fn successful_handoff_worktree_error(
    project: &ProjectConfig,
    session: &crate::state::OpenCodeSessionRecord,
    git: &crate::opencode::GitClosureEvidence,
) -> Option<String> {
    let raw_path = git.worktree_path.as_str();
    let trimmed_path = raw_path.trim();
    if raw_path != trimmed_path {
        return Some("git closure worktree path included leading or trailing whitespace".into());
    }

    let path = PathBuf::from(trimmed_path);
    if path.as_os_str().is_empty() {
        return Some("git closure evidence did not include a worktree path".into());
    }
    if !worktree_path_allowed(&project.branch.worktree_root, &path) {
        return Some(format!(
            "git closure worktree path `{}` is outside configured worktree root `{}`",
            path.display(),
            project.branch.worktree_root.display()
        ));
    }
    let active_path = PathBuf::from(session.worktree_path.trim());
    if path != active_path {
        return Some(format!(
            "git closure worktree path `{}` does not match active session worktree `{}`",
            path.display(),
            active_path.display()
        ));
    }

    None
}

#[expect(
    clippy::too_many_arguments,
    reason = "parking needs project, adapters, issue, session, owner-visible reason, and durable failure evidence"
)]
pub(super) async fn park_need_owner_input(
    project: &ProjectConfig,
    store: &SqliteStore,
    linear: &impl LinearClient,
    issue: &LinearIssue,
    session: Option<&crate::state::OpenCodeSessionRecord>,
    blocker_kind: &str,
    message: String,
    failure: Option<FailureRecord>,
) -> anyhow::Result<()> {
    let owner_visible_body = owner_visible_parking_body(blocker_kind, &message);
    linear
        .record_issue_evidence(
            &issue.id,
            LinearIssueEvidence {
                kind: blocker_kind.into(),
                body: owner_visible_body,
            },
        )
        .await?;
    linear
        .transition_issue(&issue.id, LinearTransition::NeedOwnerInput)
        .await?;
    let record = IssueStateRecord {
        project_id: project.id.clone(),
        issue_id: issue.id.clone(),
        identifier: issue.identifier.clone(),
        title: issue.title.clone(),
        lifecycle_stage: LifecycleStage::Blocked,
        blocker: Some(BlockerRecord {
            kind: blocker_kind.into(),
            message,
            observed_at: issue.updated_at.clone(),
        }),
        failure,
        git_ref: None,
        cleanup_status: CleanupStatus::Clean,
    };
    store.upsert_issue(&record).await?;
    if let Some(session) = session {
        let mut parked_session = session.clone();
        parked_session.process_id = None;
        parked_session.lifecycle_stage = LifecycleStage::Blocked;
        parked_session.stage = crate::state::OpenCodeStage::Failed;
        parked_session.lifecycle_marker = Some("parked".into());
        parked_session.last_event = Some(format!("parked:{blocker_kind}"));
        store.upsert_opencode_session(&parked_session).await?;
    }
    Ok(())
}

fn owner_visible_parking_body(blocker_kind: &str, message: &str) -> String {
    if blocker_kind == "owner_question" {
        return message.to_string();
    }

    format!(
        "{message}\n\nOwner input needed: decide whether to keep this issue parked, change provider/runtime configuration, or move it back to Todo for another implementation attempt."
    )
}
