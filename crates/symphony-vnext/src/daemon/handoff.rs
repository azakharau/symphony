use std::path::PathBuf;

use tracing::{debug, info, warn};

use crate::{
    config::ProjectConfig,
    linear::{LinearClient, LinearIssue, LinearIssueEvidence, LinearTransition},
    opencode::{
        OpenCodeError, OpenCodeHandoff, OpenCodeLauncher, OpenCodeStopReason,
        build_acp_launch_spec, worktree_path_allowed,
    },
    state::{
        BlockerRecord, CleanupStatus, FailureRecord, GitRefRecord, IssueStateRecord, LifecycleStage,
    },
    storage::SqliteStore,
};

use super::{
    cleanup::cleanup_worktree,
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
) -> anyhow::Result<()> {
    let Some(session) = latest_session(store, &project.id, &issue.id).await? else {
        warn!(
            project_id = %project.id,
            issue = %issue.identifier,
            "in-progress issue has no recorded OpenCode session yet"
        );
        return Ok(());
    };
    let handoff = match opencode.latest_handoff(&session).await {
        Ok(Some(handoff)) => handoff,
        Ok(None) => {
            debug!(
                project_id = %project.id,
                issue = %issue.identifier,
                session_id = %session.session_id,
                "OpenCode handoff not available yet"
            );
            return Ok(());
        }
        Err(OpenCodeError::MalformedHandoff(message)) => {
            warn!(
                project_id = %project.id,
                issue = %issue.identifier,
                session_id = %session.session_id,
                message,
                "OpenCode handoff sidecar failed validation"
            );
            request_opencode_repair(
                project,
                store,
                opencode,
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
            return Ok(());
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
        return Ok(());
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

    Ok(())
}

async fn close_successful_handoff(
    project: &ProjectConfig,
    store: &SqliteStore,
    opencode: &impl OpenCodeLauncher,
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
        request_opencode_repair(
            project,
            store,
            opencode,
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
        request_opencode_repair(
            project,
            store,
            opencode,
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
        request_opencode_repair(
            project,
            store,
            opencode,
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

    let evidence_body = git_closure_evidence_body(handoff, git);
    linear
        .record_issue_evidence(
            &issue.id,
            LinearIssueEvidence {
                kind: "opencode_git_closure".into(),
                body: evidence_body,
            },
        )
        .await?;
    linear
        .transition_issue(&issue.id, LinearTransition::Done)
        .await?;
    cleanup_worktree(&project.repo_path, &git.worktree_path).await?;
    info!(
        project_id = %project.id,
        issue = %issue.identifier,
        session_id = %session.session_id,
        branch = %git.branch,
        head_sha = git.head_sha.as_deref().unwrap_or(""),
        "OpenCode handoff accepted and issue closed"
    );
    let record = IssueStateRecord {
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
    };
    store.upsert_issue(&record).await?;
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
        let started = opencode
            .continue_repair(&spec, session, failure_fingerprint, failure_fingerprint)
            .await?;
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
    let started = opencode
        .continue_repair(&spec, session, &fingerprint, &message)
        .await?;
    let mut record = issue_record(
        project,
        issue,
        "In Progress",
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

async fn latest_session(
    store: &SqliteStore,
    project_id: &str,
    issue_id: &str,
) -> anyhow::Result<Option<crate::state::OpenCodeSessionRecord>> {
    let mut sessions = store
        .opencode_sessions_for_issue(project_id, issue_id)
        .await?;
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
        state: LinearTransition::NeedOwnerInput.state_name().into(),
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
