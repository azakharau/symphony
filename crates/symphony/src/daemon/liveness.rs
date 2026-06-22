use crate::{
    config::ProjectConfig,
    state::{LifecycleStage, RuntimeLivenessStatus},
    storage::SqliteStore,
};

use super::session::session_requires_resume;

pub(super) async fn project_liveness_projection(
    store: &SqliteStore,
    project: &ProjectConfig,
    running: u32,
    eligible: usize,
    blocked: usize,
    capacity: usize,
) -> anyhow::Result<(RuntimeLivenessStatus, String)> {
    if let Some(status) = project_runner_problem(store, &project.id).await? {
        let reason = match status {
            RuntimeLivenessStatus::RunnerSetupFailed => {
                "at least one runner session failed ACP setup before session attachment"
            }
            RuntimeLivenessStatus::RunnerStaleKilled => {
                "at least one stale runner process tree was terminated before continuation"
            }
            RuntimeLivenessStatus::RunnerProcessDead => {
                "at least one running runner session has no live runner process"
            }
            _ => "runner runner is not healthy",
        };
        return Ok((status, reason.into()));
    }
    if running >= project.concurrency.max_sessions {
        return Ok((
            RuntimeLivenessStatus::CapacityFull,
            "dispatch capacity is full".into(),
        ));
    }
    if eligible > 0 && capacity > 0 {
        return Ok((
            RuntimeLivenessStatus::HealthyCapacityAvailable,
            "eligible issue exists and dispatch capacity is available".into(),
        ));
    }
    if blocked > 0 {
        return Ok((
            RuntimeLivenessStatus::BlockedIssues,
            "candidate issues exist but are blocked or parked".into(),
        ));
    }
    Ok((
        RuntimeLivenessStatus::NoEligibleIssues,
        "candidate scan found no eligible issues".into(),
    ))
}

async fn project_runner_problem(
    store: &SqliteStore,
    project_id: &str,
) -> anyhow::Result<Option<RuntimeLivenessStatus>> {
    for issue in store.issues_for_project(project_id).await? {
        let sessions = store
            .runner_sessions_for_issue(project_id, &issue.issue_id)
            .await?;
        let Some(session) = sessions.into_iter().next_back() else {
            continue;
        };
        if session
            .lifecycle_marker
            .as_deref()
            .is_some_and(|marker| marker.starts_with("setup_failed:"))
        {
            return Ok(Some(RuntimeLivenessStatus::RunnerSetupFailed));
        }
        if session
            .last_event
            .as_deref()
            .is_some_and(|event| event.starts_with("stale_killed:"))
        {
            return Ok(Some(RuntimeLivenessStatus::RunnerStaleKilled));
        }
        if issue.lifecycle_stage != LifecycleStage::Running {
            continue;
        }
        if session_requires_resume(&session).await {
            return Ok(Some(RuntimeLivenessStatus::RunnerProcessDead));
        }
    }
    Ok(None)
}
