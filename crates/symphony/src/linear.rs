mod graphql_model;
mod queries;
mod types;

use serde_json::{Value, json};
use tracing::{debug, info};

use crate::config::ProjectConfig;
use graphql_model::{LinearIssueConnection, LinearIssueNode, WorkflowStateNode};
use queries::{
    CANDIDATE_ISSUES_QUERY, CREATE_COMMENT_MUTATION, CREATE_ISSUE_RELATION_MUTATION,
    CREATE_MANAGED_ISSUE_MUTATION, ISSUE_STATES_QUERY, TEAM_CREATE_CONTEXT_QUERY,
    UPDATE_ISSUE_STATE_MUTATION,
};
pub use types::{
    LinearBlocker, LinearClientError, LinearIssue, LinearIssueEvidence, LinearMilestone,
    LinearProjectConfig, LinearTransition, LinearUpstreamContext, ManagedLinearIssueCreate,
    ManagedLinearIssueState, ManagedLinearRelation,
};

const CANDIDATE_STATES: &[&str] = &[
    "Backlog",
    "Todo",
    "In Progress",
    "Need Owner Input",
    "Done",
    "Canceled",
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

    async fn find_managed_issue(
        &self,
        project: &ProjectConfig,
        fingerprint: &str,
    ) -> Result<Option<LinearIssue>, LinearClientError> {
        Ok(self
            .fetch_candidate_issues(project)
            .await?
            .into_iter()
            .find(|issue| is_open_managed_issue(issue, fingerprint)))
    }

    async fn create_managed_issue(
        &self,
        _project: &ProjectConfig,
        _request: ManagedLinearIssueCreate,
    ) -> Result<LinearIssue, LinearClientError> {
        Err(LinearClientError::Message(
            "managed Linear issue creation is not configured".into(),
        ))
    }

    async fn create_issue_relation(
        &self,
        _source_issue_id: &str,
        _managed_issue_id: &str,
        _relation: ManagedLinearRelation,
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
pub struct ReqwestGraphqlTransport {
    client: reqwest::Client,
}

impl ReqwestGraphqlTransport {
    #[must_use]
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }
}

impl Default for ReqwestGraphqlTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl LinearGraphqlClient<ReqwestGraphqlTransport> {
    pub fn from_env() -> Result<Self, LinearClientError> {
        let api_key = std::env::var("LINEAR_API_KEY")
            .map_err(|_| LinearClientError::Message("LINEAR_API_KEY is required".into()))?;
        Ok(Self::new(
            "https://api.linear.app/graphql",
            api_key,
            ReqwestGraphqlTransport::new(),
        ))
    }
}

#[async_trait::async_trait]
impl LinearGraphqlTransport for ReqwestGraphqlTransport {
    async fn post_graphql(
        &self,
        endpoint: &str,
        api_key: &str,
        request: Value,
    ) -> Result<Value, LinearClientError> {
        let response = self
            .client
            .post(endpoint)
            .header("Authorization", api_key)
            .json(&request)
            .send()
            .await
            .map_err(|error| LinearClientError::Message(format!("linear http request: {error}")))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|error| LinearClientError::Message(format!("linear http body: {error}")))?;
        if !status.is_success() {
            return Err(LinearClientError::Message(format!(
                "linear http status {status}: {body}"
            )));
        }
        serde_json::from_str(&body)
            .map_err(|error| LinearClientError::Message(format!("decode linear response: {error}")))
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

    async fn create_managed_issue(
        &self,
        project: &ProjectConfig,
        request: ManagedLinearIssueCreate,
    ) -> Result<LinearIssue, LinearClientError> {
        let issue = create_managed_issue_with(
            |request| async move {
                self.client
                    .execute::<Value>(request.query, request.variables, request.data_path)
                    .await
                    .map_err(LinearClientError::from)
            },
            project,
            request,
        )
        .await?;
        info!(issue_id = %issue.id, "Linear SDK created managed issue");
        Ok(issue)
    }

    async fn create_issue_relation(
        &self,
        source_issue_id: &str,
        managed_issue_id: &str,
        relation: ManagedLinearRelation,
    ) -> Result<(), LinearClientError> {
        create_issue_relation_with(
            |request| async move {
                self.client
                    .execute::<Value>(request.query, request.variables, request.data_path)
                    .await
                    .map_err(LinearClientError::from)
            },
            source_issue_id,
            managed_issue_id,
            relation,
        )
        .await
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

    async fn create_managed_issue(
        &self,
        project: &ProjectConfig,
        request: ManagedLinearIssueCreate,
    ) -> Result<LinearIssue, LinearClientError> {
        let issue = create_managed_issue_with(
            |request| async move {
                self.post(json!({
                    "query": request.query,
                    "variables": request.variables,
                }))
                .await
            },
            project,
            request,
        )
        .await?;
        info!(issue_id = %issue.id, "Linear GraphQL created managed issue");
        Ok(issue)
    }

    async fn create_issue_relation(
        &self,
        source_issue_id: &str,
        managed_issue_id: &str,
        relation: ManagedLinearRelation,
    ) -> Result<(), LinearClientError> {
        create_issue_relation_with(
            |request| async move {
                self.post(json!({
                    "query": request.query,
                    "variables": request.variables,
                }))
                .await
            },
            source_issue_id,
            managed_issue_id,
            relation,
        )
        .await
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

async fn create_managed_issue_with<F, Fut>(
    mut execute: F,
    project: &ProjectConfig,
    request: ManagedLinearIssueCreate,
) -> Result<LinearIssue, LinearClientError>
where
    F: FnMut(GraphqlRequest) -> Fut,
    Fut: std::future::Future<Output = Result<Value, LinearClientError>>,
{
    let project_id = project
        .linear
        .project_id
        .as_deref()
        .ok_or_else(|| LinearClientError::Message("linear.project_id is required".into()))?;
    let context = execute(GraphqlRequest {
        query: TEAM_CREATE_CONTEXT_QUERY,
        variables: json!({ "teamKey": project.linear.team_key }),
        data_path: "teams",
    })
    .await?;
    let team = context
        .pointer("/nodes/0")
        .or_else(|| context.pointer("/teams/nodes/0"))
        .or_else(|| context.pointer("/data/teams/nodes/0"))
        .ok_or_else(|| LinearClientError::Message("missing Linear team context".into()))?;
    let team_id = team
        .pointer("/id")
        .and_then(Value::as_str)
        .ok_or_else(|| LinearClientError::Message("missing Linear team id".into()))?;
    let state_id = state_id_from_team(team, request.state.state_name())?;

    let mut input = json!({
        "teamId": team_id,
        "projectId": project_id,
        "title": request.title,
        "description": request.description_with_fingerprint(),
        "priority": request.priority,
        "stateId": state_id,
    });
    if let Some(project_milestone_id) = request.project_milestone_id {
        input["projectMilestoneId"] = json!(project_milestone_id);
    }
    if !request.label_ids.is_empty() {
        input["labelIds"] = json!(request.label_ids);
    }

    let response = execute(GraphqlRequest {
        query: CREATE_MANAGED_ISSUE_MUTATION,
        variables: json!({ "input": input }),
        data_path: "issueCreate",
    })
    .await?;
    ensure_success(&response, "/success", "issueCreate")
        .or_else(|_| ensure_success(&response, "/data/issueCreate/success", "issueCreate"))?;
    let issue = response
        .pointer("/issue")
        .or_else(|| response.pointer("/data/issueCreate/issue"))
        .cloned()
        .ok_or_else(|| LinearClientError::Message("missing created managed issue".into()))?;
    serde_json::from_value::<LinearIssueNode>(issue)
        .map(LinearIssueNode::into_issue)
        .map_err(|error| LinearClientError::Message(format!("decode managed issue: {error}")))
}

async fn create_issue_relation_with<F, Fut>(
    execute: F,
    source_issue_id: &str,
    managed_issue_id: &str,
    relation: ManagedLinearRelation,
) -> Result<(), LinearClientError>
where
    F: FnOnce(GraphqlRequest) -> Fut,
    Fut: std::future::Future<Output = Result<Value, LinearClientError>>,
{
    let relation = if source_issue_id == managed_issue_id {
        ManagedLinearRelation::Related
    } else {
        relation
    };
    let response = execute(GraphqlRequest {
        query: CREATE_ISSUE_RELATION_MUTATION,
        variables: json!({
            "issueId": source_issue_id,
            "relatedIssueId": managed_issue_id,
            "type": relation.relation_type(),
        }),
        data_path: "issueRelationCreate",
    })
    .await?;
    ensure_success(&response, "/success", "issueRelationCreate").or_else(|_| {
        ensure_success(
            &response,
            "/data/issueRelationCreate/success",
            "issueRelationCreate",
        )
    })
}

fn state_id_from_team(team: &Value, state_name: &str) -> Result<String, LinearClientError> {
    let states = team
        .pointer("/states/nodes")
        .cloned()
        .ok_or_else(|| LinearClientError::Message("missing team workflow states".into()))?;
    serde_json::from_value::<Vec<WorkflowStateNode>>(states)
        .map_err(|error| LinearClientError::Message(format!("decode states: {error}")))?
        .into_iter()
        .find(|state| state.name == state_name)
        .map(|state| state.id)
        .ok_or_else(|| LinearClientError::Message(format!("missing state `{state_name}`")))
}

fn is_open_managed_issue(issue: &LinearIssue, fingerprint: &str) -> bool {
    !matches!(issue.state.as_str(), "Done" | "Canceled")
        && issue
            .description
            .as_deref()
            .is_some_and(|description| description.contains(fingerprint))
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
