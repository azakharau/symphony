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
    let worktree = project.branch.worktree_root.join(&issue.identifier);
    format!(
        "Issue {identifier}: {title}\n\n\
         Project: {project_id}\n\
         Repository: {repo_path}\n\
         Isolated worktree: {worktree}\n\
         Eval default suite: {eval_suite} (fallback metadata, not a blanket workspace gate)\n\
         Linear state: {state}\n\
         URL: {url}\n\n\
         Upstream accepted context:\n\
         {upstream_context}\n\n\
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
         The handoff file must be valid JSON with durable execution evidence, not a Markdown result packet:\n\
         Use the sidecar JSON contract below for {handoff_path}; keep chat summaries separate from this file.\n\
         Symphony accepts runner orchestrator field names such as status, schema_version, subagents_used, object eval_results, and git.pushed, then normalizes them before strict validation.\n\
         {{\n\
           \"session_id\": \"{session_id}\",\n\
           \"lifecycle_stages\": [\"starting\", \"running\", \"eval\", \"review\", \"handoff\", \"completed\"],\n\
           \"subagents_used\": [\"agent-name:session-id\"],\n\
           \"eval_results\": {{\"outcome\": \"accept\", \"details\": \"command outcomes\", \"commands\": [{{\"command\": \"git diff --check\", \"status\": \"pass\"}}]}},\n\
           \"changed_files\": [\"path:start-end\"],\n\
           \"git\": {{\"branch\": \"{branch_name}\", \"head_sha\": \"commit-sha\", \"worktree_path\": \"{worktree}\", \"pushed\": true}},\n\
           \"risks\": [\"remaining risk or omitted validation\"],\n\
           \"stop_reason\": \"accepted\"\n\
         }}\n\
         For eval failures use \"stop_reason\": {{\"type\":\"eval_failed\",\"failure_fingerprint\":\"stable-id\"}}.\n\
         For provider or owner blockers use {{\"type\":\"provider_blocker\",\"message\":\"...\"}} or {{\"type\":\"owner_question\",\"question\":\"...\"}}.\n\
         Do not write only prose fields such as result, summary, tests_run, or next_action without the structured git/eval/stop_reason fields above.\n\n\
         Full issue spec:\n{description}\n",
        identifier = issue.identifier,
        session_id = "the ACP session id",
        title = issue.title,
        project_id = project.id,
        repo_path = project.repo_path.display(),
        worktree = worktree.display(),
        handoff_path = handoff_sidecar_path(&worktree).display(),
        eval_suite = project.eval.default_suite,
        state = issue.state,
        url = issue.url.as_deref().unwrap_or("none"),
        upstream_context = upstream_context_text(issue),
        validation_policy = validation_policy_text(),
        mcp_tool_loop_guard = mcp_tool_loop_guard_text(),
        delegated_subagent_contract = delegated_subagent_contract_text(),
        triage_policy = triage_policy_text(),
        commit_policy = commit_policy_text(),
    )
}

fn upstream_context_text(issue: &LinearIssue) -> String {
    if issue.upstream_context.is_empty() {
        return "- No accepted upstream Linear blocker context is available for this issue."
            .to_owned();
    }

    let mut lines = Vec::new();
    for context in issue.upstream_context.iter().take(8) {
        lines.push(format!(
            "- {} (`{}`): {}",
            context.identifier,
            context.state,
            empty_as("untitled upstream issue", &context.title)
        ));
        if let Some(url) = context.url.as_deref().filter(|url| !url.is_empty()) {
            lines.push(format!("  URL: {url}"));
        }
        if let Some(branch) = context
            .branch_name
            .as_deref()
            .filter(|branch| !branch.is_empty())
        {
            lines.push(format!("  Branch: {branch}"));
        }
        push_limited_values(
            &mut lines,
            "  Accepted artifacts",
            &context.accepted_artifacts,
            12,
        );
        if let Some(summary) = context
            .handoff_summary
            .as_deref()
            .filter(|summary| !summary.trim().is_empty())
        {
            lines.push("  Latest handoff excerpt:".to_owned());
            lines.extend(
                summary
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .take(12)
                    .map(|line| format!("    {line}")),
            );
        }
        lines.push(
            "  Required use: treat this as accepted upstream input; inspect accepted artifacts and git context before rediscovering or replanning this surface."
                .to_owned(),
        );
    }

    if issue.upstream_context.len() > 8 {
        lines.push(format!(
            "- {} additional accepted upstream issues omitted from the launch prompt; inspect Linear relations if needed.",
            issue.upstream_context.len() - 8
        ));
    }

    lines.join("\n")
}

fn empty_as<'a>(fallback: &'a str, value: &'a str) -> &'a str {
    if value.trim().is_empty() {
        fallback
    } else {
        value
    }
}

fn push_limited_values(lines: &mut Vec<String>, label: &str, values: &[String], limit: usize) {
    if values.is_empty() {
        return;
    }

    let visible = values
        .iter()
        .take(limit)
        .map(|value| format!("`{value}`"))
        .collect::<Vec<_>>()
        .join(", ");
    if values.len() > limit {
        lines.push(format!(
            "{label}: {visible} (+{} more)",
            values.len() - limit
        ));
    } else {
        lines.push(format!("{label}: {visible}"));
    }
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
     - Delegated reviewer/evaluator subagents should inspect files/tests/evidence and return a concise structured verdict to the parent ACP session.\n\
     - The parent ACP session owns final validation, git closure, and structured handoff sidecar writeback.\n\
     - If a delegated subagent reports a tool schema/version error, do not retry the same failing mutation from another delegated subagent; continue with a text verdict and parent-owned closure."
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
