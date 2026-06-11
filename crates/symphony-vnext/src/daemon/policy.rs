use std::cmp::Ordering;

use crate::{
    linear::{LinearBlocker, LinearIssue},
    state::{BlockerRecord, FailureRecord, IssueStateRecord},
    storage::SqliteStore,
};

pub(super) fn matching_failure_count(failure: Option<&FailureRecord>, fingerprint: &str) -> u32 {
    failure
        .filter(|failure| {
            failure.kind == "eval_failure"
                && failure.fingerprint.as_deref().unwrap_or(&failure.message) == fingerprint
        })
        .map(|failure| failure.occurrence_count.max(1))
        .unwrap_or(0)
}

pub(super) fn stable_fingerprint(input: &str) -> String {
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

pub(super) fn recoverable_opencode_failure(failure: &FailureRecord) -> bool {
    matches!(failure.kind.as_str(), "malformed_handoff" | "eval_failure")
}

pub(super) fn compare_issues_for_dispatch(left: &LinearIssue, right: &LinearIssue) -> Ordering {
    priority_order(left.priority)
        .cmp(&priority_order(right.priority))
        .then_with(|| left.identifier.cmp(&right.identifier))
        .then_with(|| left.id.cmp(&right.id))
}

fn priority_order(priority: Option<i64>) -> (i64, i64) {
    priority.map_or((1, i64::MAX), |priority| (0, priority))
}

pub(super) fn is_terminal_state(state: &str) -> bool {
    matches!(state, "Done" | "Canceled")
}

pub(super) fn nonterminal_blocker(blockers: &[LinearBlocker]) -> Option<&LinearBlocker> {
    blockers
        .iter()
        .find(|blocker| !blocker.state.as_deref().is_some_and(is_terminal_state))
}

pub(super) fn blocker_record(blocker: &LinearBlocker) -> BlockerRecord {
    let label = blocker
        .identifier
        .as_deref()
        .or(blocker.id.as_deref())
        .unwrap_or("unknown issue");
    let state = blocker.state.as_deref().unwrap_or("unknown state");
    BlockerRecord {
        kind: "linear_blocker".into(),
        message: format!("{label} is {state}"),
        observed_at: None,
    }
}

pub(super) fn has_new_owner_response(
    existing: Option<&IssueStateRecord>,
    issue: &LinearIssue,
) -> bool {
    if !issue.has_new_owner_answer {
        return false;
    }

    let Some(observed_at) = existing
        .and_then(|record| record.blocker.as_ref())
        .and_then(|blocker| blocker.observed_at.as_deref())
    else {
        return true;
    };

    let Some(answer_created_at) = issue.owner_answer_created_at.as_deref() else {
        return true;
    };

    answer_created_at > observed_at
}

pub(super) async fn has_existing_session(
    store: &SqliteStore,
    project_id: &str,
    issue_id: &str,
) -> anyhow::Result<bool> {
    Ok(!store
        .opencode_sessions_for_issue(project_id, issue_id)
        .await?
        .is_empty())
}
