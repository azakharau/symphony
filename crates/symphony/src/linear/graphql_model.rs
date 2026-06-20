use serde::Deserialize;

use super::{LinearBlocker, LinearIssue, LinearMilestone, LinearUpstreamContext};

#[derive(Debug, Deserialize)]
pub(super) struct LinearIssueConnection {
    pub(super) nodes: Vec<LinearIssueNode>,
    #[serde(default, rename = "pageInfo")]
    pub(super) page_info: LinearPageInfo,
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct LinearPageInfo {
    #[serde(default, rename = "hasNextPage")]
    pub(super) has_next_page: bool,
    #[serde(default, rename = "endCursor")]
    pub(super) end_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct LinearIssueNode {
    id: String,
    identifier: String,
    title: String,
    description: Option<String>,
    state: WorkflowStateName,
    priority: Option<i64>,
    #[serde(rename = "branchName")]
    branch_name: Option<String>,
    url: Option<String>,
    #[serde(rename = "projectMilestone")]
    project_milestone: Option<LinearMilestoneNode>,
    labels: LinearLabelConnection,
    relations: LinearRelationConnection,
    #[serde(default, rename = "inverseRelations")]
    inverse_relations: LinearInverseRelationConnection,
    comments: LinearCommentConnection,
    #[serde(rename = "createdAt")]
    created_at: Option<String>,
    #[serde(rename = "updatedAt")]
    updated_at: Option<String>,
}

impl LinearIssueNode {
    pub(super) fn into_issue(self) -> LinearIssue {
        let owner_answer_created_at = latest_owner_answer_comment(&self.comments.nodes)
            .and_then(|comment| comment.created_at.clone());
        let relation_nodes = self.relations.nodes;
        let inverse_relation_nodes = self.inverse_relations.nodes;
        let mut upstream_context: Vec<_> = relation_nodes
            .iter()
            .filter(|relation| relation.relation_type == "blocked_by")
            .filter_map(|relation| relation.related_issue.accepted_context())
            .collect();
        upstream_context.extend(
            inverse_relation_nodes
                .iter()
                .filter(|relation| relation.relation_type == "blocks")
                .filter_map(|relation| relation.issue.accepted_context()),
        );

        let mut blocked_by: Vec<_> = relation_nodes
            .into_iter()
            .filter(|relation| relation.relation_type == "blocked_by")
            .map(|relation| LinearBlocker {
                id: Some(relation.related_issue.id),
                identifier: Some(relation.related_issue.identifier),
                state: Some(relation.related_issue.state.name),
            })
            .collect();
        blocked_by.extend(
            inverse_relation_nodes
                .into_iter()
                .filter(|relation| relation.relation_type == "blocks")
                .map(|relation| LinearBlocker {
                    id: Some(relation.issue.id),
                    identifier: Some(relation.issue.identifier),
                    state: Some(relation.issue.state.name),
                }),
        );

        LinearIssue {
            id: self.id,
            identifier: self.identifier,
            title: self.title,
            description: self.description,
            state: self.state.name,
            priority: self.priority,
            branch_name: self.branch_name,
            url: self.url,
            project_milestone: self
                .project_milestone
                .map(LinearMilestoneNode::into_milestone),
            labels: self
                .labels
                .nodes
                .into_iter()
                .map(|label| label.name)
                .collect(),
            blocked_by,
            upstream_context: dedupe_upstream_contexts(upstream_context),
            has_new_owner_answer: owner_answer_created_at.is_some(),
            owner_answer_created_at,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

#[derive(Debug, Deserialize)]
struct LinearMilestoneNode {
    id: String,
    name: String,
}

impl LinearMilestoneNode {
    fn into_milestone(self) -> LinearMilestone {
        LinearMilestone {
            id: self.id,
            name: self.name,
        }
    }
}

#[derive(Debug, Deserialize)]
struct WorkflowStateName {
    name: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct WorkflowStateNode {
    pub(super) id: String,
    pub(super) name: String,
}

#[derive(Debug, Deserialize)]
struct LinearLabelConnection {
    nodes: Vec<LinearLabelNode>,
}

#[derive(Debug, Deserialize)]
struct LinearLabelNode {
    name: String,
}

#[derive(Debug, Default, Deserialize)]
struct LinearRelationConnection {
    nodes: Vec<LinearRelationNode>,
}

#[derive(Debug, Deserialize)]
struct LinearRelationNode {
    #[serde(rename = "type")]
    relation_type: String,
    #[serde(rename = "relatedIssue")]
    related_issue: RelatedIssueNode,
}

#[derive(Debug, Default, Deserialize)]
struct LinearInverseRelationConnection {
    nodes: Vec<LinearInverseRelationNode>,
}

#[derive(Debug, Deserialize)]
struct LinearInverseRelationNode {
    #[serde(rename = "type")]
    relation_type: String,
    issue: RelatedIssueNode,
}

#[derive(Debug, Default, Deserialize)]
struct LinearCommentConnection {
    nodes: Vec<LinearCommentNode>,
}

#[derive(Debug, Deserialize)]
struct LinearCommentNode {
    body: Option<String>,
    #[serde(rename = "createdAt")]
    created_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RelatedIssueNode {
    id: String,
    identifier: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
    state: WorkflowStateName,
    #[serde(default, rename = "branchName")]
    branch_name: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    comments: LinearCommentConnection,
}

impl RelatedIssueNode {
    fn accepted_context(&self) -> Option<LinearUpstreamContext> {
        if !matches!(self.state.name.as_str(), "Done" | "completed" | "Completed") {
            return None;
        }

        let mut workspace_ids = Vec::new();
        let mut task_ids = Vec::new();
        let mut artifacts = Vec::new();
        if let Some(description) = self.description.as_deref() {
            extract_context_refs(description, &mut workspace_ids, &mut task_ids);
            extract_artifacts(description, &mut artifacts);
        }
        let handoff_summary = latest_handoff_comment(&self.comments.nodes).map(|comment| {
            let body = comment.body.as_deref().unwrap_or_default();
            extract_context_refs(body, &mut workspace_ids, &mut task_ids);
            extract_artifacts(body, &mut artifacts);
            compact_handoff_summary(body)
        });

        Some(LinearUpstreamContext {
            id: self.id.clone(),
            identifier: self.identifier.clone(),
            title: self.title.clone().unwrap_or_default(),
            state: self.state.name.clone(),
            url: self.url.clone(),
            branch_name: self.branch_name.clone(),
            mnemesh_workspace_ids: dedupe_preserve_order(workspace_ids),
            mnemesh_task_ids: dedupe_preserve_order(task_ids),
            accepted_artifacts: dedupe_preserve_order(artifacts),
            handoff_summary,
        })
    }
}

fn latest_handoff_comment(comments: &[LinearCommentNode]) -> Option<&LinearCommentNode> {
    comments
        .iter()
        .filter(|comment| {
            comment
                .body
                .as_deref()
                .is_some_and(|body| body.to_lowercase().contains("opencode handoff accepted"))
        })
        .max_by_key(|comment| comment.created_at.as_deref().unwrap_or_default())
}

fn extract_context_refs(text: &str, workspace_ids: &mut Vec<String>, task_ids: &mut Vec<String>) {
    for line in text.lines() {
        let lower = line.to_lowercase();
        if lower.contains("mnemesh workspace_id") || lower.contains("workspace_id") {
            workspace_ids.extend(extract_backtick_or_colon_values(line));
        }
        if lower.contains("mnemesh task_id") || lower.contains("task_id") {
            task_ids.extend(extract_backtick_or_colon_values(line));
        }
    }
}

fn extract_artifacts(text: &str, artifacts: &mut Vec<String>) {
    let mut in_changed_files = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("### ") {
            in_changed_files = trimmed.eq_ignore_ascii_case("### changed files");
            continue;
        }
        let lower = trimmed.to_lowercase();
        if in_changed_files
            || lower.contains("accepted artifact")
            || lower.contains("artifact:")
            || lower.contains("changed file")
        {
            artifacts.extend(extract_backtick_values(trimmed));
        }
    }
}

fn extract_backtick_or_colon_values(line: &str) -> Vec<String> {
    let backtick_values = extract_backtick_values(line);
    if !backtick_values.is_empty() {
        return backtick_values;
    }
    line.split_once(':')
        .map(|(_, value)| vec![value.trim().trim_matches(['`', '*', '-']).to_string()])
        .unwrap_or_default()
        .into_iter()
        .filter(|value| !value.is_empty())
        .collect()
}

fn extract_backtick_values(line: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut rest = line;
    while let Some(start) = rest.find('`') {
        let after_start = &rest[start + 1..];
        let Some(end) = after_start.find('`') else {
            break;
        };
        let value = after_start[..end].trim();
        if !value.is_empty() {
            values.push(value.to_string());
        }
        rest = &after_start[end + 1..];
    }
    values
}

fn compact_handoff_summary(body: &str) -> String {
    body.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(18)
        .collect::<Vec<_>>()
        .join("\n")
}

fn dedupe_preserve_order(values: Vec<String>) -> Vec<String> {
    values.into_iter().fold(Vec::new(), |mut acc, value| {
        if !acc.contains(&value) {
            acc.push(value);
        }
        acc
    })
}

fn dedupe_upstream_contexts(values: Vec<LinearUpstreamContext>) -> Vec<LinearUpstreamContext> {
    values.into_iter().fold(Vec::new(), |mut acc, value| {
        if !acc.iter().any(|existing| existing.id == value.id) {
            acc.push(value);
        }
        acc
    })
}

fn latest_owner_answer_comment(comments: &[LinearCommentNode]) -> Option<&LinearCommentNode> {
    comments
        .iter()
        .filter(|comment| owner_answer_comment(comment))
        .max_by_key(|comment| comment.created_at.as_deref().unwrap_or_default())
}

fn owner_answer_comment(comment: &LinearCommentNode) -> bool {
    let Some(body) = comment.body.as_deref() else {
        return false;
    };
    let normalized = body.trim().to_lowercase();
    if normalized.is_empty()
        || machine_generated_owner_input_comment(&normalized)
        || long_question_comment(&normalized)
    {
        return false;
    }

    true
}

fn machine_generated_owner_input_comment(body: &str) -> bool {
    if body.starts_with("kind: ") || body.starts_with("kind:\n") {
        return true;
    }

    [
        "<!-- symphony:",
        "## opencode handoff",
        "## opencode session attached",
        "## symphony stop rule",
        "## benchmark",
        "## validation",
        "## changed files",
        "```text\nstatus:",
        "symphony stop rule",
        "opencode handoff",
        "opencode session attached",
        "changed files",
        "validation results",
    ]
    .iter()
    .any(|marker| body.contains(marker))
}

fn long_question_comment(body: &str) -> bool {
    body.len() > 80 && body.contains('?')
}
