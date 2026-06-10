mod graphql_model;
mod queries;
mod types;

use serde_json::{Value, json};
use tracing::{debug, info};

use crate::config::ProjectConfig;
use graphql_model::{LinearIssueConnection, LinearIssueNode, WorkflowStateNode};
use queries::{
    CANDIDATE_ISSUES_QUERY, CREATE_COMMENT_MUTATION, ISSUE_STATES_QUERY,
    UPDATE_ISSUE_STATE_MUTATION,
};
pub use types::{
    LinearBlocker, LinearClientError, LinearIssue, LinearIssueEvidence, LinearMilestone,
    LinearProjectConfig, LinearTransition,
};

const CANDIDATE_STATES: &[&str] = &[
    "Backlog",
    "Todo",
    "In Progress",
    "Need Owner Input",
    "Done",
    "Canceled",
    "Cancelled",
    "Closed",
    "Duplicate",
];

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
        let issues = fetch_candidate_issues_with(
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
        .await?;
        debug!(
            project_id = %project.id,
            issues = issues.len(),
            "Linear SDK fetched candidate issues"
        );
        Ok(issues)
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
        ensure_success(&response, "/success", "issueUpdate")?;
        info!(
            issue_id,
            state = transition.state_name(),
            "Linear SDK transitioned issue"
        );
        Ok(())
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
        ensure_success(&response, "/success", "commentCreate")?;
        info!(
            issue_id,
            kind = %evidence.kind,
            "Linear SDK recorded issue evidence"
        );
        Ok(())
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
        let issues = fetch_candidate_issues_with(
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
        .await?;
        debug!(
            project_id = %project.id,
            issues = issues.len(),
            "Linear GraphQL fetched candidate issues"
        );
        Ok(issues)
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
        ensure_success(&response, "/data/issueUpdate/success", "issueUpdate")?;
        info!(
            issue_id,
            state = transition.state_name(),
            "Linear GraphQL transitioned issue"
        );
        Ok(())
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
        ensure_success(&response, "/data/commentCreate/success", "commentCreate")?;
        info!(
            issue_id,
            kind = %evidence.kind,
            "Linear GraphQL recorded issue evidence"
        );
        Ok(())
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
    let mut issues = Vec::new();
    let mut after: Option<String> = None;

    loop {
        let connection = execute(GraphqlRequest {
            query: CANDIDATE_ISSUES_QUERY,
            variables: json!({
                "teamKey": project.linear.team_key,
                "projectId": project_id,
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
