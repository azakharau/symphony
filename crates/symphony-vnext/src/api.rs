use serde::{Deserialize, Serialize};

use crate::{
    config::RootConfig,
    state::{
        CleanupStatus, EvalRunRecord, GitRefRecord, IssueStateRecord, LifecycleStage,
        OpenCodeSessionRecord, OpenCodeStage,
    },
    storage::{SqliteStore, StorageError},
};

pub const AGGREGATE_DASHBOARD_ENDPOINT: &str = "/api/dashboard";
pub const PROJECT_DRILLDOWN_ENDPOINT_TEMPLATE: &str = "/api/projects/{project_id}";
pub const ISSUE_DETAIL_ENDPOINT_TEMPLATE: &str = "/api/projects/{project_id}/issues/{issue_id}";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RuntimeReadModel {
    pub projects: Vec<ProjectReadModel>,
}

impl RuntimeReadModel {
    pub fn from_store(store: &SqliteStore) -> Result<Self, StorageError> {
        let mut projects = Vec::new();

        for project in store.projects()? {
            let issues = store
                .issues_for_project(&project.project_id)?
                .into_iter()
                .map(|issue| issue_read_model(store, issue))
                .collect::<Result<Vec<_>, _>>()?;

            projects.push(ProjectReadModel {
                project_id: project.project_id,
                name: project.name,
                enabled: project.enabled,
                lifecycle_stage: project.lifecycle_stage,
                cleanup_status: project.cleanup_status,
                issues,
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
    pub issues: Vec<IssueReadModel>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct IssueReadModel {
    pub issue: IssueStateRecord,
    pub opencode_sessions: Vec<OpenCodeSessionRecord>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RuntimeDashboardApi {
    aggregate: AggregateDashboardResponse,
    projects: Vec<ProjectDashboardResponse>,
}

impl RuntimeDashboardApi {
    pub fn from_store(config: &RootConfig, store: &SqliteStore) -> Result<Self, StorageError> {
        let runtime = RuntimeReadModel::from_store(store)?;
        let mut projects = Vec::new();

        for project in runtime.projects {
            let configured = config.project(&project.project_id);
            let max_sessions = configured
                .map(|project| project.concurrency.max_sessions)
                .unwrap_or(0);
            projects.push(project_dashboard_response(store, project, max_sessions)?);
        }

        let aggregate = AggregateDashboardResponse {
            projects: projects
                .iter()
                .map(ProjectDashboardResponse::card)
                .collect(),
        };

        Ok(Self {
            aggregate,
            projects,
        })
    }

    pub fn aggregate(&self) -> &AggregateDashboardResponse {
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
    pub projects: Vec<ProjectDashboardCard>,
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
    pub cleanup_status: CleanupStatus,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProjectCapacity {
    pub max_sessions: u32,
    pub running_sessions: u32,
    pub available_sessions: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProjectDashboardResponse {
    pub project_id: String,
    pub name: String,
    pub enabled: bool,
    pub lifecycle_stage: LifecycleStage,
    pub cleanup_status: CleanupStatus,
    pub capacity: ProjectCapacity,
    pub active_issues: Vec<IssueDetailResponse>,
    pub history_issues: Vec<IssueDetailResponse>,
}

impl ProjectDashboardResponse {
    fn card(&self) -> ProjectDashboardCard {
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
            cleanup_status: self.cleanup_status,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct IssueDetailResponse {
    pub project_id: String,
    pub issue_id: String,
    pub identifier: String,
    pub title: String,
    pub linear_state: String,
    pub lifecycle_stage: LifecycleStage,
    pub display_status: String,
    pub blocker: Option<crate::state::BlockerRecord>,
    pub failure: Option<crate::state::FailureRecord>,
    pub git_ref: Option<GitRefRecord>,
    pub cleanup_status: CleanupStatus,
    pub stop_reason: Option<String>,
    pub last_runner_event: Option<String>,
    pub opencode_sessions: Vec<OpenCodeSessionDetail>,
    pub eval_results: Vec<EvalRunRecord>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OpenCodeSessionDetail {
    pub opencode_session_id: String,
    pub agent: String,
    pub model: Option<String>,
    pub worktree_path: String,
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
    pub cost_micros: u64,
    pub lifecycle_marker: Option<String>,
    pub last_event: Option<String>,
    pub silence_observed: bool,
}

fn issue_read_model(
    store: &SqliteStore,
    issue: IssueStateRecord,
) -> Result<IssueReadModel, StorageError> {
    let opencode_sessions =
        store.opencode_sessions_for_issue(&issue.project_id, &issue.issue_id)?;
    Ok(IssueReadModel {
        issue,
        opencode_sessions,
    })
}

fn project_dashboard_response(
    store: &SqliteStore,
    project: ProjectReadModel,
    max_sessions: u32,
) -> Result<ProjectDashboardResponse, StorageError> {
    let running_sessions = project
        .issues
        .iter()
        .filter(|issue| issue.issue.lifecycle_stage == LifecycleStage::Running)
        .count() as u32;
    let capacity = ProjectCapacity {
        max_sessions,
        running_sessions,
        available_sessions: max_sessions.saturating_sub(running_sessions),
    };

    let mut active_issues = Vec::new();
    let mut history_issues = Vec::new();
    for issue in project.issues {
        let detail = issue_detail_response(store, issue)?;
        if detail.lifecycle_stage == LifecycleStage::Completed {
            history_issues.push(detail);
        } else {
            active_issues.push(detail);
        }
    }

    Ok(ProjectDashboardResponse {
        project_id: project.project_id,
        name: project.name,
        enabled: project.enabled,
        lifecycle_stage: project.lifecycle_stage,
        cleanup_status: project.cleanup_status,
        capacity,
        active_issues,
        history_issues,
    })
}

fn issue_detail_response(
    store: &SqliteStore,
    issue: IssueReadModel,
) -> Result<IssueDetailResponse, StorageError> {
    let eval_results = store.eval_runs_for_issue(&issue.issue.project_id, &issue.issue.issue_id)?;
    let mut sessions = Vec::new();
    for session in issue.opencode_sessions {
        sessions.push(session_detail(store, session)?);
    }
    let last_runner_event = sessions
        .iter()
        .rev()
        .find_map(|session| session.last_event.clone());
    let stop_reason = issue
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
        });
    let display_status = issue_display_status(&issue.issue, sessions.last());

    Ok(IssueDetailResponse {
        project_id: issue.issue.project_id,
        issue_id: issue.issue.issue_id,
        identifier: issue.issue.identifier,
        title: issue.issue.title,
        linear_state: issue.issue.state,
        lifecycle_stage: issue.issue.lifecycle_stage,
        display_status,
        blocker: issue.issue.blocker,
        failure: issue.issue.failure,
        git_ref: issue.issue.git_ref,
        cleanup_status: issue.issue.cleanup_status,
        stop_reason,
        last_runner_event,
        opencode_sessions: sessions,
        eval_results,
    })
}

fn session_detail(
    store: &SqliteStore,
    session: OpenCodeSessionRecord,
) -> Result<OpenCodeSessionDetail, StorageError> {
    let stage_history = store
        .opencode_stage_events_for_session(
            &session.project_id,
            &session.issue_id,
            &session.session_id,
        )?
        .into_iter()
        .map(|event| event.stage)
        .collect::<Vec<_>>();
    let stage_history = if stage_history.is_empty() {
        vec![session.stage]
    } else {
        stage_history
    };

    Ok(OpenCodeSessionDetail {
        opencode_session_id: session.session_id,
        agent: session.agent,
        model: session.model,
        worktree_path: session.worktree_path,
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
        token_count: session.token_count,
        cost_micros: session.cost_micros,
        lifecycle_marker: session.lifecycle_marker,
        last_event: session.last_event,
        silence_observed: session.silence_observed,
    })
}

fn issue_display_status(
    issue: &IssueStateRecord,
    latest_session: Option<&OpenCodeSessionDetail>,
) -> String {
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
        if session.current_stage == OpenCodeStage::Eval {
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
        "active".into()
    } else if project.active_issues.is_empty() {
        "idle".into()
    } else {
        "parked".into()
    }
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
