use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::{
    config::RootConfig,
    runner::{
        RunnerSessionTreeActivity, RunnerSessionTreeMetrics, read_omp_session_tree_activity,
        read_omp_session_tree_metrics, read_session_tree_activity,
    },
    state::{
        CleanupStatus, EvalRunRecord, GitRefRecord, IssueStateRecord, LifecycleStage,
        ProjectRuntimeLivenessRecord, RunnerSessionRecord, RunnerStage, RuntimeLivenessStatus,
    },
    storage::{SqliteStore, StorageError},
};

pub const AGGREGATE_DASHBOARD_ENDPOINT: &str = "/api/dashboard";
pub const UI_AGGREGATE_DASHBOARD_ENDPOINT: &str = "/api/dashboard/ui";
pub const DASHBOARD_EVENTS_ENDPOINT: &str = "/api/dashboard/events";
pub const PROJECT_DRILLDOWN_ENDPOINT_TEMPLATE: &str = "/api/projects/{project_id}";
pub const ISSUE_DETAIL_ENDPOINT_TEMPLATE: &str = "/api/projects/{project_id}/issues/{issue_id}";

mod dashboard_contract;
mod self_defect_routing;
mod token_metrics;

pub use dashboard_contract::{
    DashboardEventStreamResponse, UiAggregateDashboardResponse, UiIssueDetailResponse,
    UiProjectDashboardResponse,
};
use dashboard_contract::{dashboard_event_stream_response, ui_aggregate_response};
pub use self_defect_routing::{
    ManagedSelfDefectProjection, SelfDefectRecommendationProjection, SelfDefectRouteSummary,
    SelfDefectRoutingProjection, SelfDefectSourceContext,
};
use self_defect_routing::{self_defect_route_summaries, self_defect_routing_projection};
pub use token_metrics::DashboardTokenMetrics;
use token_metrics::aggregate_token_metrics;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApiJsonResponse {
    pub status: u16,
    pub content_type: &'static str,
    pub body: String,
}

pub async fn runtime_api_json_response(
    config: &RootConfig,
    store: &SqliteStore,
    path: &str,
) -> Result<ApiJsonResponse, StorageError> {
    let api = RuntimeDashboardApi::from_store(config, store).await?;

    if path == AGGREGATE_DASHBOARD_ENDPOINT {
        return json_response(200, api.aggregate());
    }
    if path == UI_AGGREGATE_DASHBOARD_ENDPOINT {
        return json_response(200, &ui_aggregate_response(&api));
    }
    if path == DASHBOARD_EVENTS_ENDPOINT {
        return event_stream_response(200, &dashboard_event_stream_response(&api));
    }

    let Some(rest) = path.strip_prefix("/api/projects/") else {
        return json_response(404, &serde_json::json!({ "error": "not_found" }));
    };
    let parts = rest.split('/').collect::<Vec<_>>();

    match parts.as_slice() {
        [project_id] => api.project_drilldown(project_id)?.map_or_else(
            || json_response(404, &serde_json::json!({ "error": "project_not_found" })),
            |project| json_response(200, project),
        ),
        [project_id, "ui"] => api.project_drilldown(project_id)?.map_or_else(
            || json_response(404, &serde_json::json!({ "error": "project_not_found" })),
            |project| json_response(200, &UiProjectDashboardResponse::from(project)),
        ),
        [project_id, "issues", issue_id] => api.issue_detail(project_id, issue_id)?.map_or_else(
            || json_response(404, &serde_json::json!({ "error": "issue_not_found" })),
            |issue| json_response(200, issue),
        ),
        [project_id, "issues", issue_id, "ui"] => {
            api.issue_detail(project_id, issue_id)?.map_or_else(
                || json_response(404, &serde_json::json!({ "error": "issue_not_found" })),
                |issue| json_response(200, &UiIssueDetailResponse::from(issue)),
            )
        }
        _ => json_response(404, &serde_json::json!({ "error": "not_found" })),
    }
}

fn json_response<T: Serialize>(status: u16, value: &T) -> Result<ApiJsonResponse, StorageError> {
    Ok(ApiJsonResponse {
        status,
        content_type: "application/json",
        body: serde_json::to_string(value).map_err(StorageError::from)?,
    })
}

fn event_stream_response<T: Serialize>(
    status: u16,
    value: &T,
) -> Result<ApiJsonResponse, StorageError> {
    let data = serde_json::to_string(value).map_err(StorageError::from)?;
    Ok(ApiJsonResponse {
        status,
        content_type: "text/event-stream; charset=utf-8",
        body: format!("event: dashboard.snapshot\ndata: {data}\n\n"),
    })
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RuntimeReadModel {
    pub projects: Vec<ProjectReadModel>,
}

impl RuntimeReadModel {
    pub async fn from_store(store: &SqliteStore) -> Result<Self, StorageError> {
        let mut projects = Vec::new();

        for project in store.projects().await? {
            let issues = store.issues_for_project(&project.project_id).await?;
            let mut issue_models = Vec::with_capacity(issues.len());
            for issue in issues {
                issue_models.push(issue_read_model(store, issue).await?);
            }

            let liveness = store.project_liveness(&project.project_id).await?;
            projects.push(ProjectReadModel {
                project_id: project.project_id,
                name: project.name,
                enabled: project.enabled,
                lifecycle_stage: project.lifecycle_stage,
                cleanup_status: project.cleanup_status,
                liveness,
                issues: issue_models,
            });
        }

        Ok(Self { projects })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProjectReadModel {
    pub project_id: String,
    pub name: String,
    pub enabled: bool,
    pub lifecycle_stage: crate::state::LifecycleStage,
    pub cleanup_status: crate::state::CleanupStatus,
    pub liveness: Option<ProjectRuntimeLivenessRecord>,
    pub issues: Vec<IssueReadModel>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct IssueReadModel {
    pub issue: IssueStateRecord,
    pub runner_sessions: Vec<RunnerSessionRecord>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RuntimeDashboardApi {
    aggregate: AggregateDashboardResponse,
    projects: Vec<ProjectDashboardResponse>,
}

impl RuntimeDashboardApi {
    pub async fn from_store(
        config: &RootConfig,
        store: &SqliteStore,
    ) -> Result<Self, StorageError> {
        let runtime = RuntimeReadModel::from_store(store).await?;
        let mut projects = Vec::new();

        for project in runtime.projects {
            let configured = config.project(&project.project_id);
            let max_sessions = configured
                .map(|project| project.concurrency.max_sessions)
                .unwrap_or(0);
            projects.push(
                project_dashboard_response(
                    store,
                    project,
                    max_sessions,
                    config
                        .runner_archive
                        .as_ref()
                        .map(|storage| storage.database_path.clone()),
                )
                .await?,
            );
        }

        let project_cards = projects
            .iter()
            .map(ProjectDashboardResponse::card)
            .collect::<Vec<_>>();
        let aggregate = AggregateDashboardResponse {
            totals: aggregate_dashboard_totals(&project_cards),
            projects: project_cards,
        };

        Ok(Self {
            aggregate,
            projects,
        })
    }

    pub const fn aggregate(&self) -> &AggregateDashboardResponse {
        &self.aggregate
    }

    pub fn project_drilldown(
        &self,
        project_id: &str,
    ) -> Result<Option<&ProjectDashboardResponse>, StorageError> {
        Ok(self
            .projects
            .iter()
            .find(|project| project.project_id == project_id))
    }

    pub fn issue_detail(
        &self,
        project_id: &str,
        issue_id: &str,
    ) -> Result<Option<&IssueDetailResponse>, StorageError> {
        Ok(self
            .projects
            .iter()
            .find(|project| project.project_id == project_id)
            .and_then(|project| {
                project
                    .active_issues
                    .iter()
                    .chain(project.history_issues.iter())
                    .find(|issue| issue.issue_id == issue_id)
            }))
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AggregateDashboardResponse {
    pub totals: AggregateDashboardTotals,
    pub projects: Vec<ProjectDashboardCard>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AggregateDashboardTotals {
    pub project_count: usize,
    pub enabled_project_count: usize,
    pub running_issue_count: usize,
    pub available_sessions: u32,
    pub max_sessions: u32,
    pub running_tokens: u64,
    pub running_cached_tokens: u64,
    pub recorded_tokens: u64,
    pub token_metrics: DashboardTokenMetrics,
    pub running_cost_micros: u64,
    pub recorded_cost_micros: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProjectDashboardCard {
    pub project_id: String,
    pub name: String,
    pub enabled: bool,
    pub active_count: usize,
    pub parked_count: usize,
    pub terminal_count: usize,
    pub runner_health: String,
    pub last_event: String,
    pub capacity: ProjectCapacity,
    pub liveness: ProjectRuntimeLivenessResponse,
    pub cleanup_status: CleanupStatus,
    pub running_tokens: u64,
    pub running_cached_tokens: u64,
    pub recorded_tokens: u64,
    pub token_metrics: DashboardTokenMetrics,
    pub running_cost_micros: u64,
    pub recorded_cost_micros: u64,
    pub running_issues: Vec<RunningIssueSummary>,
    pub self_defect_routes: Vec<SelfDefectRouteSummary>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RunningIssueSummary {
    pub project_id: String,
    pub project_name: String,
    pub issue_id: String,
    pub identifier: String,
    pub title: String,
    pub display_status: String,
    pub session_id: Option<String>,
    pub preferred_runner_session_id: Option<String>,
    pub provider_mode: Option<crate::state::RuntimeProviderMode>,
    pub provider_id: Option<String>,
    pub process_id: Option<u32>,
    pub process_alive: Option<bool>,
    pub lifecycle_stage: Option<LifecycleStage>,
    pub stage: Option<RunnerStage>,
    pub agent: Option<String>,
    pub model: Option<String>,
    pub active_agent: Option<String>,
    pub active_model: Option<String>,
    pub token_count: u64,
    pub cached_token_count: u64,
    pub token_metrics: DashboardTokenMetrics,
    pub cost_micros: u64,
    pub subagents_used: u64,
    pub running_tool_count: u64,
    pub pending_tool_count: u64,
    pub todo_count: u64,
    pub started_at_ms: Option<u64>,
    pub duration_ms: Option<u64>,
    pub last_event: Option<String>,
    pub runtime_failure_kind: Option<crate::state::RuntimeFailureKind>,
    pub acp_frame_count: u64,
    pub session_evidence_refs: Vec<String>,
    pub silence_observed: bool,
    pub worktree_path: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProjectCapacity {
    pub max_sessions: u32,
    pub running_sessions: u32,
    pub available_sessions: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProjectRuntimeLivenessResponse {
    pub status: RuntimeLivenessStatus,
    pub reason: String,
    pub primary_reason_code: String,
    pub primary_reason_detail: String,
    pub last_poll_at: Option<String>,
    pub last_successful_candidate_scan_at: Option<String>,
    pub capacity: ProjectCapacity,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProjectDashboardResponse {
    pub project_id: String,
    pub name: String,
    pub enabled: bool,
    pub lifecycle_stage: LifecycleStage,
    pub cleanup_status: CleanupStatus,
    pub capacity: ProjectCapacity,
    pub liveness: ProjectRuntimeLivenessResponse,
    pub selected_candidate: Option<SelectedCandidateResponse>,
    pub suppression_reasons: Vec<CandidateSuppressionResponse>,
    pub active_issues: Vec<IssueDetailResponse>,
    pub token_metrics: DashboardTokenMetrics,
    pub history_issues: Vec<IssueDetailResponse>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SelectedCandidateResponse {
    pub issue_id: String,
    pub identifier: String,
    pub lifecycle_stage: LifecycleStage,
    pub reason: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CandidateSuppressionResponse {
    pub issue_id: String,
    pub identifier: String,
    pub reason_kind: String,
    pub reason: String,
}

impl ProjectDashboardResponse {
    fn card(&self) -> ProjectDashboardCard {
        let running_issues = self.running_issue_summaries();
        let running_tokens = running_issues
            .iter()
            .map(|issue| issue.token_count)
            .sum::<u64>();
        let running_cached_tokens = running_issues
            .iter()
            .map(|issue| issue.cached_token_count)
            .sum::<u64>();
        let running_cost_micros = running_issues
            .iter()
            .map(|issue| issue.cost_micros)
            .sum::<u64>();
        let recorded_tokens = self.recorded_tokens();
        let token_metrics =
            aggregate_token_metrics(running_issues.iter().map(|issue| &issue.token_metrics));
        let recorded_cost_micros = self.recorded_cost_micros();

        ProjectDashboardCard {
            project_id: self.project_id.clone(),
            name: self.name.clone(),
            enabled: self.enabled,
            active_count: self
                .active_issues
                .iter()
                .filter(|issue| issue.lifecycle_stage == LifecycleStage::Running)
                .count(),
            parked_count: self
                .active_issues
                .iter()
                .filter(|issue| issue.lifecycle_stage == LifecycleStage::Blocked)
                .count(),
            terminal_count: self.history_issues.len(),
            runner_health: project_runner_health(self),
            last_event: project_last_event(self),
            capacity: self.capacity.clone(),
            liveness: self.liveness.clone(),
            cleanup_status: self.cleanup_status,
            running_tokens,
            running_cached_tokens,
            recorded_tokens,
            token_metrics,
            running_cost_micros,
            recorded_cost_micros,
            running_issues,
            self_defect_routes: self_defect_route_summaries(
                self.active_issues.iter().chain(self.history_issues.iter()),
            ),
        }
    }

    fn running_issue_summaries(&self) -> Vec<RunningIssueSummary> {
        self.active_issues
            .iter()
            .filter(|issue| {
                issue.lifecycle_stage == LifecycleStage::Running
                    && issue
                        .runner_sessions
                        .iter()
                        .any(session_is_active_for_display)
            })
            .map(|issue| running_issue_summary(self, issue))
            .collect()
    }

    fn recorded_tokens(&self) -> u64 {
        self.active_issues
            .iter()
            .chain(self.history_issues.iter())
            .map(issue_recorded_tokens)
            .sum()
    }

    fn recorded_cost_micros(&self) -> u64 {
        self.active_issues
            .iter()
            .chain(self.history_issues.iter())
            .map(issue_recorded_cost_micros)
            .sum()
    }
}

fn aggregate_dashboard_totals(projects: &[ProjectDashboardCard]) -> AggregateDashboardTotals {
    AggregateDashboardTotals {
        project_count: projects.len(),
        enabled_project_count: projects.iter().filter(|project| project.enabled).count(),
        running_issue_count: projects
            .iter()
            .map(|project| project.running_issues.len())
            .sum(),
        available_sessions: projects
            .iter()
            .map(|project| project.capacity.available_sessions)
            .sum(),
        max_sessions: projects
            .iter()
            .map(|project| project.capacity.max_sessions)
            .sum(),
        running_tokens: projects.iter().map(|project| project.running_tokens).sum(),
        running_cached_tokens: projects
            .iter()
            .map(|project| project.running_cached_tokens)
            .sum(),
        recorded_tokens: projects.iter().map(|project| project.recorded_tokens).sum(),
        token_metrics: aggregate_running_token_metrics(projects),
        running_cost_micros: projects
            .iter()
            .map(|project| project.running_cost_micros)
            .sum(),
        recorded_cost_micros: projects
            .iter()
            .map(|project| project.recorded_cost_micros)
            .sum(),
    }
}

fn aggregate_running_token_metrics(projects: &[ProjectDashboardCard]) -> DashboardTokenMetrics {
    aggregate_token_metrics(
        projects
            .iter()
            .filter(|project| !project.running_issues.is_empty())
            .map(|project| &project.token_metrics),
    )
}

fn running_issue_summary(
    project: &ProjectDashboardResponse,
    issue: &IssueDetailResponse,
) -> RunningIssueSummary {
    let session = preferred_issue_session(&issue.runner_sessions);
    let activity = session.and_then(|session| session.activity.as_ref());

    RunningIssueSummary {
        project_id: project.project_id.clone(),
        project_name: project.name.clone(),
        issue_id: issue.issue_id.clone(),
        identifier: issue.identifier.clone(),
        title: issue.title.clone(),
        display_status: issue.display_status.clone(),
        session_id: session.map(|session| session.runner_session_id.clone()),
        preferred_runner_session_id: session.map(|session| session.runner_session_id.clone()),
        provider_mode: session.map(|session| session.provider_mode),
        provider_id: session.and_then(|session| session.provider_id.clone()),
        process_id: session.and_then(|session| session.process_id),
        process_alive: session.and_then(|session| session.process_alive),
        lifecycle_stage: session.map(|session| session.lifecycle_stage),
        stage: session.map(|session| session.current_stage),
        agent: session.map(|session| session.agent.clone()),
        model: session.and_then(|session| session.model.clone()),
        active_agent: session.and_then(|session| session.active_agent.clone()),
        active_model: session.and_then(|session| session.active_model.clone()),
        token_count: session.map_or(0, |session| session.token_count),
        cached_token_count: session.map_or(0, |session| session.cached_token_count),
        token_metrics: session.map_or_else(DashboardTokenMetrics::unavailable, |session| {
            session.token_metrics.clone()
        }),
        cost_micros: session.map_or(0, |session| session.cost_micros),
        subagents_used: session.map_or(0, |session| session.subagents_used),
        running_tool_count: activity.map_or(0, |activity| activity.running_tool_count),
        pending_tool_count: activity.map_or(0, |activity| activity.pending_tool_count),
        todo_count: session.map_or(0, |session| session.todo_count),
        started_at_ms: session.and_then(|session| session.started_at_ms),
        duration_ms: session.and_then(|session| session.duration_ms),
        last_event: session
            .and_then(|session| session.last_event.clone())
            .or_else(|| issue.last_runner_event.clone()),
        runtime_failure_kind: session.and_then(|session| session.runtime_failure_kind.clone()),
        acp_frame_count: session.map_or(0, |session| session.acp_frame_count),
        session_evidence_refs: session
            .map(|session| session.session_evidence_refs.clone())
            .unwrap_or_default(),
        silence_observed: session.is_some_and(|session| session.silence_observed),
        worktree_path: session.map(|session| session.worktree_path.clone()),
    }
}

fn issue_recorded_tokens(issue: &IssueDetailResponse) -> u64 {
    issue.token_metrics.accounted_total_token_count
}

fn issue_recorded_cost_micros(issue: &IssueDetailResponse) -> u64 {
    issue
        .runner_sessions
        .iter()
        .map(|session| session.cost_micros)
        .sum()
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct IssueDetailResponse {
    pub project_id: String,
    pub issue_id: String,
    pub identifier: String,
    pub title: String,
    pub lifecycle_stage: LifecycleStage,
    pub display_status: String,
    pub blocker: Option<crate::state::BlockerRecord>,
    pub failure: Option<crate::state::FailureRecord>,
    pub runtime_defect: Option<RuntimeDefectProjection>,
    pub self_defect_routing: Option<SelfDefectRoutingProjection>,
    pub git_ref: Option<GitRefRecord>,
    pub cleanup_status: CleanupStatus,
    pub stop_reason: Option<String>,
    pub last_runner_event: Option<String>,
    pub preferred_runner_session_id: Option<String>,
    pub token_metrics: DashboardTokenMetrics,
    pub runner_sessions: Vec<RunnerSessionDetail>,
    pub eval_results: Vec<EvalRunRecord>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RuntimeDefectProjection {
    pub classification: String,
    pub fingerprint: Option<String>,
    pub repair_attempt_count: u32,
    pub next_action: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RunnerSessionDetail {
    pub runner_session_id: String,
    pub provider_mode: crate::state::RuntimeProviderMode,
    pub provider_id: Option<String>,
    pub agent: String,
    pub model: Option<String>,
    pub worktree_path: String,
    pub process_id: Option<u32>,
    pub process_alive: Option<bool>,
    pub lifecycle_stage: LifecycleStage,
    pub current_stage: RunnerStage,
    pub stage_history: Vec<RunnerStage>,
    pub active_agent: Option<String>,
    pub active_model: Option<String>,
    pub subagents_used: u64,
    pub eval_stage: Option<String>,
    pub message_count: u64,
    pub todo_count: u64,
    pub part_count: u64,
    pub token_count: u64,
    pub cached_token_count: u64,
    pub token_metrics: DashboardTokenMetrics,
    pub cost_micros: u64,
    pub started_at_ms: Option<u64>,
    pub duration_ms: Option<u64>,
    pub lifecycle_marker: Option<String>,
    pub last_event: Option<String>,
    pub runtime_failure_kind: Option<crate::state::RuntimeFailureKind>,
    pub acp_frame_count: u64,
    pub session_evidence_refs: Vec<String>,
    pub silence_observed: bool,
    pub activity: Option<RunnerSessionTreeActivity>,
    pub activity_error: Option<String>,
}

async fn issue_read_model(
    store: &SqliteStore,
    issue: IssueStateRecord,
) -> Result<IssueReadModel, StorageError> {
    let runner_sessions = store
        .runner_sessions_for_issue(&issue.project_id, &issue.issue_id)
        .await?;
    Ok(IssueReadModel {
        issue,
        runner_sessions,
    })
}

async fn project_dashboard_response(
    store: &SqliteStore,
    project: ProjectReadModel,
    max_sessions: u32,
    runner_archive_database_path: Option<PathBuf>,
) -> Result<ProjectDashboardResponse, StorageError> {
    let capacity = project_capacity(&project, max_sessions);
    let mut liveness = project_liveness_response(&project, &capacity);

    let mut active_issues = Vec::new();
    let mut history_issues = Vec::new();
    for issue in project.issues {
        let detail =
            issue_detail_response(store, issue, runner_archive_database_path.as_ref()).await?;
        if matches!(
            detail.lifecycle_stage,
            LifecycleStage::Completed | LifecycleStage::Canceled
        ) {
            history_issues.push(detail);
        } else {
            active_issues.push(detail);
        }
    }

    let (primary_reason_code, primary_reason_detail) = primary_execution_reason(
        project.enabled,
        project.cleanup_status,
        &active_issues,
        &history_issues,
        &liveness,
    );
    liveness.primary_reason_code = primary_reason_code.into();
    liveness.primary_reason_detail = primary_reason_detail;
    let selected_candidate = selected_candidate_response(&active_issues);
    let suppression_reasons = suppression_reason_responses(&active_issues);
    let token_metrics = aggregate_token_metrics(
        active_issues
            .iter()
            .chain(history_issues.iter())
            .map(|issue| &issue.token_metrics),
    );

    Ok(ProjectDashboardResponse {
        project_id: project.project_id,
        name: project.name,
        enabled: project.enabled,
        lifecycle_stage: project.lifecycle_stage,
        cleanup_status: project.cleanup_status,
        capacity,
        liveness,
        selected_candidate,
        suppression_reasons,
        active_issues,
        token_metrics,
        history_issues,
    })
}

fn selected_candidate_response(
    active_issues: &[IssueDetailResponse],
) -> Option<SelectedCandidateResponse> {
    active_issues
        .iter()
        .filter(|issue| issue.lifecycle_stage == LifecycleStage::Running && issue.blocker.is_none())
        .min_by(|left, right| left.identifier.cmp(&right.identifier))
        .map(|issue| SelectedCandidateResponse {
            issue_id: issue.issue_id.clone(),
            identifier: issue.identifier.clone(),
            lifecycle_stage: issue.lifecycle_stage,
            reason: "active execution".into(),
        })
}

fn suppression_reason_responses(
    active_issues: &[IssueDetailResponse],
) -> Vec<CandidateSuppressionResponse> {
    active_issues
        .iter()
        .filter_map(|issue| {
            issue
                .blocker
                .as_ref()
                .map(|blocker| CandidateSuppressionResponse {
                    issue_id: issue.issue_id.clone(),
                    identifier: issue.identifier.clone(),
                    reason_kind: blocker.kind.clone(),
                    reason: blocker.message.clone(),
                })
        })
        .collect()
}

fn project_capacity(project: &ProjectReadModel, max_sessions: u32) -> ProjectCapacity {
    let running_sessions = project
        .issues
        .iter()
        .filter(|issue| issue_has_running_execution(issue))
        .count() as u32;
    ProjectCapacity {
        max_sessions,
        running_sessions,
        available_sessions: max_sessions.saturating_sub(running_sessions),
    }
}

fn issue_has_running_execution(issue: &IssueReadModel) -> bool {
    issue.issue.lifecycle_stage == LifecycleStage::Running
        || issue.runner_sessions.iter().any(|session| {
            session.lifecycle_stage == LifecycleStage::Running
                && !matches!(session.stage, RunnerStage::Failed | RunnerStage::Completed)
                && session.process_id.is_some()
        })
}

fn project_liveness_response(
    project: &ProjectReadModel,
    fallback_capacity: &ProjectCapacity,
) -> ProjectRuntimeLivenessResponse {
    project.liveness.as_ref().map_or_else(
        || {
            let reason = if project.enabled {
                "runtime has not reported a poll for this enabled project"
            } else {
                "project disabled"
            };
            ProjectRuntimeLivenessResponse {
                status: RuntimeLivenessStatus::InactiveRuntime,
                reason: reason.into(),
                primary_reason_code: RuntimeLivenessStatus::InactiveRuntime.as_str().into(),
                primary_reason_detail: reason.into(),
                last_poll_at: None,
                last_successful_candidate_scan_at: None,
                capacity: fallback_capacity.clone(),
            }
        },
        |liveness| ProjectRuntimeLivenessResponse {
            status: liveness.status,
            reason: liveness.reason.clone(),
            primary_reason_code: liveness.status.as_str().into(),
            primary_reason_detail: liveness.reason.clone(),
            last_poll_at: liveness.last_poll_at.clone(),
            last_successful_candidate_scan_at: liveness.last_successful_candidate_scan_at.clone(),
            capacity: ProjectCapacity {
                max_sessions: liveness.max_sessions,
                running_sessions: liveness.running_sessions,
                available_sessions: liveness.available_sessions,
            },
        },
    )
}

fn primary_execution_reason(
    project_enabled: bool,
    project_cleanup_status: CleanupStatus,
    active_issues: &[IssueDetailResponse],
    history_issues: &[IssueDetailResponse],
    liveness: &ProjectRuntimeLivenessResponse,
) -> (&'static str, String) {
    if !project_enabled {
        return (
            "disabled_project",
            "project is disabled in the Symphony configuration".into(),
        );
    }
    if matches!(
        project_cleanup_status,
        CleanupStatus::Pending | CleanupStatus::InProgress
    ) {
        return (
            "cleanup_pending",
            format!("project cleanup is {project_cleanup_status}"),
        );
    }
    if let Some(issue) = active_issues
        .iter()
        .find(|issue| issue_has_worktree_failure(issue))
    {
        return (
            "worktree_blocked",
            issue
                .failure
                .as_ref()
                .map(|failure| failure.message.clone())
                .unwrap_or_else(|| "runner worktree validation failed".into()),
        );
    }
    if let Some(issue) = active_issues
        .iter()
        .find(|issue| issue_has_git_closure_failure(issue))
    {
        return (
            "git_closure_blocked",
            issue
                .failure
                .as_ref()
                .map(|failure| failure.message.clone())
                .unwrap_or_else(|| "runner git closure validation failed".into()),
        );
    }
    if let Some(issue) = active_issues.iter().find(|issue| {
        issue.runtime_defect.is_some() && issue.lifecycle_stage != LifecycleStage::Running
    }) {
        if active_issues.iter().any(issue_has_provider_blocker) {
            return (
                "provider_blocker",
                "provider/runtime configuration blocks active execution".into(),
            );
        }
        return (
            "runtime_defect_blocked",
            issue
                .failure
                .as_ref()
                .map(|failure| failure.message.clone())
                .unwrap_or_else(|| "Symphony runtime defect requires repair".into()),
        );
    }
    if liveness.status == RuntimeLivenessStatus::RunnerProcessDead
        || active_issues.iter().any(issue_has_dead_runner)
    {
        return (
            "runner_dead",
            "a running runner session has no live runner process".into(),
        );
    }
    if active_issues.iter().any(issue_waits_for_handoff) {
        return (
            "waiting_for_handoff",
            "an active runner session is waiting in handoff".into(),
        );
    }
    if active_issues.iter().any(issue_has_active_runner_session) {
        if liveness.capacity.available_sessions > 0 {
            let suffix = if matches!(
                liveness.status,
                RuntimeLivenessStatus::NoEligibleIssues | RuntimeLivenessStatus::BlockedIssues
            ) {
                " and no additional runnable candidate"
            } else {
                ""
            };
            return (
                "active_runner_session",
                format!(
                    "runner session is actively executing; {} dispatch slot available{suffix}",
                    liveness.capacity.available_sessions
                ),
            );
        }
        return (
            "active_runner_session",
            "runner session is actively executing".into(),
        );
    }
    if liveness.capacity.available_sessions == 0 {
        return ("capacity_full", "project dispatch capacity is full".into());
    }
    if active_issues.iter().any(issue_has_owner_input_blocker) {
        return (
            "owner_input_parked",
            "an issue is parked for owner input".into(),
        );
    }
    if active_issues.iter().any(issue_has_provider_blocker) {
        return (
            "provider_blocker",
            "provider/runtime configuration blocks active execution".into(),
        );
    }
    if active_issues.iter().any(issue_has_linear_blocker) {
        return (
            "linear_blockers",
            "an issue is blocked by Linear dependencies".into(),
        );
    }
    if history_issues.iter().any(|issue| {
        matches!(
            issue.cleanup_status,
            CleanupStatus::Pending | CleanupStatus::InProgress
        )
    }) {
        return (
            "cleanup_pending",
            "completed issue cleanup is pending".into(),
        );
    }
    match liveness.status {
        RuntimeLivenessStatus::InactiveRuntime => ("inactive_runtime", liveness.reason.clone()),
        RuntimeLivenessStatus::NoEligibleIssues if liveness.capacity.running_sessions > 0 => {
            ("no_runnable_candidate", liveness.reason.clone())
        }
        RuntimeLivenessStatus::NoEligibleIssues => (
            "idle",
            "project has no runnable candidates after the latest scan".into(),
        ),
        RuntimeLivenessStatus::BlockedIssues => ("no_runnable_candidate", liveness.reason.clone()),
        RuntimeLivenessStatus::CapacityFull => ("capacity_full", liveness.reason.clone()),
        RuntimeLivenessStatus::HealthyCapacityAvailable => {
            ("capacity_available", liveness.reason.clone())
        }
        RuntimeLivenessStatus::RunnerProcessDead => ("runner_dead", liveness.reason.clone()),
        RuntimeLivenessStatus::RunnerSetupFailed => {
            ("runner_setup_failed", liveness.reason.clone())
        }
        RuntimeLivenessStatus::RunnerStaleKilled => {
            ("runner_stale_killed", liveness.reason.clone())
        }
    }
}

fn issue_has_dead_runner(issue: &IssueDetailResponse) -> bool {
    issue.lifecycle_stage == LifecycleStage::Running
        && issue
            .runner_sessions
            .iter()
            .any(|session| session.process_alive == Some(false))
}

fn issue_has_worktree_failure(issue: &IssueDetailResponse) -> bool {
    issue.lifecycle_stage == LifecycleStage::Failed
        && issue.failure.as_ref().is_some_and(|failure| {
            failure.fingerprint.as_deref() == Some("launch_failed")
                && failure.message.contains("worktree")
        })
}

fn issue_has_git_closure_failure(issue: &IssueDetailResponse) -> bool {
    issue.lifecycle_stage == LifecycleStage::Failed
        && issue.failure.as_ref().is_some_and(|failure| {
            failure.fingerprint.as_deref() == Some("git_closure_unverified")
                || failure.kind == "handoff_git_closure_failed"
        })
}

fn issue_waits_for_handoff(issue: &IssueDetailResponse) -> bool {
    issue.lifecycle_stage == LifecycleStage::Running
        && issue
            .runner_sessions
            .iter()
            .any(|session| session.current_stage == RunnerStage::Handoff)
}

fn issue_has_active_runner_session(issue: &IssueDetailResponse) -> bool {
    issue.lifecycle_stage == LifecycleStage::Running
        && issue.runner_sessions.iter().any(|session| {
            matches!(
                session.current_stage,
                RunnerStage::Starting
                    | RunnerStage::Running
                    | RunnerStage::Eval
                    | RunnerStage::Review
                    | RunnerStage::Silent
            )
        })
}

fn issue_has_owner_input_blocker(issue: &IssueDetailResponse) -> bool {
    issue.lifecycle_stage == LifecycleStage::Blocked
        && issue.blocker.as_ref().is_some_and(|blocker| {
            matches!(blocker.kind.as_str(), "owner_input" | "owner_question")
        })
}

fn issue_has_provider_blocker(issue: &IssueDetailResponse) -> bool {
    issue.lifecycle_stage == LifecycleStage::Blocked
        && issue
            .blocker
            .as_ref()
            .is_some_and(|blocker| blocker.kind == "provider_blocker")
}

fn issue_has_linear_blocker(issue: &IssueDetailResponse) -> bool {
    issue.lifecycle_stage == LifecycleStage::Blocked
        && issue
            .blocker
            .as_ref()
            .is_some_and(|blocker| blocker.kind == "linear_blocker")
}

async fn issue_detail_response(
    store: &SqliteStore,
    issue: IssueReadModel,
    runner_archive_database_path: Option<&PathBuf>,
) -> Result<IssueDetailResponse, StorageError> {
    let eval_results = store
        .eval_runs_for_issue(&issue.issue.project_id, &issue.issue.issue_id)
        .await?;
    let mut sessions = Vec::new();
    for session in issue.runner_sessions {
        sessions.push(session_detail(store, session, runner_archive_database_path).await?);
    }
    sessions.sort_by_key(session_display_priority);
    let preferred_session = preferred_issue_session(&sessions);
    let last_runner_event = preferred_session.and_then(|session| session.last_event.clone());
    let preferred_runner_session_id =
        preferred_session.map(|session| session.runner_session_id.clone());
    let stop_reason = if issue.issue.lifecycle_stage == LifecycleStage::Running {
        None
    } else {
        issue
            .issue
            .failure
            .as_ref()
            .map(|failure| failure.kind.clone())
            .or_else(|| {
                issue
                    .issue
                    .blocker
                    .as_ref()
                    .map(|blocker| blocker.kind.clone())
            })
    };
    let display_status = issue_display_status(&issue.issue, preferred_session);
    let runtime_defect = runtime_defect_projection(&issue.issue);
    let self_defect_routing = self_defect_routing_projection(store, &issue.issue).await?;
    let token_metrics =
        aggregate_token_metrics(sessions.iter().map(|session| &session.token_metrics));

    Ok(IssueDetailResponse {
        project_id: issue.issue.project_id,
        issue_id: issue.issue.issue_id,
        identifier: issue.issue.identifier,
        title: issue.issue.title,
        lifecycle_stage: issue.issue.lifecycle_stage,
        display_status,
        blocker: issue.issue.blocker,
        failure: issue.issue.failure,
        runtime_defect,
        self_defect_routing,
        git_ref: issue.issue.git_ref,
        cleanup_status: issue.issue.cleanup_status,
        stop_reason,
        last_runner_event,
        preferred_runner_session_id,
        token_metrics,
        runner_sessions: sessions,
        eval_results,
    })
}

fn preferred_issue_session(sessions: &[RunnerSessionDetail]) -> Option<&RunnerSessionDetail> {
    sessions
        .iter()
        .find(|session| session_is_active_for_display(session))
        .or_else(|| sessions.last())
}

fn session_display_priority(session: &RunnerSessionDetail) -> u8 {
    if session_is_active_for_display(session) {
        0
    } else {
        1
    }
}

fn session_is_active_for_display(session: &RunnerSessionDetail) -> bool {
    session.lifecycle_stage == LifecycleStage::Running
        && matches!(
            session.current_stage,
            RunnerStage::Starting
                | RunnerStage::Running
                | RunnerStage::Eval
                | RunnerStage::Review
                | RunnerStage::Handoff
                | RunnerStage::Silent
        )
}

fn runtime_defect_projection(issue: &IssueStateRecord) -> Option<RuntimeDefectProjection> {
    let failure = issue.failure.as_ref()?;
    if !matches!(
        failure.kind.as_str(),
        "runtime_defect" | "malformed_handoff" | "runtime_launch_failed"
    ) {
        return None;
    }

    Some(RuntimeDefectProjection {
        classification: failure.kind.clone(),
        fingerprint: failure.fingerprint.clone(),
        repair_attempt_count: failure.occurrence_count,
        next_action: runtime_defect_next_action(issue).into(),
    })
}

fn runtime_defect_next_action(issue: &IssueStateRecord) -> &'static str {
    match (issue.lifecycle_stage, issue.cleanup_status) {
        (_, CleanupStatus::Pending | CleanupStatus::InProgress) => "wait_for_cleanup",
        (LifecycleStage::Running, _) => "continue_repair",
        (LifecycleStage::Failed, _) => "queue_repair",
        (LifecycleStage::Blocked, _) => "unblock_before_repair",
        (LifecycleStage::Queued, _) => "start_repair",
        (LifecycleStage::Canceled | LifecycleStage::Completed, _) => "monitor",
    }
}

async fn session_detail(
    store: &SqliteStore,
    session: RunnerSessionRecord,
    runner_archive_database_path: Option<&PathBuf>,
) -> Result<RunnerSessionDetail, StorageError> {
    let stage_history = store
        .runner_stage_events_for_session(
            &session.project_id,
            &session.issue_id,
            &session.session_id,
        )
        .await?
        .into_iter()
        .map(|event| event.stage)
        .collect::<Vec<_>>();
    let stage_history = if stage_history.is_empty() {
        vec![session.stage]
    } else {
        stage_history
    };
    let process_id = session.process_id;
    let process_alive = process_alive(process_id).await;
    let (activity, activity_error) =
        session_activity(&session, runner_archive_database_path, process_alive).await;
    let token_metrics = DashboardTokenMetrics::from_session(&session);
    let cached_token_count = token_metrics.cached_token_count;
    let token_count = token_metrics.accounted_total_token_count;
    let (started_at_ms, duration_ms, omp_activity_error) =
        session_duration_inputs(&session, activity.as_ref()).await;

    Ok(RunnerSessionDetail {
        runner_session_id: session.session_id,
        provider_mode: session.provider_mode,
        provider_id: session.provider_id,
        agent: session.agent,
        model: session.model,
        worktree_path: session.worktree_path,
        process_id,
        process_alive,
        lifecycle_stage: session.lifecycle_stage,
        current_stage: session.stage,
        stage_history,
        active_agent: session.active_agent,
        active_model: session.active_model,
        subagents_used: session.subagent_count,
        eval_stage: session.eval_stage,
        message_count: session.message_count,
        todo_count: session.todo_count,
        part_count: session.part_count,
        token_count,
        cached_token_count,
        token_metrics,
        cost_micros: session.cost_micros,
        started_at_ms,
        duration_ms,
        lifecycle_marker: session.lifecycle_marker,
        last_event: session.last_event,
        runtime_failure_kind: session.runtime_failure_kind,
        acp_frame_count: session.acp_frame_count,
        session_evidence_refs: session.session_evidence_refs,
        silence_observed: session.silence_observed,
        activity,
        activity_error: activity_error.or(omp_activity_error),
    })
}

async fn session_activity(
    session: &RunnerSessionRecord,
    runner_archive_database_path: Option<&PathBuf>,
    process_alive: Option<bool>,
) -> (Option<RunnerSessionTreeActivity>, Option<String>) {
    if let Some(path) = runner_archive_database_path {
        match read_session_tree_activity(path.clone(), &session.session_id, 40).await {
            Ok(Some(activity)) => return (Some(activity), None),
            Ok(None) => {}
            Err(error) => return (None, Some(format!("parse failed: {error}"))),
        }
    }

    if session.provider_mode == crate::state::RuntimeProviderMode::OmpAcp {
        return match read_omp_session_tree_activity(&session.session_id, 40).await {
            Ok(Some(activity)) => (Some(activity), None),
            Ok(None) => (
                None,
                Some(omp_activity_absence_reason(session, process_alive)),
            ),
            Err(error) => (None, Some(format!("parse failed: {error}"))),
        };
    }

    (
        None,
        Some("not exposed by provider: runner archive activity is not configured".into()),
    )
}

fn omp_activity_absence_reason(
    session: &RunnerSessionRecord,
    process_alive: Option<bool>,
) -> String {
    if process_alive == Some(false)
        || matches!(session.stage, RunnerStage::Failed | RunnerStage::Completed)
        || !matches!(session.lifecycle_stage, LifecycleStage::Running)
    {
        return "stale: OMP JSONL activity was not observed for this stopped runner session".into();
    }
    "not yet observed: OMP JSONL activity for this session was not found".into()
}

async fn process_alive(process_id: Option<u32>) -> Option<bool> {
    let process_id = process_id?;
    Some(
        tokio::fs::try_exists(format!("/proc/{process_id}"))
            .await
            .unwrap_or(false),
    )
}

async fn session_duration_inputs(
    session: &RunnerSessionRecord,
    activity: Option<&RunnerSessionTreeActivity>,
) -> (Option<u64>, Option<u64>, Option<String>) {
    let activity_started_at_ms = session_activity_started_at_ms(&session.session_id, activity);
    let activity_duration_ms = session_activity_duration_ms(&session.session_id, activity);
    if activity_started_at_ms.is_some() {
        return (activity_started_at_ms, activity_duration_ms, None);
    }

    if session.provider_mode == crate::state::RuntimeProviderMode::OmpAcp {
        match read_omp_session_tree_metrics(&session.session_id).await {
            Ok(Some(metrics)) => {
                let (started_at_ms, duration_ms) = omp_metrics_duration_inputs(&metrics);
                if started_at_ms.is_some() {
                    return (started_at_ms, duration_ms, None);
                }
            }
            Ok(None) => {}
            Err(error) => return (None, None, Some(error.to_string())),
        }
    }

    (live_process_started_at_ms(session).await, None, None)
}

async fn live_process_started_at_ms(session: &RunnerSessionRecord) -> Option<u64> {
    if session.lifecycle_stage != LifecycleStage::Running
        || matches!(session.stage, RunnerStage::Failed | RunnerStage::Completed)
    {
        return None;
    }
    let process_id = session.process_id?;
    let process_uptime_seconds = process_uptime_seconds(process_id).await?;
    let boot_time_ms = boot_time_epoch_seconds().await?.checked_mul(1_000)?;
    boot_time_ms.checked_add(process_uptime_seconds.saturating_mul(1_000))
}

async fn process_uptime_seconds(process_id: u32) -> Option<u64> {
    let stat = tokio::fs::read_to_string(format!("/proc/{process_id}/stat"))
        .await
        .ok()?;
    let start_time_ticks = process_start_time_ticks(&stat)?;
    let ticks_per_second = clock_ticks_per_second().await?;
    Some(start_time_ticks / ticks_per_second)
}

async fn boot_time_epoch_seconds() -> Option<u64> {
    let stat = tokio::fs::read_to_string("/proc/stat").await.ok()?;
    stat.lines()
        .find_map(|line| line.strip_prefix("btime "))
        .and_then(|value| value.trim().parse().ok())
}

fn process_start_time_ticks(stat: &str) -> Option<u64> {
    let after_command = stat.rsplit_once(") ")?.1;
    after_command.split_whitespace().nth(19)?.parse().ok()
}

async fn clock_ticks_per_second() -> Option<u64> {
    let output = tokio::process::Command::new("getconf")
        .arg("CLK_TCK")
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    std::str::from_utf8(&output.stdout)
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn omp_metrics_duration_inputs(metrics: &RunnerSessionTreeMetrics) -> (Option<u64>, Option<u64>) {
    let duration_ms = metrics
        .started_at_ms
        .zip(metrics.last_updated_ms)
        .map(|(started, updated)| updated.saturating_sub(started));
    (metrics.started_at_ms, duration_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dashboard_api_uses_omp_metrics_started_at_for_duration_inputs() {
        let metrics = RunnerSessionTreeMetrics {
            started_at_ms: Some(1_000),
            last_updated_ms: Some(2_750),
            ..RunnerSessionTreeMetrics::default()
        };

        assert_eq!(
            omp_metrics_duration_inputs(&metrics),
            (Some(1_000), Some(1_750))
        );
    }

    #[test]
    fn project_card_token_metrics_match_running_issue_metrics_not_history() {
        let running = issue_response(
            "running",
            LifecycleStage::Running,
            vec![session_detail(
                "running-session",
                LifecycleStage::Running,
                RunnerStage::Running,
                300,
                120,
            )],
            token_metrics(300, 120, "available"),
        );
        let history = issue_response(
            "history",
            LifecycleStage::Completed,
            vec![session_detail(
                "history-session",
                LifecycleStage::Completed,
                RunnerStage::Completed,
                9_000,
                0,
            )],
            token_metrics(9_000, 0, "unavailable"),
        );
        let project = ProjectDashboardResponse {
            project_id: "symphony".into(),
            name: "Symphony".into(),
            enabled: true,
            lifecycle_stage: LifecycleStage::Running,
            cleanup_status: CleanupStatus::Clean,
            capacity: ProjectCapacity {
                max_sessions: 1,
                running_sessions: 1,
                available_sessions: 0,
            },
            liveness: ProjectRuntimeLivenessResponse {
                status: RuntimeLivenessStatus::HealthyCapacityAvailable,
                reason: "running".into(),
                primary_reason_code: "active_runner_session".into(),
                primary_reason_detail: "runner session is actively executing".into(),
                last_poll_at: None,
                last_successful_candidate_scan_at: None,
                capacity: ProjectCapacity {
                    max_sessions: 1,
                    running_sessions: 1,
                    available_sessions: 0,
                },
            },
            selected_candidate: None,
            suppression_reasons: Vec::new(),
            active_issues: vec![running],
            token_metrics: token_metrics(9_300, 120, "degraded"),
            history_issues: vec![history],
        };

        let card = project.card();

        assert_eq!(card.running_tokens, 300);
        assert_eq!(card.running_cached_tokens, 120);
        assert_eq!(card.token_metrics.accounted_total_token_count, 300);
        assert_eq!(card.token_metrics.cached_token_count, 120);
        assert_eq!(card.token_metrics.metrics_status, "available");
    }

    #[test]
    fn aggregate_dashboard_totals_token_metrics_ignore_idle_project_metrics() {
        let running_project = project_card(
            "running",
            vec![running_issue_summary(
                "running-issue",
                300,
                120,
                "available",
            )],
            token_metrics(300, 120, "available"),
        );
        let idle_project = project_card("idle", Vec::new(), token_metrics(0, 0, "unavailable"));

        let totals = aggregate_dashboard_totals(&[running_project, idle_project]);

        assert_eq!(totals.running_issue_count, 1);
        assert_eq!(totals.running_tokens, 300);
        assert_eq!(totals.running_cached_tokens, 120);
        assert_eq!(totals.token_metrics.accounted_total_token_count, 300);
        assert_eq!(totals.token_metrics.cached_token_count, 120);
        assert_eq!(totals.token_metrics.metrics_status, "available");
        assert_eq!(totals.token_metrics.metrics_freshness, "fresh");
        assert_eq!(totals.token_metrics.metrics_reason, None);
    }

    fn project_card(
        id: &str,
        running_issues: Vec<RunningIssueSummary>,
        token_metrics: DashboardTokenMetrics,
    ) -> ProjectDashboardCard {
        ProjectDashboardCard {
            project_id: id.into(),
            name: id.into(),
            enabled: true,
            active_count: running_issues.len(),
            parked_count: 0,
            terminal_count: 0,
            runner_health: "active".into(),
            last_event: "none".into(),
            capacity: ProjectCapacity {
                max_sessions: 1,
                running_sessions: running_issues.len() as u32,
                available_sessions: u32::from(running_issues.is_empty()),
            },
            liveness: ProjectRuntimeLivenessResponse {
                status: RuntimeLivenessStatus::HealthyCapacityAvailable,
                reason: "test".into(),
                primary_reason_code: "test".into(),
                primary_reason_detail: "test".into(),
                last_poll_at: None,
                last_successful_candidate_scan_at: None,
                capacity: ProjectCapacity {
                    max_sessions: 1,
                    running_sessions: running_issues.len() as u32,
                    available_sessions: u32::from(running_issues.is_empty()),
                },
            },
            cleanup_status: CleanupStatus::Clean,
            running_tokens: running_issues.iter().map(|issue| issue.token_count).sum(),
            running_cached_tokens: running_issues
                .iter()
                .map(|issue| issue.cached_token_count)
                .sum(),
            recorded_tokens: token_metrics.accounted_total_token_count,
            token_metrics,
            running_cost_micros: 0,
            recorded_cost_micros: 0,
            running_issues,
            self_defect_routes: Vec::new(),
        }
    }

    fn running_issue_summary(
        id: &str,
        token_count: u64,
        cached_token_count: u64,
        status: &str,
    ) -> RunningIssueSummary {
        RunningIssueSummary {
            project_id: "symphony".into(),
            project_name: "Symphony".into(),
            issue_id: id.into(),
            identifier: format!("SYM-{id}"),
            title: id.into(),
            display_status: "running".into(),
            session_id: Some(format!("session-{id}")),
            preferred_runner_session_id: Some(format!("session-{id}")),
            provider_mode: Some(crate::state::RuntimeProviderMode::OmpAcp),
            provider_id: Some("omp".into()),
            process_id: None,
            process_alive: None,
            lifecycle_stage: Some(LifecycleStage::Running),
            stage: Some(RunnerStage::Running),
            agent: Some("build".into()),
            model: Some("gpt-5.5".into()),
            active_agent: Some("build".into()),
            active_model: Some("gpt-5.5".into()),
            token_count,
            cached_token_count,
            token_metrics: token_metrics(token_count, cached_token_count, status),
            cost_micros: 0,
            subagents_used: 0,
            running_tool_count: 0,
            pending_tool_count: 0,
            todo_count: 0,
            started_at_ms: None,
            duration_ms: None,
            last_event: None,
            runtime_failure_kind: None,
            acp_frame_count: 0,
            session_evidence_refs: Vec::new(),
            silence_observed: false,
            worktree_path: None,
        }
    }

    fn token_metrics(total: u64, cached: u64, status: &str) -> DashboardTokenMetrics {
        DashboardTokenMetrics {
            accounted_total_token_count: total,
            non_cached_token_count: total.saturating_sub(cached),
            cached_token_count: cached,
            input_token_count: total.saturating_sub(cached),
            output_token_count: 0,
            reasoning_token_count: 0,
            cache_read_token_count: cached,
            cache_write_token_count: 0,
            reported_total_token_count: total,
            metrics_status: status.into(),
            metrics_source: "test".into(),
            metrics_freshness: if status == "unavailable" {
                "unavailable".into()
            } else {
                "fresh".into()
            },
            metrics_reason: (status != "available").then(|| "test metric state".into()),
        }
    }

    fn issue_response(
        id: &str,
        lifecycle_stage: LifecycleStage,
        runner_sessions: Vec<RunnerSessionDetail>,
        token_metrics: DashboardTokenMetrics,
    ) -> IssueDetailResponse {
        IssueDetailResponse {
            project_id: "symphony".into(),
            issue_id: id.into(),
            identifier: format!("SYM-{id}"),
            title: id.into(),
            lifecycle_stage,
            display_status: "running".into(),
            blocker: None,
            failure: None,
            runtime_defect: None,
            self_defect_routing: None,
            git_ref: None,
            cleanup_status: CleanupStatus::Clean,
            stop_reason: None,
            last_runner_event: None,
            preferred_runner_session_id: None,
            token_metrics,
            runner_sessions,
            eval_results: Vec::new(),
        }
    }

    fn session_detail(
        id: &str,
        lifecycle_stage: LifecycleStage,
        current_stage: RunnerStage,
        token_count: u64,
        cached_token_count: u64,
    ) -> RunnerSessionDetail {
        RunnerSessionDetail {
            runner_session_id: id.into(),
            provider_mode: crate::state::RuntimeProviderMode::OmpAcp,
            provider_id: Some("omp".into()),
            agent: "build".into(),
            model: Some("gpt-5.5".into()),
            worktree_path: "/tmp/worktree".into(),
            process_id: None,
            process_alive: None,
            lifecycle_stage,
            current_stage,
            stage_history: vec![current_stage],
            active_agent: Some("build".into()),
            active_model: Some("gpt-5.5".into()),
            subagents_used: 0,
            eval_stage: None,
            message_count: 0,
            todo_count: 0,
            part_count: 0,
            token_count,
            cached_token_count,
            token_metrics: token_metrics(token_count, cached_token_count, "available"),
            cost_micros: 0,
            started_at_ms: None,
            duration_ms: None,
            lifecycle_marker: None,
            last_event: None,
            runtime_failure_kind: None,
            acp_frame_count: 0,
            session_evidence_refs: Vec::new(),
            silence_observed: false,
            activity: None,
            activity_error: None,
        }
    }
}

fn session_activity_duration_ms(
    session_id: &str,
    activity: Option<&RunnerSessionTreeActivity>,
) -> Option<u64> {
    let activity = activity?;
    let root = session_activity_root(session_id, activity)?;
    let last_updated_ms = activity
        .last_updated_ms
        .or_else(|| {
            activity
                .sessions
                .iter()
                .chain(activity.subagents.iter())
                .map(|session| session.time_updated_ms)
                .max()
        })
        .unwrap_or(root.time_updated_ms);

    Some(last_updated_ms.saturating_sub(root.time_created_ms))
}

fn session_activity_started_at_ms(
    session_id: &str,
    activity: Option<&RunnerSessionTreeActivity>,
) -> Option<u64> {
    session_activity_root(session_id, activity?).map(|session| session.time_created_ms)
}

fn session_activity_root<'a>(
    session_id: &str,
    activity: &'a RunnerSessionTreeActivity,
) -> Option<&'a crate::runner::RunnerSessionActivity> {
    activity
        .sessions
        .iter()
        .find(|session| session.session_id == session_id)
        .or_else(|| {
            activity
                .sessions
                .iter()
                .find(|session| session.session_id == activity.root_session_id)
        })
}

fn issue_display_status(
    issue: &IssueStateRecord,
    latest_session: Option<&RunnerSessionDetail>,
) -> String {
    if runtime_defect_projection(issue).is_some() {
        return match issue.lifecycle_stage {
            LifecycleStage::Running => "runtime repair".into(),
            LifecycleStage::Failed => "runtime defect".into(),
            _ => "runtime defect".into(),
        };
    }

    if let Some(blocker) = &issue.blocker {
        return match blocker.kind.as_str() {
            "owner_input" | "owner_question" => "owner input".into(),
            "provider_blocker" => "provider/infra blocker".into(),
            "linear_blocker" => "blocked".into(),
            _ => blocker.kind.replace('_', " "),
        };
    }

    if let Some(failure) = &issue.failure
        && failure.kind == "eval_failure"
        && issue.lifecycle_stage == LifecycleStage::Running
    {
        return "repair loop".into();
    }

    if let Some(session) = latest_session {
        if session.silence_observed {
            return "silence observed".into();
        }
        if session.current_stage == RunnerStage::Eval {
            return "eval running".into();
        }
    }

    match (issue.lifecycle_stage, issue.cleanup_status) {
        (LifecycleStage::Running, _) => "running".into(),
        (LifecycleStage::Blocked, _) => "blocked".into(),
        (LifecycleStage::Completed, CleanupStatus::Pending) => "cleanup pending".into(),
        (LifecycleStage::Completed, CleanupStatus::InProgress) => "cleanup pending".into(),
        (LifecycleStage::Completed, CleanupStatus::Complete) => "completed cleanup".into(),
        (LifecycleStage::Completed, _) => "done".into(),
        (LifecycleStage::Canceled, _) => "canceled".into(),
        (LifecycleStage::Failed, _) => "failed".into(),
        (LifecycleStage::Queued, _) => "queued".into(),
    }
}

fn project_runner_health(project: &ProjectDashboardResponse) -> String {
    for issue in &project.active_issues {
        if issue.display_status == "repair loop" {
            return "repair loop".into();
        }
    }
    for issue in &project.active_issues {
        if issue.display_status == "provider/infra blocker" {
            return "provider/infra blocker".into();
        }
    }
    for issue in &project.active_issues {
        if issue.display_status == "eval running" {
            return "eval running".into();
        }
    }
    if project
        .active_issues
        .iter()
        .any(|issue| issue.lifecycle_stage == LifecycleStage::Running)
    {
        if project.capacity.available_sessions > 0 {
            "active/capacity_available".into()
        } else {
            "active".into()
        }
    } else if project.active_issues.is_empty() {
        "idle".into()
    } else if project
        .active_issues
        .iter()
        .any(issue_has_attention_blocker)
    {
        "blocked".into()
    } else {
        match project.liveness.primary_reason_code.as_str() {
            "capacity_available" | "healthy_capacity_available" => "capacity_available".into(),
            "no_runnable_candidate" => "no_runnable_candidate".into(),
            "idle" => "idle".into(),
            _ => "no_runnable_candidate".into(),
        }
    }
}

fn issue_has_attention_blocker(issue: &IssueDetailResponse) -> bool {
    issue.blocker.is_some()
        || issue.runtime_defect.is_some()
        || issue.failure.as_ref().is_some_and(|failure| {
            matches!(
                failure.kind.as_str(),
                "provider_blocker"
                    | "malformed_handoff"
                    | "handoff_git_closure_failed"
                    | "runtime_launch_failed"
            )
        })
}

fn project_last_event(project: &ProjectDashboardResponse) -> String {
    project
        .active_issues
        .iter()
        .chain(project.history_issues.iter())
        .rev()
        .find_map(|issue| issue.last_runner_event.clone())
        .unwrap_or_else(|| "none".into())
}
