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
    managed_project: &ProjectConfig,
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
    let policy = ManagedSelfDefectPolicy::for_failure(failure);
    let summary = runtime_self_defect_summary(message, failure, session, policy);
    let managed_issue = match store.open_self_defect_by_fingerprint(fingerprint).await? {
        Some(record) => LinearIssue {
            id: record.managed_issue_id,
            identifier: record.managed_issue_identifier,
            title: format!("Symphony self-defect: {fingerprint}"),
            description: None,
            state: "Todo".into(),
            priority: Some(policy.priority),
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
        None => match linear
            .find_managed_issue(managed_project, fingerprint)
            .await?
        {
            Some(issue) => issue,
            None => {
                linear
                    .create_managed_issue(
                        managed_project,
                        ManagedLinearIssueCreate {
                            source_issue_id: issue.id.clone(),
                            fingerprint: fingerprint.to_string(),
                            title: format!("Symphony self-defect: {fingerprint}"),
                            description: summary.clone(),
                            priority: policy.priority,
                            state: policy.state,
                            project_milestone_id: managed_issue_milestone_id(
                                project,
                                managed_project,
                                issue,
                            ),
                            label_ids: Vec::new(),
                        },
                    )
                    .await?
            }
        },
    };

    let relation = self_defect_relation(&issue.id, &managed_issue.id);
    let evidence_summary = runtime_self_defect_evidence_summary(&summary, relation.mode);
    linear
        .create_issue_relation(
            &relation.issue_id,
            &relation.related_issue_id,
            relation.kind,
        )
        .await?;
    linear
        .record_issue_evidence(
            &managed_issue.id,
            LinearIssueEvidence {
                kind: evidence_kind.into(),
                body: evidence_summary.clone(),
            },
        )
        .await?;

    Ok(store
        .record_self_defect_occurrence(&SelfDefectOccurrenceRecord {
            fingerprint: fingerprint.to_string(),
            defect_kind: failure.kind.clone(),
            category: policy.category.into(),
            severity: policy.severity.into(),
            initial_routing_decision: "managed_self_defect".into(),
            source_project_id: project.id.clone(),
            source_issue_id: issue.id.clone(),
            source_issue_identifier: issue.identifier.clone(),
            source_session_id: Some(session.session_id.clone()),
            source_process_id: session.process_id,
            managed_issue_id: managed_issue.id,
            managed_issue_identifier: managed_issue.identifier,
            latest_evidence_summary: evidence_summary,
            relation_mode: relation.mode,
        })
        .await?)
}

fn managed_issue_milestone_id(
    source_project: &ProjectConfig,
    managed_project: &ProjectConfig,
    source_issue: &LinearIssue,
) -> Option<String> {
    if source_project.id == managed_project.id {
        source_issue
            .project_milestone
            .as_ref()
            .map(|milestone| milestone.id.clone())
    } else {
        None
    }
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
    policy: ManagedSelfDefectPolicy,
) -> String {
    let fingerprint = failure
        .fingerprint
        .as_deref()
        .unwrap_or(failure.kind.as_str());
    format!(
        "Symphony runtime self-defect\nkind: {kind}\nfingerprint: {fingerprint}\nmanaged_severity: {severity}\nmanaged_state: {state}\nsource_project: {source_project}\nsource_issue: {source_issue}\nsession_id: {session_id}\nprocess_id: {process_id}\noccurrence: {occurrence}\nsummary: {message}",
        kind = failure.kind,
        severity = policy.severity,
        state = policy.state.state_name(),
        source_project = session.project_id,
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

fn runtime_self_defect_evidence_summary(
    summary: &str,
    relation_mode: SelfDefectRelationMode,
) -> String {
    format!(
        "{summary}\nrelation_mode: {relation_mode}",
        relation_mode = relation_mode.as_str()
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ManagedSelfDefectPolicy {
    severity: &'static str,
    category: &'static str,
    priority: i64,
    state: ManagedLinearIssueState,
}

impl ManagedSelfDefectPolicy {
    fn for_failure(failure: &FailureRecord) -> Self {
        let fingerprint = failure
            .fingerprint
            .as_deref()
            .unwrap_or(failure.kind.as_str());
        match fingerprint {
            "missing_handoff_sidecar"
            | "malformed_handoff_sidecar"
            | "incomplete_success_handoff"
            | "missing_git_closure"
            | "git_closure_unverified"
            | "launch_failed"
            | "session_id_mismatch" => Self::p0(failure_kind_category(failure)),
            "stale_failed_session_reuse"
            | "runtime_db_linear_divergence"
            | "cleanup_failed_after_accepted_closure" => Self::p1(failure_kind_category(failure)),
            "dashboard_projection_gap_hides_live_execution" => {
                Self::p2(failure_kind_category(failure))
            }
            _ if failure.kind == "malformed_handoff" => Self::p0("handoff"),
            _ if failure.kind == "runtime_defect" => Self::p1("runtime"),
            _ => Self::p1("runtime"),
        }
    }

    const fn p0(category: &'static str) -> Self {
        Self {
            severity: "p0",
            category,
            priority: 1,
            state: ManagedLinearIssueState::Todo,
        }
    }

    const fn p1(category: &'static str) -> Self {
        Self {
            severity: "p1",
            category,
            priority: 2,
            state: ManagedLinearIssueState::Backlog,
        }
    }

    const fn p2(category: &'static str) -> Self {
        Self {
            severity: "p2",
            category,
            priority: 3,
            state: ManagedLinearIssueState::Backlog,
        }
    }
}

fn failure_kind_category(failure: &FailureRecord) -> &'static str {
    match failure.kind.as_str() {
        "malformed_handoff" => "handoff",
        "git_closure" => "git_closure",
        "cleanup" => "cleanup",
        "projection_gap" => "dashboard_api",
        _ => "runtime",
    }
}

struct SelfDefectRelation {
    issue_id: String,
    related_issue_id: String,
    kind: ManagedLinearRelation,
    mode: SelfDefectRelationMode,
}

fn self_defect_relation(source_issue_id: &str, managed_issue_id: &str) -> SelfDefectRelation {
    if source_issue_id == managed_issue_id {
        SelfDefectRelation {
            issue_id: source_issue_id.to_owned(),
            related_issue_id: managed_issue_id.to_owned(),
            kind: ManagedLinearRelation::Related,
            mode: SelfDefectRelationMode::RelatedOnly,
        }
    } else {
        SelfDefectRelation {
            issue_id: managed_issue_id.to_owned(),
            related_issue_id: source_issue_id.to_owned(),
            kind: ManagedLinearRelation::Blocks,
            mode: SelfDefectRelationMode::Blocking,
        }
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
        assert!(
            record
                .latest_evidence_summary
                .contains("relation_mode: related_only")
        );
        assert_eq!(
            linear.relations(),
            vec![(
                "issue-1".into(),
                "issue-1".into(),
                ManagedLinearRelation::Related
            )]
        );
        let evidence = linear.evidence();
        assert_eq!(evidence.len(), 1);
        assert!(evidence[0].1.body.contains("source_project: symphony"));
        assert!(evidence[0].1.body.contains("source_issue: issue-1"));
        assert!(evidence[0].1.body.contains("session_id: oc-session"));
        assert!(evidence[0].1.body.contains("process_id: 42"));
        assert!(
            evidence[0]
                .1
                .body
                .contains("fingerprint: fingerprint-related")
        );
        assert!(evidence[0].1.body.contains("relation_mode: related_only"));
    }

    #[test]
    fn evidence_summary_names_blocking_relation() {
        let summary = runtime_self_defect_evidence_summary(
            "fingerprint: launch_failed\nsource_project: symphony",
            SelfDefectRelationMode::Blocking,
        );

        assert!(summary.contains("fingerprint: launch_failed"));
        assert!(summary.contains("source_project: symphony"));
        assert!(summary.contains("relation_mode: blocking"));
    }

    #[tokio::test]
    async fn managed_self_defect_blocks_source_issue_not_the_reverse() {
        let store = test_store().await;
        let project = test_project();
        let source = linear_issue("source-issue", "NRV-10");
        let managed = linear_issue("managed-issue", "SYM-60");
        let linear = SameIssueLinearClient::new(managed);

        let record = record_runtime_self_defect(
            &project,
            &project,
            &store,
            &linear,
            RuntimeSelfDefectInput {
                issue: &source,
                evidence_kind: "runtime_defect",
                message: "missing handoff should route to self-defect",
                failure: &FailureRecord {
                    kind: "malformed_handoff".into(),
                    message: "missing sidecar".into(),
                    fingerprint: Some("missing_handoff_sidecar".into()),
                    occurrence_count: 1,
                },
                session: &OpenCodeSessionRecord {
                    project_id: project.id.clone(),
                    issue_id: source.id.clone(),
                    session_id: "oc-session".into(),
                    agent: "build".into(),
                    model: None,
                    worktree_path: "/tmp/worktree".into(),
                    process_id: None,
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

        assert_eq!(record.relation_mode, SelfDefectRelationMode::Blocking);
        assert_eq!(
            linear.relations(),
            vec![(
                "managed-issue".into(),
                "source-issue".into(),
                ManagedLinearRelation::Blocks
            )]
        );
    }

    #[test]
    fn deterministic_policy_routes_known_self_defects_by_severity() {
        let cases = [
            (
                "missing_handoff_sidecar",
                "malformed_handoff",
                "p0",
                ManagedLinearIssueState::Todo,
                1,
            ),
            (
                "malformed_handoff_sidecar",
                "malformed_handoff",
                "p0",
                ManagedLinearIssueState::Todo,
                1,
            ),
            (
                "missing_git_closure",
                "malformed_handoff",
                "p0",
                ManagedLinearIssueState::Todo,
                1,
            ),
            (
                "git_closure_unverified",
                "malformed_handoff",
                "p0",
                ManagedLinearIssueState::Todo,
                1,
            ),
            (
                "launch_failed",
                "runtime_defect",
                "p0",
                ManagedLinearIssueState::Todo,
                1,
            ),
            (
                "stale_failed_session_reuse",
                "runtime_defect",
                "p1",
                ManagedLinearIssueState::Backlog,
                2,
            ),
            (
                "runtime_db_linear_divergence",
                "runtime_defect",
                "p1",
                ManagedLinearIssueState::Backlog,
                2,
            ),
            (
                "cleanup_failed_after_accepted_closure",
                "cleanup",
                "p1",
                ManagedLinearIssueState::Backlog,
                2,
            ),
            (
                "dashboard_projection_gap_hides_live_execution",
                "projection_gap",
                "p2",
                ManagedLinearIssueState::Backlog,
                3,
            ),
        ];

        for (fingerprint, kind, severity, state, priority) in cases {
            let policy = ManagedSelfDefectPolicy::for_failure(&FailureRecord {
                kind: kind.into(),
                message: fingerprint.into(),
                fingerprint: Some(fingerprint.into()),
                occurrence_count: 1,
            });
            assert_eq!(policy.severity, severity, "{fingerprint}");
            assert_eq!(policy.state, state, "{fingerprint}");
            assert_eq!(policy.priority, priority, "{fingerprint}");
        }
    }

    struct SameIssueLinearClient {
        issue: LinearIssue,
        relations: Mutex<Vec<(String, String, ManagedLinearRelation)>>,
        evidence: Mutex<Vec<(String, LinearIssueEvidence)>>,
    }

    impl SameIssueLinearClient {
        fn new(issue: LinearIssue) -> Self {
            Self {
                issue,
                relations: Mutex::new(Vec::new()),
                evidence: Mutex::new(Vec::new()),
            }
        }

        fn relations(&self) -> Vec<(String, String, ManagedLinearRelation)> {
            self.relations.lock().expect("relations lock").clone()
        }

        fn evidence(&self) -> Vec<(String, LinearIssueEvidence)> {
            self.evidence.lock().expect("evidence lock").clone()
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
