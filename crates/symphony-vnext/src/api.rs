use serde::{Deserialize, Serialize};

use crate::{
    state::{IssueStateRecord, OpenCodeSessionRecord},
    storage::{SqliteStore, StorageError},
};

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
