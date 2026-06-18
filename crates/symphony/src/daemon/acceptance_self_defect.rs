use crate::{
    config::{ProjectConfig, RootConfig},
    linear::{LinearClient, LinearIssue},
    state::{
        FailureRecord, LifecycleStage, OpenCodeSessionRecord, OpenCodeStage, SelfDefectRecord,
    },
    storage::SqliteStore,
};

use super::self_defects::{RuntimeSelfDefectInput, record_runtime_self_defect};

pub(super) struct AcceptanceSelfDefectInput<'a> {
    pub source_project_id: &'a str,
    pub source_issue_identifier: &'a str,
    pub session_id: &'a str,
    pub fingerprint: &'a str,
    pub message: &'a str,
    pub process_id: Option<u32>,
}

pub(super) async fn record_acceptance_self_defect_with_linear_client(
    config: &RootConfig,
    store: &SqliteStore,
    linear: &impl LinearClient,
    input: AcceptanceSelfDefectInput<'_>,
) -> anyhow::Result<SelfDefectRecord> {
    let project = config.project(input.source_project_id).ok_or_else(|| {
        anyhow::anyhow!(
            "source project `{}` is not configured",
            input.source_project_id
        )
    })?;
    let managed_project = config.project("symphony").unwrap_or_else(|| {
        config
            .projects()
            .first()
            .expect("at least one configured project")
    });
    let issue = linear
        .fetch_candidate_issues(project)
        .await?
        .into_iter()
        .find(|issue| issue.identifier == input.source_issue_identifier)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "active source issue `{}` was not returned for project `{}`",
                input.source_issue_identifier,
                project.id
            )
        })?;

    let failure = FailureRecord {
        kind: "malformed_handoff".into(),
        message: input.message.into(),
        fingerprint: Some(input.fingerprint.into()),
        occurrence_count: 1,
    };
    let session = acceptance_session_record(project, &issue, input.session_id, input.process_id);

    record_runtime_self_defect(
        project,
        managed_project,
        store,
        linear,
        RuntimeSelfDefectInput {
            issue: &issue,
            evidence_kind: "acceptance_self_defect",
            message: input.message,
            failure: &failure,
            session: &session,
        },
    )
    .await
}

fn acceptance_session_record(
    project: &ProjectConfig,
    issue: &LinearIssue,
    session_id: &str,
    process_id: Option<u32>,
) -> OpenCodeSessionRecord {
    OpenCodeSessionRecord {
        project_id: project.id.clone(),
        issue_id: issue.id.clone(),
        session_id: session_id.into(),
        agent: "acceptance-self-defect".into(),
        model: None,
        worktree_path: project.repo_path.to_string_lossy().into_owned(),
        process_id,
        lifecycle_stage: LifecycleStage::Failed,
        stage: OpenCodeStage::Failed,
        active_agent: None,
        active_model: None,
        message_count: 0,
        todo_count: 0,
        part_count: 0,
        token_count: 0,
        cost_micros: 0,
        subagent_count: 0,
        eval_stage: None,
        lifecycle_marker: None,
        last_event: None,
        silence_observed: false,
    }
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, sync::Mutex};

    use crate::{
        config::{BranchPolicy, ConcurrencyConfig, EvalDefaults},
        linear::{
            LinearClientError, LinearIssueEvidence, LinearMilestone, LinearProjectConfig,
            ManagedLinearRelation,
        },
        opencode::{OpenCodeRuntimeConfig, PermissionPolicy},
        state::SelfDefectRelationMode,
    };

    use super::*;

    #[tokio::test]
    async fn acceptance_self_defect_uses_existing_source_issue_without_transitioning_it() {
        let store = test_store().await;
        let config = RootConfig::from_toml_str(test_config_toml()).expect("config");
        store.reconcile_projects(&config).await.expect("projects");
        let source = linear_issue_with_state("source-issue", "SYM-59", "In Progress");
        let managed = linear_issue("managed-issue", "SYM-60");
        let linear = RecordingLinearClient::new(managed, vec![source]);

        let record = record_acceptance_self_defect_with_linear_client(
            &config,
            &store,
            &linear,
            AcceptanceSelfDefectInput {
                source_project_id: "symphony",
                source_issue_identifier: "SYM-59",
                session_id: "live-acceptance-session",
                fingerprint: "malformed_handoff_sidecar",
                message: "controlled live acceptance occurrence",
                process_id: Some(1234),
            },
        )
        .await
        .expect("record acceptance self-defect");

        assert_eq!(record.source_issue_identifier, "SYM-59");
        assert_eq!(
            record.source_session_id.as_deref(),
            Some("live-acceptance-session")
        );
        assert_eq!(record.source_process_id, Some(1234));
        assert_eq!(record.relation_mode, SelfDefectRelationMode::RelatedOnly);
        assert_eq!(
            linear.transitions(),
            Vec::<(String, crate::linear::LinearTransition)>::new()
        );
        assert_eq!(
            linear.relations(),
            vec![(
                "source-issue".into(),
                "managed-issue".into(),
                ManagedLinearRelation::Related
            )]
        );
        let evidence = linear.evidence();
        assert_eq!(evidence.len(), 1);
        assert_eq!(evidence[0].0, "managed-issue");
        assert!(evidence[0].1.body.contains("kind: malformed_handoff"));
        assert!(evidence[0].1.body.contains("source_issue: source-issue"));
        assert!(
            evidence[0]
                .1
                .body
                .contains("session_id: live-acceptance-session")
        );
        assert!(
            evidence[0]
                .1
                .body
                .contains("skipped_blocker_reason: active_symphony_self_deadlock_prevention")
        );
    }

    struct RecordingLinearClient {
        managed: LinearIssue,
        candidates: Vec<LinearIssue>,
        relations: Mutex<Vec<(String, String, ManagedLinearRelation)>>,
        evidence: Mutex<Vec<(String, LinearIssueEvidence)>>,
        transitions: Mutex<Vec<(String, crate::linear::LinearTransition)>>,
    }

    impl RecordingLinearClient {
        fn new(managed: LinearIssue, candidates: Vec<LinearIssue>) -> Self {
            Self {
                managed,
                candidates,
                relations: Mutex::new(Vec::new()),
                evidence: Mutex::new(Vec::new()),
                transitions: Mutex::new(Vec::new()),
            }
        }

        fn relations(&self) -> Vec<(String, String, ManagedLinearRelation)> {
            self.relations.lock().expect("relations lock").clone()
        }

        fn evidence(&self) -> Vec<(String, LinearIssueEvidence)> {
            self.evidence.lock().expect("evidence lock").clone()
        }

        fn transitions(&self) -> Vec<(String, crate::linear::LinearTransition)> {
            self.transitions.lock().expect("transitions lock").clone()
        }
    }

    #[async_trait::async_trait]
    impl LinearClient for RecordingLinearClient {
        async fn fetch_candidate_issues(
            &self,
            _project: &ProjectConfig,
        ) -> Result<Vec<LinearIssue>, LinearClientError> {
            Ok(self.candidates.clone())
        }

        async fn transition_issue(
            &self,
            issue_id: &str,
            transition: crate::linear::LinearTransition,
        ) -> Result<(), LinearClientError> {
            self.transitions
                .lock()
                .expect("transitions lock")
                .push((issue_id.into(), transition));
            Ok(())
        }

        async fn find_managed_issue(
            &self,
            _project: &ProjectConfig,
            _fingerprint: &str,
        ) -> Result<Option<LinearIssue>, LinearClientError> {
            Ok(Some(self.managed.clone()))
        }

        async fn record_issue_evidence(
            &self,
            issue_id: &str,
            evidence: LinearIssueEvidence,
        ) -> Result<(), LinearClientError> {
            self.evidence
                .lock()
                .expect("evidence lock")
                .push((issue_id.into(), evidence));
            Ok(())
        }

        async fn create_issue_relation(
            &self,
            source_issue_id: &str,
            managed_issue_id: &str,
            relation: ManagedLinearRelation,
        ) -> Result<(), LinearClientError> {
            self.relations.lock().expect("relations lock").push((
                source_issue_id.into(),
                managed_issue_id.into(),
                relation,
            ));
            Ok(())
        }
    }

    async fn test_store() -> SqliteStore {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("runtime.sqlite3");
        let store = SqliteStore::open(path).await.expect("open");
        store.migrate().await.expect("migrate");
        std::mem::forget(dir);
        store
    }

    const fn test_config_toml() -> &'static str {
        r#"
[[projects]]
id = "symphony"
name = "Symphony"
enabled = true
workflow_path = "/tmp/workflow"
repo_path = "/tmp/repo"

[projects.branch]
base = "main"
worktree_root = "/tmp/worktrees"

[projects.linear]
team_key = "SYM"
project_id = "symphony-project"

[projects.opencode]
command = "opencode"
args = ["acp"]
agent = "build"
permission_policy = "reject"

[projects.eval]
default_suite = "default"

[projects.concurrency]
max_sessions = 1
"#
    }

    fn test_project() -> ProjectConfig {
        ProjectConfig {
            id: "symphony".into(),
            name: "Symphony".into(),
            enabled: true,
            workflow_path: PathBuf::from("/tmp/workflow"),
            repo_path: PathBuf::from("/tmp/repo"),
            mnemesh: None,
            branch: BranchPolicy {
                base: "main".into(),
                worktree_root: PathBuf::from("/tmp/worktrees"),
            },
            linear: LinearProjectConfig {
                team_key: "SYM".into(),
                project_id: Some("symphony-project".into()),
            },
            opencode: OpenCodeRuntimeConfig {
                command: PathBuf::from("opencode"),
                args: vec!["acp".into()],
                agent: "build".into(),
                model: None,
                effort: None,
                permission_policy: PermissionPolicy::Reject,
            },
            eval: EvalDefaults {
                default_suite: "default".into(),
                max_identical_failure_fingerprints: 2,
            },
            concurrency: ConcurrencyConfig { max_sessions: 1 },
        }
    }

    fn linear_issue(id: &str, identifier: &str) -> LinearIssue {
        linear_issue_with_state(id, identifier, "Todo")
    }

    fn linear_issue_with_state(id: &str, identifier: &str, state: &str) -> LinearIssue {
        LinearIssue {
            id: id.into(),
            identifier: identifier.into(),
            title: format!("{identifier} title"),
            description: Some("managed issue".into()),
            state: state.into(),
            priority: Some(1),
            branch_name: None,
            url: None,
            labels: Vec::new(),
            project_milestone: Some(LinearMilestone {
                id: "milestone".into(),
                name: "Milestone".into(),
            }),
            blocked_by: Vec::new(),
            has_new_owner_answer: false,
            owner_answer_created_at: None,
            created_at: None,
            updated_at: None,
        }
    }

    #[test]
    fn test_project_config_matches_toml_shape() {
        let config = RootConfig::from_toml_str(test_config_toml()).expect("config");
        assert_eq!(config.projects(), &[test_project()]);
    }
}
