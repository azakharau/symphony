use crate::{
    config::ProjectConfig,
    linear::LinearIssue,
    opencode::OpenCodeHandoff,
    state::{BlockerRecord, CleanupStatus, GitRefRecord, IssueStateRecord, LifecycleStage},
};

pub(super) fn issue_record(
    project: &ProjectConfig,
    issue: &LinearIssue,
    lifecycle_stage: LifecycleStage,
    blocker: Option<BlockerRecord>,
    cleanup_status: CleanupStatus,
) -> IssueStateRecord {
    IssueStateRecord {
        project_id: project.id.clone(),
        issue_id: issue.id.clone(),
        identifier: issue.identifier.clone(),
        title: issue.title.clone(),
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

pub(super) fn git_closure_evidence_body(
    handoff: &OpenCodeHandoff,
    git: &crate::opencode::GitClosureEvidence,
    integrated_base: Option<&str>,
) -> String {
    format!(
        "session_id: {}\nbranch: {}\nhead_sha: {}\nintegrated_base: {}\npr_url: {}\nchanged_files: {}\nevals: {}\nrisks: {}",
        handoff.session_id,
        git.branch,
        git.head_sha.as_deref().unwrap_or(""),
        integrated_base.unwrap_or("none"),
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
