use crate::{config::ProjectConfig, linear::LinearIssue};

use super::worktree::handoff_sidecar_path;

pub(super) fn build_issue_prompt(
    project: &ProjectConfig,
    issue: &LinearIssue,
    branch_name: &str,
) -> String {
    let description = issue
        .description
        .as_deref()
        .unwrap_or("No description provided.");
    format!(
        "Run OpenCode ACP for {identifier}: {title}\n\n\
         Project: {project_id}\n\
         Repository: {repo_path}\n\
         Isolated worktree: {worktree}\n\
         Mnemesh workspace root: {mnemesh_workspace_root}\n\
         Eval default suite: {eval_suite} (fallback metadata, not a blanket workspace gate)\n\
         Linear state: {state}\n\
         URL: {url}\n\n\
         Mnemesh evidence policy:\n\
         {mnemesh_policy}\n\n\
         Validation policy:\n\
         {validation_policy}\n\n\
         Commit policy for successful handoff:\n\
         {commit_policy}\n\n\
         After validation, commit, and push are complete, write the structured Symphony handoff JSON to:\n\
         {handoff_path}\n\n\
         The handoff file must be valid JSON matching this exact shape:\n\
         {{\n\
           \"session_id\": \"{session_id}\",\n\
           \"lifecycle_stages\": [\"starting\", \"running\", \"eval\", \"handoff\", \"completed\"],\n\
           \"subagents\": [\"agent-name:session-id\"],\n\
           \"eval_results\": [{{\"suite\": \"suite-name\", \"passed\": true, \"failure_fingerprint\": null, \"details\": \"command outcomes\", \"evidence_ref\": null}}],\n\
           \"changed_files\": [\"path:start-end\"],\n\
           \"git\": {{\"branch\": \"{branch_name}\", \"head_sha\": \"commit-sha\", \"pr_url\": null, \"worktree_path\": \"{worktree}\"}},\n\
           \"risks\": [\"remaining risk or omitted validation\"],\n\
           \"stop_reason\": {{\"type\": \"success\"}}\n\
         }}\n\
         For eval failures use \"stop_reason\": {{\"type\":\"eval_failed\",\"failure_fingerprint\":\"stable-id\"}}.\n\
         For provider or owner blockers use \"provider_blocker\" or \"owner_question\" with \"message\"/\"question\".\n\
         Do not use status, subagents_used, object-shaped eval_results, or string stop_reason values.\n\n\
         Full issue spec:\n{description}\n",
        identifier = issue.identifier,
        session_id = "the ACP session id",
        title = issue.title,
        project_id = project.id,
        repo_path = project.repo_path.display(),
        worktree = project
            .branch
            .worktree_root
            .join(&issue.identifier)
            .display(),
        mnemesh_workspace_root = project
            .mnemesh
            .as_ref()
            .map(|mnemesh| mnemesh.workspace_root.display().to_string())
            .unwrap_or_else(|| "missing".to_owned()),
        handoff_path =
            handoff_sidecar_path(project.branch.worktree_root.join(&issue.identifier)).display(),
        eval_suite = project.eval.default_suite,
        state = issue.state,
        url = issue.url.as_deref().unwrap_or("none"),
        mnemesh_policy = mnemesh_policy_text(),
        validation_policy = validation_policy_text(),
        commit_policy = commit_policy_text(),
    )
}

const fn mnemesh_policy_text() -> &'static str {
    "- Use the listed Mnemesh workspace root as the durable project evidence workspace for all Mnemesh MCP calls, observations, claims, evidence, verification, and handoff records.\n\
     - The Mnemesh workspace belongs to the canonical project root, not the isolated issue worktree.\n\
     - Do not create or register a separate Mnemesh workspace for the isolated worktree.\n\
     - If that global workspace is missing or unavailable, stop with provider_blocker and explain the workspace failure; do not continue with degraded local evidence."
}

pub(super) const fn validation_policy_text() -> &'static str {
    "- Treat the issue's Validation section as the authority for required commands.\n\
     - Scope validation to the changed surface and the issue's explicit acceptance criteria.\n\
     - For docs-only/no-code changes, run documentation/file-level validation such as git diff --check and reference checks; do not run cargo nextest --workspace, full workspace tests, or release gates unless the issue explicitly requires them.\n\
     - For Rust source changes, prefer the narrowest package/filter/profile that covers the changed behavior before escalating to workspace-level checks.\n\
     - If a broader check is intentionally skipped, record the reason in eval_results.details and risks."
}

pub(super) const fn commit_policy_text() -> &'static str {
    "- If the task changes code, docs, config, tests, or any other git-tracked state, commit and push those changes before writing a success handoff.\n\
     - Do not report success with changed_files unless git.head_sha is the pushed commit that contains those changes and is reachable from origin/git.branch.\n\
     - Use git.branch exactly as shown in the handoff schema; never write `HEAD` as git.branch.\n\
     - If commit or push fails, do not write a success handoff; stop with a provider_blocker or eval_failed handoff that includes the command failure details.\n\
     - Write or rewrite the handoff sidecar only after validation, commit, and push are complete so git.head_sha reflects the final durable revision.\n\
     - If there are truly no git changes, leave changed_files empty, keep git.branch and git.worktree_path populated, set git.head_sha to null, and explain the no-change outcome in eval_results.details.\n\
     - A successful handoff with changed_files but no matching pushed commit is invalid and must be repaired before Symphony can move the issue to Done."
}
