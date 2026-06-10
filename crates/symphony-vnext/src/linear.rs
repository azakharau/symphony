use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

use crate::config::ProjectConfig;

const CANDIDATE_STATES: &[&str] = &[
    "Backlog",
    "Todo",
    "In Progress",
    "Need Owner Input",
    "Preparing",
    "In Review",
    "RCA Required",
    "Done",
    "Canceled",
    "Cancelled",
    "Closed",
    "Duplicate",
];

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LinearProjectConfig {
    pub team_key: String,
    pub project_id: Option<String>,
    pub project_milestone_id: Option<String>,
}

#[async_trait::async_trait]
pub trait LinearClient: Sync {
    async fn fetch_candidate_issues(
        &self,
        project: &ProjectConfig,
    ) -> Result<Vec<LinearIssue>, LinearClientError>;

    async fn transition_issue(
        &self,
        issue_id: &str,
        transition: LinearTransition,
    ) -> Result<(), LinearClientError>;

    async fn record_issue_evidence(
        &self,
        _issue_id: &str,
        _evidence: LinearIssueEvidence,
    ) -> Result<(), LinearClientError> {
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct EmptyLinearClient;

#[async_trait::async_trait]
impl LinearClient for EmptyLinearClient {
    async fn fetch_candidate_issues(
        &self,
        _project: &ProjectConfig,
    ) -> Result<Vec<LinearIssue>, LinearClientError> {
        Ok(Vec::new())
    }

    async fn transition_issue(
        &self,
        _issue_id: &str,
        _transition: LinearTransition,
    ) -> Result<(), LinearClientError> {
        Ok(())
    }
}

#[async_trait::async_trait]
pub trait LinearGraphqlTransport {
    async fn post_graphql(
        &self,
        endpoint: &str,
        api_key: &str,
        request: Value,
    ) -> Result<Value, LinearClientError>;
}

#[derive(Clone, Debug)]
pub struct LinearGraphqlClient<T> {
    endpoint: String,
    api_key: String,
    transport: T,
}

impl<T> LinearGraphqlClient<T> {
    pub fn new(endpoint: impl Into<String>, api_key: impl Into<String>, transport: T) -> Self {
        Self {
            endpoint: endpoint.into(),
            api_key: api_key.into(),
            transport,
        }
    }
}

#[derive(Clone, Debug)]
pub struct LinearSdkClient {
    client: lineark_sdk::Client,
}

impl LinearSdkClient {
    pub fn from_env() -> Result<Self, LinearClientError> {
        let api_key = std::env::var("LINEAR_API_KEY")
            .map_err(|_| LinearClientError::Message("LINEAR_API_KEY is required".into()))?;
        Ok(Self {
            client: lineark_sdk::Client::from_token(api_key)?,
        })
    }
}

#[async_trait::async_trait]
impl LinearClient for LinearSdkClient {
    async fn fetch_candidate_issues(
        &self,
        project: &ProjectConfig,
    ) -> Result<Vec<LinearIssue>, LinearClientError> {
        fetch_candidate_issues_with(
            |request| async move {
                self.client
                    .execute::<LinearIssueConnection>(
                        request.query,
                        request.variables,
                        request.data_path,
                    )
                    .await
                    .map_err(LinearClientError::from)
            },
            project,
        )
        .await
    }

    async fn transition_issue(
        &self,
        issue_id: &str,
        transition: LinearTransition,
    ) -> Result<(), LinearClientError> {
        let state_id = state_id_for_issue_with(
            |request| async move {
                self.client
                    .execute::<Value>(request.query, request.variables, request.data_path)
                    .await
                    .map_err(LinearClientError::from)
            },
            issue_id,
            transition.state_name(),
        )
        .await?;
        let response = self
            .client
            .execute::<Value>(
                UPDATE_ISSUE_STATE_MUTATION,
                json!({
                    "issueId": issue_id,
                    "stateId": state_id,
                }),
                "issueUpdate",
            )
            .await?;
        ensure_success(&response, "/success", "issueUpdate")
    }

    async fn record_issue_evidence(
        &self,
        issue_id: &str,
        evidence: LinearIssueEvidence,
    ) -> Result<(), LinearClientError> {
        let body = format!("kind: {}\n\n{}", evidence.kind, evidence.body);
        let response = self
            .client
            .execute::<Value>(
                CREATE_COMMENT_MUTATION,
                json!({
                    "issueId": issue_id,
                    "body": body,
                }),
                "commentCreate",
            )
            .await?;
        ensure_success(&response, "/success", "commentCreate")
    }
}

#[async_trait::async_trait]
impl<T> LinearClient for LinearGraphqlClient<T>
where
    T: LinearGraphqlTransport + Send + Sync,
{
    async fn fetch_candidate_issues(
        &self,
        project: &ProjectConfig,
    ) -> Result<Vec<LinearIssue>, LinearClientError> {
        fetch_candidate_issues_with(
            |request| async move {
                let response = self
                    .post(json!({
                        "query": request.query,
                        "variables": request.variables,
                    }))
                    .await?;
                let value = response.pointer("/data/issues").cloned().ok_or_else(|| {
                    LinearClientError::Message("missing issues connection".into())
                })?;
                serde_json::from_value(value)
                    .map_err(|error| LinearClientError::Message(format!("decode issues: {error}")))
            },
            project,
        )
        .await
    }

    async fn transition_issue(
        &self,
        issue_id: &str,
        transition: LinearTransition,
    ) -> Result<(), LinearClientError> {
        let state_id = self
            .state_id_for_issue(issue_id, transition.state_name())
            .await?;
        let response = self
            .post(json!({
                "query": UPDATE_ISSUE_STATE_MUTATION,
                "variables": {
                    "issueId": issue_id,
                    "stateId": state_id,
                },
            }))
            .await?;
        ensure_success(&response, "/data/issueUpdate/success", "issueUpdate")
    }

    async fn record_issue_evidence(
        &self,
        issue_id: &str,
        evidence: LinearIssueEvidence,
    ) -> Result<(), LinearClientError> {
        let body = format!("kind: {}\n\n{}", evidence.kind, evidence.body);
        let response = self
            .post(json!({
                "query": CREATE_COMMENT_MUTATION,
                "variables": {
                    "issueId": issue_id,
                    "body": body,
                },
            }))
            .await?;
        ensure_success(&response, "/data/commentCreate/success", "commentCreate")
    }
}

impl<T> LinearGraphqlClient<T>
where
    T: LinearGraphqlTransport + Send + Sync,
{
    async fn post(&self, request: Value) -> Result<Value, LinearClientError> {
        let response = self
            .transport
            .post_graphql(&self.endpoint, &self.api_key, request)
            .await?;
        if let Some(errors) = response.get("errors") {
            return Err(LinearClientError::Message(format!(
                "linear graphql errors: {errors}"
            )));
        }
        Ok(response)
    }

    async fn state_id_for_issue(
        &self,
        issue_id: &str,
        state_name: &str,
    ) -> Result<String, LinearClientError> {
        state_id_for_issue_with(
            |request| async move {
                self.post(json!({
                    "query": request.query,
                    "variables": request.variables,
                }))
                .await
            },
            issue_id,
            state_name,
        )
        .await
    }
}

struct GraphqlRequest {
    query: &'static str,
    variables: Value,
    data_path: &'static str,
}

async fn fetch_candidate_issues_with<F, Fut>(
    mut execute: F,
    project: &ProjectConfig,
) -> Result<Vec<LinearIssue>, LinearClientError>
where
    F: FnMut(GraphqlRequest) -> Fut,
    Fut: std::future::Future<Output = Result<LinearIssueConnection, LinearClientError>>,
{
    let project_id = project
        .linear
        .project_id
        .as_deref()
        .ok_or_else(|| LinearClientError::Message("linear.project_id is required".into()))?;
    let project_milestone_id = project
        .linear
        .project_milestone_id
        .as_deref()
        .ok_or_else(|| {
            LinearClientError::Message("linear.project_milestone_id is required".into())
        })?;
    let mut issues = Vec::new();
    let mut after: Option<String> = None;

    loop {
        let connection = execute(GraphqlRequest {
            query: CANDIDATE_ISSUES_QUERY,
            variables: json!({
                "teamKey": project.linear.team_key,
                "projectId": project_id,
                "projectMilestoneId": project_milestone_id,
                "states": CANDIDATE_STATES,
                "after": after,
            }),
            data_path: "issues",
        })
        .await?;

        issues.extend(
            connection
                .nodes
                .into_iter()
                .map(LinearIssueNode::into_issue),
        );

        if !connection.page_info.has_next_page {
            return Ok(issues);
        }
        after = connection.page_info.end_cursor;
        if after.as_deref().is_none_or(str::is_empty) {
            return Err(LinearClientError::Message(
                "linear issues pageInfo requested next page without endCursor".into(),
            ));
        }
    }
}

async fn state_id_for_issue_with<F, Fut>(
    execute: F,
    issue_id: &str,
    state_name: &str,
) -> Result<String, LinearClientError>
where
    F: FnOnce(GraphqlRequest) -> Fut,
    Fut: std::future::Future<Output = Result<Value, LinearClientError>>,
{
    let response = execute(GraphqlRequest {
        query: ISSUE_STATES_QUERY,
        variables: json!({ "issueId": issue_id }),
        data_path: "issue",
    })
    .await?;
    let nodes = response
        .pointer("/team/states/nodes")
        .or_else(|| response.pointer("/data/issue/team/states/nodes"))
        .cloned()
        .ok_or_else(|| LinearClientError::Message("missing workflow states".into()))?;
    let states = serde_json::from_value::<Vec<WorkflowStateNode>>(nodes)
        .map_err(|error| LinearClientError::Message(format!("decode states: {error}")))?;

    states
        .into_iter()
        .find(|state| state.name == state_name)
        .map(|state| state.id)
        .ok_or_else(|| LinearClientError::Message(format!("missing state `{state_name}`")))
}

fn ensure_success(
    response: &Value,
    pointer: &str,
    mutation: &str,
) -> Result<(), LinearClientError> {
    match response.pointer(pointer).and_then(Value::as_bool) {
        Some(true) => Ok(()),
        _ => Err(LinearClientError::Message(format!(
            "{mutation} did not return success"
        ))),
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
    pub owner_answer_created_at: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LinearIssueConnection {
    nodes: Vec<LinearIssueNode>,
    #[serde(default, rename = "pageInfo")]
    page_info: LinearPageInfo,
}

#[derive(Debug, Default, Deserialize)]
struct LinearPageInfo {
    #[serde(default, rename = "hasNextPage")]
    has_next_page: bool,
    #[serde(default, rename = "endCursor")]
    end_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LinearIssueNode {
    id: String,
    identifier: String,
    title: String,
    description: Option<String>,
    state: WorkflowStateName,
    priority: Option<i64>,
    #[serde(rename = "branchName")]
    branch_name: Option<String>,
    url: Option<String>,
    labels: LinearLabelConnection,
    relations: LinearRelationConnection,
    comments: LinearCommentConnection,
    #[serde(rename = "createdAt")]
    created_at: Option<String>,
    #[serde(rename = "updatedAt")]
    updated_at: Option<String>,
}

impl LinearIssueNode {
    fn into_issue(self) -> LinearIssue {
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
struct WorkflowStateName {
    name: String,
}

#[derive(Debug, Deserialize)]
struct WorkflowStateNode {
    id: String,
    name: String,
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
    parent: Option<LinearCommentParentNode>,
    #[serde(rename = "createdAt")]
    created_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LinearCommentParentNode {
    id: String,
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

    if comment
        .parent
        .as_ref()
        .is_some_and(|parent| !parent.id.is_empty())
    {
        return true;
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

#[derive(Debug, Deserialize)]
struct RelatedIssueNode {
    id: String,
    identifier: String,
    state: WorkflowStateName,
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
    #[error("linear sdk error: {0}")]
    Sdk(#[from] lineark_sdk::LinearError),
}

const CANDIDATE_ISSUES_QUERY: &str = r#"
query CandidateIssues($teamKey: String!, $projectId: ID!, $projectMilestoneId: ID!, $states: [String!], $after: String) {
  issues(
    filter: {
      team: { key: { eq: $teamKey } }
      project: { id: { eq: $projectId } }
      projectMilestone: { id: { eq: $projectMilestoneId } }
      state: { name: { in: $states } }
    }
    first: 100
    after: $after
  ) {
    pageInfo {
      hasNextPage
      endCursor
    }
    nodes {
      id
      identifier
      title
      description
      state { name }
      priority
      branchName
      url
      labels { nodes { name } }
      comments(last: 50, orderBy: createdAt) {
        nodes {
          body
          parent { id }
          createdAt
        }
      }
      relations {
        nodes {
          type
          relatedIssue {
            id
            identifier
            state { name }
          }
        }
      }
      createdAt
      updatedAt
    }
  }
}
"#;

const ISSUE_STATES_QUERY: &str = r#"
query IssueStates($issueId: String!) {
  issue(id: $issueId) {
    team {
      states {
        nodes {
          id
          name
        }
      }
    }
  }
}
"#;

const UPDATE_ISSUE_STATE_MUTATION: &str = r#"
mutation UpdateIssueState($issueId: String!, $stateId: String!) {
  issueUpdate(id: $issueId, input: { stateId: $stateId }) {
    success
  }
}
"#;

const CREATE_COMMENT_MUTATION: &str = r#"
mutation CreateIssueEvidence($issueId: String!, $body: String!) {
  commentCreate(input: { issueId: $issueId, body: $body }) {
    success
  }
}
"#;
