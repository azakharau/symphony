use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::config::ProjectConfig;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LinearProjectConfig {
    pub team_key: String,
    pub project_id: Option<String>,
    pub project_milestone_id: Option<String>,
}

pub trait LinearClient {
    fn fetch_candidate_issues(
        &self,
        project: &ProjectConfig,
    ) -> Result<Vec<LinearIssue>, LinearClientError>;

    fn transition_issue(
        &self,
        issue_id: &str,
        transition: LinearTransition,
    ) -> Result<(), LinearClientError>;

    fn record_issue_evidence(
        &self,
        _issue_id: &str,
        _evidence: LinearIssueEvidence,
    ) -> Result<(), LinearClientError> {
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct EmptyLinearClient;

impl LinearClient for EmptyLinearClient {
    fn fetch_candidate_issues(
        &self,
        _project: &ProjectConfig,
    ) -> Result<Vec<LinearIssue>, LinearClientError> {
        Ok(Vec::new())
    }

    fn transition_issue(
        &self,
        _issue_id: &str,
        _transition: LinearTransition,
    ) -> Result<(), LinearClientError> {
        Ok(())
    }
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
    pub blocked_by: Vec<LinearBlocker>,
    pub has_new_owner_answer: bool,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

impl LinearIssue {
    pub fn blocked_by(mut self, blockers: Vec<LinearBlocker>) -> Self {
        self.blocked_by = blockers;
        self
    }

    pub fn with_new_owner_answer(mut self, has_new_owner_answer: bool) -> Self {
        self.has_new_owner_answer = has_new_owner_answer;
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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LinearIssueEvidence {
    pub kind: String,
    pub body: String,
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
    pub fn state_name(self) -> &'static str {
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
}
