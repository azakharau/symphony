use std::cmp::Ordering;

use crate::{
    linear::{LinearBlocker, LinearIssue},
    state::{BlockerRecord, FailureRecord, IssueStateRecord},
};

pub(super) fn matching_failure_count(failure: Option<&FailureRecord>, fingerprint: &str) -> u32 {
    failure
        .filter(|failure| failure.fingerprint.as_deref().unwrap_or(&failure.message) == fingerprint)
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
    failure.kind == "eval_failure"
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

fn is_accepted_blocker_state(state: &str) -> bool {
    matches!(state, "Done" | "completed" | "Completed")
}

pub(super) fn unaccepted_blocker(blockers: &[LinearBlocker]) -> Option<&LinearBlocker> {
    blockers.iter().find(|blocker| {
        !blocker
            .state
            .as_deref()
            .is_some_and(is_accepted_blocker_state)
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    fn failure(
        kind: &str,
        fingerprint: Option<&str>,
        message: &str,
        occurrence_count: u32,
    ) -> FailureRecord {
        FailureRecord {
            kind: kind.into(),
            message: message.into(),
            fingerprint: fingerprint.map(str::to_owned),
            occurrence_count,
        }
    }

    #[test]
    fn matching_failure_count_preserves_eval_failure_matching() {
        let failure = failure("eval_failure", Some("fmt-check-7f"), "fmt failed", 2);

        assert_eq!(matching_failure_count(Some(&failure), "fmt-check-7f"), 2);
        assert_eq!(matching_failure_count(Some(&failure), "clippy-loop"), 0);
    }

    #[test]
    fn matching_failure_count_matches_runtime_repair_fingerprints() {
        let failure = failure(
            "malformed_handoff",
            Some("session_id_mismatch"),
            "session id mismatch",
            1,
        );

        assert_eq!(
            matching_failure_count(Some(&failure), "session_id_mismatch"),
            1
        );
    }
}
