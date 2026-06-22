use std::fmt::Write as _;

use crate::{
    config::ProjectConfig,
    linear::LinearIssue,
    runner::{RunnerEvalResult, RunnerHandoff},
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
    handoff: &RunnerHandoff,
    git: &crate::runner::GitClosureEvidence,
    integrated_base: Option<&str>,
) -> String {
    let mut body = String::new();
    let _ = writeln!(body, "## runner Handoff Accepted\n");
    if integrated_base.is_some() {
        let _ = writeln!(
            body,
            "runner completed the task, pushed the issue branch, and Symphony integrated it into the canonical branch.\n"
        );
    } else {
        let _ = writeln!(
            body,
            "runner completed the task and Symphony accepted a no-change git closure.\n"
        );
    }

    let _ = writeln!(body, "### Result");
    let _ = writeln!(body, "- Status: Done");
    let _ = writeln!(body, "- Session: `{}`", handoff.session_id);
    let _ = writeln!(
        body,
        "- Subagents: {}",
        readable_list(&handoff.subagents, "none reported", 8)
    );

    let _ = writeln!(body, "\n### Git");
    let _ = writeln!(body, "- Branch: `{}`", git.branch);
    let _ = writeln!(
        body,
        "- Commit: `{}`",
        git.head_sha.as_deref().unwrap_or("not reported")
    );
    let _ = writeln!(
        body,
        "- Integrated base: `{}`",
        integrated_base.unwrap_or("none")
    );
    let _ = writeln!(
        body,
        "- PR: {}",
        git.pr_url
            .as_deref()
            .map_or_else(|| "none".to_string(), |url| format!("[link]({url})"))
    );

    let _ = writeln!(body, "\n### Validation");
    if handoff.eval_results.is_empty() {
        let _ = writeln!(body, "- No validation suites were reported.");
    } else {
        for eval in &handoff.eval_results {
            let _ = writeln!(body, "- {}", readable_eval(eval));
        }
    }

    let _ = writeln!(body, "\n### Changed Files");
    if handoff.changed_files.is_empty() {
        let _ = writeln!(body, "- No git-tracked files changed.");
    } else {
        for path in handoff.changed_files.iter().take(12) {
            let _ = writeln!(body, "- `{path}`");
        }
        let remaining = handoff.changed_files.len().saturating_sub(12);
        if remaining > 0 {
            let _ = writeln!(body, "- ...and {remaining} more");
        }
    }

    let _ = writeln!(body, "\n### Risks");
    if handoff.risks.is_empty() {
        let _ = writeln!(body, "- None reported.");
    } else {
        for risk in handoff.risks.iter().take(8) {
            let _ = writeln!(body, "- {risk}");
        }
        let remaining = handoff.risks.len().saturating_sub(8);
        if remaining > 0 {
            let _ = writeln!(body, "- ...and {remaining} more");
        }
    }

    body
}

fn readable_eval(eval: &RunnerEvalResult) -> String {
    let status = if eval.passed { "passed" } else { "failed" };
    let mut line = format!("`{}` {status}", eval.suite);
    if let Some(details) = eval
        .details
        .as_deref()
        .filter(|details| !details.is_empty())
    {
        line.push_str(" - ");
        line.push_str(details);
    }
    if let Some(evidence_ref) = eval
        .evidence_ref
        .as_deref()
        .filter(|evidence_ref| !evidence_ref.is_empty())
    {
        let _ = write!(line, " (evidence: `{evidence_ref}`)");
    }
    line
}

fn readable_list(values: &[String], empty: &str, limit: usize) -> String {
    if values.is_empty() {
        return empty.to_string();
    }

    let mut rendered = values
        .iter()
        .take(limit)
        .map(|value| format!("`{value}`"))
        .collect::<Vec<_>>()
        .join(", ");
    let remaining = values.len().saturating_sub(limit);
    if remaining > 0 {
        let _ = write!(rendered, ", and {remaining} more");
    }
    rendered
}
