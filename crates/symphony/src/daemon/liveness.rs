use crate::{
    config::ProjectConfig,
    state::{LifecycleStage, RuntimeLivenessStatus},
    storage::SqliteStore,
};

use super::session_requires_resume;

pub(super) async fn project_liveness_projection(
    store: &SqliteStore,
    project: &ProjectConfig,
    running: u32,
    eligible: usize,
    blocked: usize,
    capacity: usize,
) -> anyhow::Result<(RuntimeLivenessStatus, String)> {
    if project_has_dead_runner(store, &project.id).await? {
        return Ok((
            RuntimeLivenessStatus::RunnerProcessDead,
            "at least one running OpenCode session has no live runner process".into(),
        ));
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

async fn project_has_dead_runner(store: &SqliteStore, project_id: &str) -> anyhow::Result<bool> {
    for issue in store.issues_for_project(project_id).await? {
        if issue.lifecycle_stage != LifecycleStage::Running {
            continue;
        }
        let mut sessions = store
            .opencode_sessions_for_issue(project_id, &issue.issue_id)
            .await?;
        sessions.sort_by(|left, right| left.session_id.cmp(&right.session_id));
        let Some(session) = sessions.into_iter().next_back() else {
            continue;
        };
        if session_requires_resume(&session).await {
            return Ok(true);
        }
    }
    Ok(false)
}
