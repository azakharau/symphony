use crate::{
    config::ProjectConfig,
    linear::{
        LinearClient, LinearIssue, LinearIssueEvidence, ManagedLinearIssueCreate,
        ManagedLinearIssueState, ManagedLinearRelation,
    },
    state::{
        FailureRecord, OpenCodeSessionRecord, SelfDefectOccurrenceRecord, SelfDefectRecord,
        SelfDefectRelationMode,
    },
    storage::SqliteStore,
};

pub(super) async fn record_runtime_self_defect(
    project: &ProjectConfig,
    store: &SqliteStore,
    linear: &impl LinearClient,
    input: RuntimeSelfDefectInput<'_>,
) -> anyhow::Result<SelfDefectRecord> {
    let RuntimeSelfDefectInput {
        issue,
        evidence_kind,
        message,
        failure,
        session,
    } = input;

    let fingerprint = failure
        .fingerprint
        .as_deref()
        .unwrap_or(failure.kind.as_str());
    let summary = runtime_self_defect_summary(message, failure, session);
    let managed_issue = match store.open_self_defect_by_fingerprint(fingerprint).await? {
        Some(record) => LinearIssue {
            id: record.managed_issue_id,
            identifier: record.managed_issue_identifier,
            title: format!("Symphony self-defect: {fingerprint}"),
            description: None,
            state: "Todo".into(),
            priority: Some(self_defect_priority(failure)),
            branch_name: None,
            url: None,
            labels: Vec::new(),
            project_milestone: None,
            blocked_by: Vec::new(),
            has_new_owner_answer: false,
            owner_answer_created_at: None,
            created_at: None,
            updated_at: None,
        },
        None => match linear.find_managed_issue(project, fingerprint).await? {
            Some(issue) => issue,
            None => {
                linear
                    .create_managed_issue(
                        project,
                        ManagedLinearIssueCreate {
                            source_issue_id: issue.id.clone(),
                            fingerprint: fingerprint.to_string(),
                            title: format!("Symphony self-defect: {fingerprint}"),
                            description: summary.clone(),
                            priority: self_defect_priority(failure),
                            state: ManagedLinearIssueState::Todo,
                            project_milestone_id: issue
                                .project_milestone
                                .as_ref()
                                .map(|milestone| milestone.id.clone()),
                            label_ids: Vec::new(),
                        },
                    )
                    .await?
            }
        },
    };

    let (relation_mode, linear_relation) = self_defect_relation(&issue.id, &managed_issue.id);
    linear
        .create_issue_relation(&issue.id, &managed_issue.id, linear_relation)
        .await?;
    linear
        .record_issue_evidence(
            &managed_issue.id,
            LinearIssueEvidence {
                kind: evidence_kind.into(),
                body: summary.clone(),
            },
        )
        .await?;

    Ok(store
        .record_self_defect_occurrence(&SelfDefectOccurrenceRecord {
            fingerprint: fingerprint.to_string(),
            defect_kind: failure.kind.clone(),
            category: "runtime".into(),
            severity: "blocking".into(),
            initial_routing_decision: "managed_self_defect".into(),
            source_project_id: project.id.clone(),
            source_issue_id: issue.id.clone(),
            source_issue_identifier: issue.identifier.clone(),
            source_session_id: Some(session.session_id.clone()),
            source_process_id: session.process_id,
            managed_issue_id: managed_issue.id,
            managed_issue_identifier: managed_issue.identifier,
            latest_evidence_summary: summary,
            relation_mode,
        })
        .await?)
}

pub(super) struct RuntimeSelfDefectInput<'a> {
    pub issue: &'a LinearIssue,
    pub evidence_kind: &'a str,
    pub message: &'a str,
    pub failure: &'a FailureRecord,
    pub session: &'a OpenCodeSessionRecord,
}

fn runtime_self_defect_summary(
    message: &str,
    failure: &FailureRecord,
    session: &OpenCodeSessionRecord,
) -> String {
    let fingerprint = failure
        .fingerprint
        .as_deref()
        .unwrap_or(failure.kind.as_str());
    format!(
        "Symphony runtime self-defect\nkind: {kind}\nfingerprint: {fingerprint}\nsource_issue: {source_issue}\nsession_id: {session_id}\nprocess_id: {process_id}\noccurrence: {occurrence}\nsummary: {message}",
        kind = failure.kind,
        source_issue = session.issue_id,
        session_id = session.session_id,
        process_id = session
            .process_id
            .map(|process_id| process_id.to_string())
            .unwrap_or_else(|| "none".into()),
        occurrence = failure.occurrence_count.max(1),
        message = bounded_line(message),
    )
}

fn bounded_line(input: &str) -> String {
    const MAX_BYTES: usize = 512;
    let collapsed = input.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.len() <= MAX_BYTES {
        return collapsed;
    }
    let mut end = MAX_BYTES;
    while !collapsed.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &collapsed[..end])
}

fn self_defect_priority(failure: &FailureRecord) -> i64 {
    match failure.kind.as_str() {
        "malformed_handoff" | "session_id_mismatch" => 1,
        _ => 2,
    }
}

fn self_defect_relation(
    source_issue_id: &str,
    managed_issue_id: &str,
) -> (SelfDefectRelationMode, ManagedLinearRelation) {
    if source_issue_id == managed_issue_id {
        (
            SelfDefectRelationMode::RelatedOnly,
            ManagedLinearRelation::Related,
        )
    } else {
        (
            SelfDefectRelationMode::Blocking,
            ManagedLinearRelation::Blocks,
        )
    }
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, sync::Mutex};

    use crate::{
        config::{BranchPolicy, ConcurrencyConfig, EvalDefaults},
        linear::{LinearBlocker, LinearClientError, LinearMilestone, LinearProjectConfig},
        opencode::{OpenCodeRuntimeConfig, PermissionPolicy},
        state::{LifecycleStage, OpenCodeStage},
    };

    use super::*;

    #[tokio::test]
    async fn records_related_only_when_managed_issue_is_source_issue() {
        let store = test_store().await;
        let project = test_project();
        let issue = linear_issue("issue-1", "SYM-1");
        let linear = SameIssueLinearClient::new(issue.clone());

        let record = record_runtime_self_defect(
            &project,
            &store,
            &linear,
            RuntimeSelfDefectInput {
                issue: &issue,
                evidence_kind: "runtime_defect",
                message: "self relation should remain related-only",
                failure: &FailureRecord {
                    kind: "malformed_handoff".into(),
                    message: "bad handoff".into(),
                    fingerprint: Some("fingerprint-related".into()),
                    occurrence_count: 1,
                },
                session: &OpenCodeSessionRecord {
                    project_id: project.id.clone(),
                    issue_id: issue.id.clone(),
                    session_id: "oc-session".into(),
                    agent: "rust-engineer".into(),
                    model: None,
                    worktree_path: "/tmp/worktree".into(),
                    process_id: Some(42),
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
                },
            },
        )
        .await
        .expect("record self-defect");

        assert_eq!(record.relation_mode, SelfDefectRelationMode::RelatedOnly);
        assert_eq!(
            linear.relations(),
            vec![(
                "issue-1".into(),
                "issue-1".into(),
                ManagedLinearRelation::Related
            )]
        );
    }

    struct SameIssueLinearClient {
        issue: LinearIssue,
        relations: Mutex<Vec<(String, String, ManagedLinearRelation)>>,
    }

    impl SameIssueLinearClient {
        fn new(issue: LinearIssue) -> Self {
            Self {
                issue,
                relations: Mutex::new(Vec::new()),
            }
        }

        fn relations(&self) -> Vec<(String, String, ManagedLinearRelation)> {
            self.relations.lock().expect("relations lock").clone()
        }
    }

    #[async_trait::async_trait]
    impl LinearClient for SameIssueLinearClient {
        async fn fetch_candidate_issues(
            &self,
            _project: &ProjectConfig,
        ) -> Result<Vec<LinearIssue>, LinearClientError> {
            Ok(Vec::new())
        }

        async fn transition_issue(
            &self,
            _issue_id: &str,
            _transition: crate::linear::LinearTransition,
        ) -> Result<(), LinearClientError> {
            Ok(())
        }

        async fn find_managed_issue(
            &self,
            _project: &ProjectConfig,
            _fingerprint: &str,
        ) -> Result<Option<LinearIssue>, LinearClientError> {
            Ok(Some(self.issue.clone()))
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
                project_id: None,
            },
            opencode: OpenCodeRuntimeConfig {
                command: PathBuf::from("opencode"),
                args: Vec::new(),
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
        LinearIssue {
            id: id.into(),
            identifier: identifier.into(),
            title: format!("{identifier} title"),
            description: Some("managed issue".into()),
            state: "Todo".into(),
            priority: Some(1),
            branch_name: None,
            url: None,
            labels: Vec::new(),
            project_milestone: Some(LinearMilestone {
                id: "milestone".into(),
                name: "Milestone".into(),
            }),
            blocked_by: Vec::<LinearBlocker>::new(),
            has_new_owner_answer: false,
            owner_answer_created_at: None,
            created_at: None,
            updated_at: None,
        }
    }
}
