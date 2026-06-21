use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LinearProjectConfig {
    pub team_key: String,
    pub project_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LinearIssue {
    pub id: String,
    pub identifier: String,
    pub title: String,
    pub description: Option<String>,
    pub state: String,
    pub priority: Option<i64>,
    pub branch_name: Option<String>,
    pub url: Option<String>,
    pub labels: Vec<String>,
    pub project_milestone: Option<LinearMilestone>,
    pub blocked_by: Vec<LinearBlocker>,
    #[serde(default)]
    pub upstream_context: Vec<LinearUpstreamContext>,
    pub has_new_owner_answer: bool,
    pub owner_answer_created_at: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LinearMilestone {
    pub id: String,
    pub name: String,
}

impl LinearIssue {
    pub fn blocked_by(mut self, blockers: Vec<LinearBlocker>) -> Self {
        self.blocked_by = blockers;
        self
    }

    pub const fn with_new_owner_answer(mut self, has_new_owner_answer: bool) -> Self {
        self.has_new_owner_answer = has_new_owner_answer;
        self
    }

    pub fn with_new_owner_answer_at(mut self, created_at: impl Into<String>) -> Self {
        self.has_new_owner_answer = true;
        self.owner_answer_created_at = Some(created_at.into());
        self
    }

    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LinearBlocker {
    pub id: Option<String>,
    pub identifier: Option<String>,
    pub state: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LinearUpstreamContext {
    pub id: String,
    pub identifier: String,
    pub title: String,
    pub state: String,
    pub url: Option<String>,
    pub branch_name: Option<String>,
    pub recall_workspace_ids: Vec<String>,
    pub recall_task_ids: Vec<String>,
    pub accepted_artifacts: Vec<String>,
    pub handoff_summary: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LinearIssueEvidence {
    pub kind: String,
    pub body: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedLinearIssueCreate {
    pub source_issue_id: String,
    pub fingerprint: String,
    pub title: String,
    pub description: String,
    pub priority: i64,
    pub state: ManagedLinearIssueState,
    pub project_milestone_id: Option<String>,
    pub label_ids: Vec<String>,
}

impl ManagedLinearIssueCreate {
    pub fn description_with_fingerprint(&self) -> String {
        format!(
            "{}\n\n<!-- symphony:managed-self-bug fingerprint={} -->",
            self.description.trim_end(),
            self.fingerprint
        )
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedLinearIssueState {
    Backlog,
    Todo,
}

impl ManagedLinearIssueState {
    pub const fn state_name(self) -> &'static str {
        match self {
            Self::Backlog => "Backlog",
            Self::Todo => "Todo",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedLinearRelation {
    Blocks,
    Related,
}

impl ManagedLinearRelation {
    pub const fn relation_type(self) -> &'static str {
        match self {
            Self::Blocks => "blocks",
            Self::Related => "related",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LinearTransition {
    Todo,
    InProgress,
    NeedOwnerInput,
    Done,
}

impl LinearTransition {
    pub const fn state_name(self) -> &'static str {
        match self {
            Self::Todo => "Todo",
            Self::InProgress => "In Progress",
            Self::NeedOwnerInput => "Need Owner Input",
            Self::Done => "Done",
        }
    }
}

#[derive(Debug, Error)]
pub enum LinearClientError {
    #[error("linear client error: {0}")]
    Message(String),
    #[error("linear sdk error: {0}")]
    Sdk(#[from] lineark_sdk::LinearError),
}
