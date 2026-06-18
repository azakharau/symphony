use std::path::Path;

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
         Mnemesh evidence workspace contract:\n\
         {mnemesh_workspace_contract}\n\n\
         MCP tool-schema loop guard:\n\
         {mcp_tool_loop_guard}\n\n\
         Delegated review/evaluator subagent contract:\n\
         {delegated_subagent_contract}\n\n\
         Validation policy:\n\
         {validation_policy}\n\n\
         Triage and owner-input boundary:\n\
         {triage_policy}\n\n\
         Commit policy for successful handoff:\n\
         {commit_policy}\n\n\
         After validation, commit, and push are complete, write the structured Symphony handoff JSON to:\n\
         {handoff_path}\n\n\
         The handoff file must be valid JSON matching this exact shape, not the Markdown ACP result packet:\n\
         Top-level JSON keys must be exactly session_id, lifecycle_stages, subagents, eval_results, changed_files, git, risks, and stop_reason; unknown fields are invalid.\n\
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
         Do not copy Markdown ACP handoff fields into this JSON. Do not use status, subagents_used, object-shaped eval_results, or string stop_reason values.\n\n\
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
        mnemesh_workspace_root = mnemesh_workspace_root_display(
            project
                .mnemesh
                .as_ref()
                .map(|mnemesh| { mnemesh.workspace_root.as_path() })
        ),
        handoff_path =
            handoff_sidecar_path(project.branch.worktree_root.join(&issue.identifier)).display(),
        eval_suite = project.eval.default_suite,
        state = issue.state,
        url = issue.url.as_deref().unwrap_or("none"),
        mnemesh_workspace_contract = mnemesh_workspace_contract_text(
            project
                .mnemesh
                .as_ref()
                .map(|mnemesh| mnemesh.workspace_root.as_path()),
            &project.branch.worktree_root.join(&issue.identifier),
        ),
        validation_policy = validation_policy_text(),
        mcp_tool_loop_guard = mcp_tool_loop_guard_text(),
        delegated_subagent_contract = delegated_subagent_contract_text(),
        triage_policy = triage_policy_text(),
        commit_policy = commit_policy_text(),
    )
}

pub(super) fn mnemesh_workspace_contract_text(
    mnemesh_workspace_root: Option<&Path>,
    isolated_worktree: &Path,
) -> String {
    let workspace_root = mnemesh_workspace_root_display(mnemesh_workspace_root);
    let isolated_worktree = isolated_worktree.display();
    format!(
        "- Use `{workspace_root}` as the durable project evidence workspace for all Mnemesh MCP calls, observations, claims, evidence, verification, and handoff records.\n\
         - The Mnemesh workspace belongs to the canonical project root, not the isolated issue worktree.\n\
         - Do not create or register a separate Mnemesh workspace for the isolated worktree `{isolated_worktree}`.\n\
         - Required `mcp__mnemesh__create_task.worktree` payload: `repo_root` must be `{workspace_root}`, `worktree_path` must be `{workspace_root}`, and `head` must describe the current git HEAD of `{workspace_root}` using only `reference` and `commit` fields.\n\
         - Never set `mcp__mnemesh__create_task.worktree.worktree_path` to `{isolated_worktree}`. Mention the isolated implementation worktree only in free-text objective or workstream label when useful.\n\
         - If `{workspace_root}` is missing or unavailable, stop with provider_blocker and explain the workspace failure; do not continue with degraded local evidence."
    )
}

fn mnemesh_workspace_root_display(mnemesh_workspace_root: Option<&Path>) -> String {
    mnemesh_workspace_root
        .map(|root| root.display().to_string())
        .unwrap_or_else(|| "missing".to_owned())
}

pub(super) const fn validation_policy_text() -> &'static str {
    "- Treat the issue's Validation section as the authority for required commands.\n\
     - Scope validation to the changed surface and the issue's explicit acceptance criteria.\n\
     - For docs-only/no-code changes, run documentation/file-level validation such as git diff --check and reference checks; do not run cargo nextest --workspace, full workspace tests, or release gates unless the issue explicitly requires them.\n\
     - For Rust source changes, prefer the narrowest package/filter/profile that covers the changed behavior before escalating to workspace-level checks.\n\
     - If a broader check is intentionally skipped, record the reason in eval_results.details and risks."
}

pub(super) const fn mcp_tool_loop_guard_text() -> &'static str {
    "- Treat MCP tool schema, validation, or version conflicts as bounded infrastructure feedback, not as an invitation to keep guessing payload shapes.\n\
     - After two failed calls to the same MCP method for schema/validation reasons, stop retrying that method in this session.\n\
     - If the failed MCP call is required for the issue's acceptance or durable evidence policy, write a provider_blocker handoff sidecar with the exact method name and error text.\n\
     - If the failed MCP call is only optional planning/artifact decoration and the issue has enough git, validation, and handoff evidence, skip that optional MCP call, record the skipped method and errors in risks, and continue to closure.\n\
     - Never spend additional turns reverse-engineering MCP payloads after the bounded retry limit; use the handoff sidecar to make the blocker explicit."
}

pub(super) const fn delegated_subagent_contract_text() -> &'static str {
    "- Delegated reviewer/evaluator subagents are read-only unless the issue spec explicitly says otherwise.\n\
     - Do not ask delegated reviewer/evaluator subagents to call Mnemesh mutation tools such as record_artifact, attach_evidence, record_verification, or create_task.\n\
     - Delegated reviewer/evaluator subagents should inspect files/tests/evidence and return a concise structured verdict to the parent OpenCode session.\n\
     - The parent OpenCode session owns any required Mnemesh writeback after reading the subagent verdict.\n\
     - If a delegated subagent already failed an MCP mutation because of schema/version errors, do not retry that mutation from another delegated subagent; continue with a text verdict and parent-owned writeback."
}

pub(super) const fn triage_policy_text() -> &'static str {
    "- Use owner_question only for real owner, product, or permission questions that need a human decision before work can continue.\n\
     - Use provider_blocker for provider, infrastructure, workspace, credential, or tool availability blockers; these are not owner input.\n\
     - Use eval_failed only for validation or evaluator failures; keep them in repair evidence rather than converting them to owner questions.\n\
     - Treat missing or malformed handoff sidecars, stale process/session evidence, git closure mismatches, cleanup failures, prompt regressions, and evaluator contract failures as runtime/tooling defects that require bounded repair, a typed runtime-defect blocker, or a Symphony self-reference bug.\n\
     - Classifier, model, and evaluator output is advisory only; only deterministic runtime policy and the Linear writer may create or mutate Linear issues.\n\
     - Do not classify runtime/tooling defects as owner input unless a real owner, product, or permission decision is required.\n\
     - Auto-created self-reference bugs use P0 Todo only for unsafe runtime advance or closure blockers; P1 degraded project paths and P2 non-blocking hardening default to Backlog unless hard policy explicitly escalates.\n\
     - If an active SYM-* issue exposes a Symphony defect, do not wait on or requeue the same active issue; park it with typed runtime-defect/provider evidence and create or link a separate self-reference bug.\n\
     - Do not requeue runtime/tooling defects to runnable Todo as product work."
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
