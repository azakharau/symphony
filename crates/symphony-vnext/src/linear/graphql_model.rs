use serde::Deserialize;

use super::{LinearBlocker, LinearIssue, LinearMilestone};

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
            blocked_by: self
                .relations
                .nodes
                .into_iter()
                .filter(|relation| relation.relation_type == "blocked_by")
                .map(|relation| LinearBlocker {
                    id: Some(relation.related_issue.id),
                    identifier: Some(relation.related_issue.identifier),
                    state: Some(relation.related_issue.state.name),
                })
                .collect(),
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

#[derive(Debug, Deserialize)]
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

#[derive(Debug, Deserialize)]
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
    state: WorkflowStateName,
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
        "codex implementation handoff",
        "codex repair handoff",
    ]
    .iter()
    .any(|marker| body.contains(marker))
}

fn long_question_comment(body: &str) -> bool {
    body.len() > 80 && body.contains('?')
}
