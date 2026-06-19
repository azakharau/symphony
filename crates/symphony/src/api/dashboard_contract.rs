use serde::{Deserialize, Serialize};

use crate::{
    api::{
        AggregateDashboardResponse, AggregateDashboardTotals, CandidateSuppressionResponse,
        DASHBOARD_EVENTS_ENDPOINT, IssueDetailResponse, OpenCodeSessionDetail, ProjectCapacity,
        ProjectDashboardCard, ProjectDashboardResponse, ProjectRuntimeLivenessResponse,
        RunningIssueSummary, RuntimeDashboardApi, RuntimeDefectProjection,
        SelectedCandidateResponse, UI_AGGREGATE_DASHBOARD_ENDPOINT,
    },
    state::{CleanupStatus, EvalRunRecord, GitRefRecord, LifecycleStage, OpenCodeStage},
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UiAggregateDashboardResponse {
    pub metadata: DashboardContractMetadata,
    pub totals: UiAggregateDashboardTotals,
    pub projects: Vec<UiProjectDashboardCard>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DashboardContractMetadata {
    pub polling_fallback_endpoint: String,
    pub live_events_endpoint: String,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct UiAggregateDashboardTotals {
    pub project_count: usize,
    pub enabled_project_count: usize,
    pub running_issue_count: usize,
    pub available_sessions: u32,
    pub max_sessions: u32,
    pub running_tokens: u64,
    pub recorded_tokens: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UiProjectDashboardCard {
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
    pub recorded_tokens: u64,
    pub running_issues: Vec<UiRunningIssueSummary>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UiRunningIssueSummary {
    pub project_id: String,
    pub project_name: String,
    pub issue_id: String,
    pub identifier: String,
    pub title: String,
    pub display_status: String,
    pub session_id: Option<String>,
    pub process_id: Option<u32>,
    pub process_alive: Option<bool>,
    pub stage: Option<OpenCodeStage>,
    pub agent: Option<String>,
    pub model: Option<String>,
    pub active_agent: Option<String>,
    pub active_model: Option<String>,
    pub token_count: u64,
    pub subagents_used: u64,
    pub running_tool_count: u64,
    pub pending_tool_count: u64,
    pub todo_count: u64,
    pub last_event: Option<String>,
    pub worktree_path: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UiProjectDashboardResponse {
    pub metadata: DashboardContractMetadata,
    pub project_id: String,
    pub name: String,
    pub enabled: bool,
    pub lifecycle_stage: LifecycleStage,
    pub cleanup_status: CleanupStatus,
    pub capacity: ProjectCapacity,
    pub liveness: ProjectRuntimeLivenessResponse,
    pub selected_candidate: Option<SelectedCandidateResponse>,
    pub suppression_reasons: Vec<CandidateSuppressionResponse>,
    pub active_issues: Vec<UiIssueDetailResponse>,
    pub history_issues: Vec<UiIssueDetailResponse>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UiIssueDetailResponse {
    pub metadata: DashboardContractMetadata,
    pub project_id: String,
    pub issue_id: String,
    pub identifier: String,
    pub title: String,
    pub lifecycle_stage: LifecycleStage,
    pub display_status: String,
    pub blocker: Option<crate::state::BlockerRecord>,
    pub failure: Option<crate::state::FailureRecord>,
    pub runtime_defect: Option<RuntimeDefectProjection>,
    pub git_ref: Option<GitRefRecord>,
    pub cleanup_status: CleanupStatus,
    pub stop_reason: Option<String>,
    pub last_runner_event: Option<String>,
    pub opencode_sessions: Vec<UiOpenCodeSessionDetail>,
    pub eval_results: Vec<EvalRunRecord>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UiOpenCodeSessionDetail {
    pub opencode_session_id: String,
    pub agent: String,
    pub model: Option<String>,
    pub worktree_path: String,
    pub process_id: Option<u32>,
    pub process_alive: Option<bool>,
    pub lifecycle_stage: LifecycleStage,
    pub current_stage: OpenCodeStage,
    pub stage_history: Vec<OpenCodeStage>,
    pub active_agent: Option<String>,
    pub active_model: Option<String>,
    pub subagents_used: u64,
    pub eval_stage: Option<String>,
    pub message_count: u64,
    pub todo_count: u64,
    pub part_count: u64,
    pub token_count: u64,
    pub last_event: Option<String>,
    pub silence_observed: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DashboardEventStreamResponse {
    pub metadata: DashboardContractMetadata,
    pub snapshot: UiAggregateDashboardResponse,
}

pub fn ui_aggregate_response(api: &RuntimeDashboardApi) -> UiAggregateDashboardResponse {
    UiAggregateDashboardResponse::from(api.aggregate())
}

pub fn dashboard_event_stream_response(api: &RuntimeDashboardApi) -> DashboardEventStreamResponse {
    DashboardEventStreamResponse {
        metadata: DashboardContractMetadata::default(),
        snapshot: ui_aggregate_response(api),
    }
}

impl Default for DashboardContractMetadata {
    fn default() -> Self {
        Self {
            polling_fallback_endpoint: UI_AGGREGATE_DASHBOARD_ENDPOINT.into(),
            live_events_endpoint: DASHBOARD_EVENTS_ENDPOINT.into(),
        }
    }
}

impl From<&AggregateDashboardResponse> for UiAggregateDashboardResponse {
    fn from(response: &AggregateDashboardResponse) -> Self {
        Self {
            metadata: DashboardContractMetadata::default(),
            totals: UiAggregateDashboardTotals::from(&response.totals),
            projects: response
                .projects
                .iter()
                .map(UiProjectDashboardCard::from)
                .collect(),
        }
    }
}

impl From<&AggregateDashboardTotals> for UiAggregateDashboardTotals {
    fn from(totals: &AggregateDashboardTotals) -> Self {
        Self {
            project_count: totals.project_count,
            enabled_project_count: totals.enabled_project_count,
            running_issue_count: totals.running_issue_count,
            available_sessions: totals.available_sessions,
            max_sessions: totals.max_sessions,
            running_tokens: totals.running_tokens,
            recorded_tokens: totals.recorded_tokens,
        }
    }
}

impl From<&ProjectDashboardCard> for UiProjectDashboardCard {
    fn from(card: &ProjectDashboardCard) -> Self {
        Self {
            project_id: card.project_id.clone(),
            name: card.name.clone(),
            enabled: card.enabled,
            active_count: card.active_count,
            parked_count: card.parked_count,
            terminal_count: card.terminal_count,
            runner_health: card.runner_health.clone(),
            last_event: card.last_event.clone(),
            capacity: card.capacity.clone(),
            liveness: card.liveness.clone(),
            cleanup_status: card.cleanup_status,
            running_tokens: card.running_tokens,
            recorded_tokens: card.recorded_tokens,
            running_issues: card
                .running_issues
                .iter()
                .map(UiRunningIssueSummary::from)
                .collect(),
        }
    }
}

impl From<&RunningIssueSummary> for UiRunningIssueSummary {
    fn from(issue: &RunningIssueSummary) -> Self {
        Self {
            project_id: issue.project_id.clone(),
            project_name: issue.project_name.clone(),
            issue_id: issue.issue_id.clone(),
            identifier: issue.identifier.clone(),
            title: issue.title.clone(),
            display_status: issue.display_status.clone(),
            session_id: issue.session_id.clone(),
            process_id: issue.process_id,
            process_alive: issue.process_alive,
            stage: issue.stage,
            agent: issue.agent.clone(),
            model: issue.model.clone(),
            active_agent: issue.active_agent.clone(),
            active_model: issue.active_model.clone(),
            token_count: issue.token_count,
            subagents_used: issue.subagents_used,
            running_tool_count: issue.running_tool_count,
            pending_tool_count: issue.pending_tool_count,
            todo_count: issue.todo_count,
            last_event: issue.last_event.clone(),
            worktree_path: issue.worktree_path.clone(),
        }
    }
}

impl From<&ProjectDashboardResponse> for UiProjectDashboardResponse {
    fn from(project: &ProjectDashboardResponse) -> Self {
        Self {
            metadata: DashboardContractMetadata::default(),
            project_id: project.project_id.clone(),
            name: project.name.clone(),
            enabled: project.enabled,
            lifecycle_stage: project.lifecycle_stage,
            cleanup_status: project.cleanup_status,
            capacity: project.capacity.clone(),
            liveness: project.liveness.clone(),
            selected_candidate: project.selected_candidate.clone(),
            suppression_reasons: project.suppression_reasons.clone(),
            active_issues: project
                .active_issues
                .iter()
                .map(UiIssueDetailResponse::from)
                .collect(),
            history_issues: project
                .history_issues
                .iter()
                .map(UiIssueDetailResponse::from)
                .collect(),
        }
    }
}

impl From<&IssueDetailResponse> for UiIssueDetailResponse {
    fn from(issue: &IssueDetailResponse) -> Self {
        Self {
            metadata: DashboardContractMetadata::default(),
            project_id: issue.project_id.clone(),
            issue_id: issue.issue_id.clone(),
            identifier: issue.identifier.clone(),
            title: issue.title.clone(),
            lifecycle_stage: issue.lifecycle_stage,
            display_status: issue.display_status.clone(),
            blocker: issue.blocker.clone(),
            failure: issue.failure.clone(),
            runtime_defect: issue.runtime_defect.clone(),
            git_ref: issue.git_ref.clone(),
            cleanup_status: issue.cleanup_status,
            stop_reason: issue.stop_reason.clone(),
            last_runner_event: issue.last_runner_event.clone(),
            opencode_sessions: issue
                .opencode_sessions
                .iter()
                .map(UiOpenCodeSessionDetail::from)
                .collect(),
            eval_results: issue.eval_results.clone(),
        }
    }
}

impl From<&OpenCodeSessionDetail> for UiOpenCodeSessionDetail {
    fn from(session: &OpenCodeSessionDetail) -> Self {
        Self {
            opencode_session_id: session.opencode_session_id.clone(),
            agent: session.agent.clone(),
            model: session.model.clone(),
            worktree_path: session.worktree_path.clone(),
            process_id: session.process_id,
            process_alive: session.process_alive,
            lifecycle_stage: session.lifecycle_stage,
            current_stage: session.current_stage,
            stage_history: session.stage_history.clone(),
            active_agent: session.active_agent.clone(),
            active_model: session.active_model.clone(),
            subagents_used: session.subagents_used,
            eval_stage: session.eval_stage.clone(),
            message_count: session.message_count,
            todo_count: session.todo_count,
            part_count: session.part_count,
            token_count: session.token_count,
            last_event: session.last_event.clone(),
            silence_observed: session.silence_observed,
        }
    }
}
