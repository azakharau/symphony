mod recommendation;

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

use recommendation::record_ambiguous_self_defect_recommendation;

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
    let Some(policy) = ManagedSelfDefectPolicy::for_failure(failure) else {
        return record_ambiguous_self_defect_recommendation(
            project,
            store,
            fingerprint,
            message,
            failure,
            session,
            issue,
        )
        .await;
    };
    let summary = runtime_self_defect_summary(message, failure, session, policy);
    let managed_issue = match store.open_self_defect_by_fingerprint(fingerprint).await? {
        Some(record) => {
            open_registry_managed_issue(managed_project, linear, fingerprint, record, policy)
                .await?
        }
        None => {
            if let Some(record) = store.latest_self_defect_by_fingerprint(fingerprint).await? {
                return record_suppressed_duplicate_self_defect(
                    project, store, issue, failure, session, &summary, record,
                )
                .await;
            }

            match linear
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
            }
        }
    };

    let relation = self_defect_relation(project, managed_project, issue, &managed_issue);
    let evidence_summary = runtime_self_defect_evidence_summary(
        &summary,
        relation.mode,
        relation.skipped_blocker_reason,
    );
    if relation.issue_id != relation.related_issue_id {
        linear
            .create_issue_relation(
                &relation.issue_id,
                &relation.related_issue_id,
                relation.kind,
            )
            .await?;
    }
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

async fn record_suppressed_duplicate_self_defect(
    project: &ProjectConfig,
    store: &SqliteStore,
    issue: &LinearIssue,
    failure: &FailureRecord,
    session: &OpenCodeSessionRecord,
    summary: &str,
    existing: SelfDefectRecord,
) -> anyhow::Result<SelfDefectRecord> {
    let evidence_summary = format!(
        "{summary}\nrelation_mode: {relation_mode}\nduplicate_policy: existing_self_defect_fingerprint_suppressed",
        relation_mode = existing.relation_mode.as_str()
    );
    Ok(store
        .record_self_defect_occurrence(&SelfDefectOccurrenceRecord {
            fingerprint: existing.fingerprint,
            defect_kind: failure.kind.clone(),
            category: existing.category,
            severity: existing.severity,
            initial_routing_decision: existing.initial_routing_decision,
            source_project_id: project.id.clone(),
            source_issue_id: issue.id.clone(),
            source_issue_identifier: issue.identifier.clone(),
            source_session_id: Some(session.session_id.clone()),
            source_process_id: session.process_id,
            managed_issue_id: existing.managed_issue_id,
            managed_issue_identifier: existing.managed_issue_identifier,
            latest_evidence_summary: evidence_summary,
            relation_mode: existing.relation_mode,
        })
        .await?)
}

async fn open_registry_managed_issue(
    managed_project: &ProjectConfig,
    linear: &impl LinearClient,
    fingerprint: &str,
    record: SelfDefectRecord,
    policy: ManagedSelfDefectPolicy,
) -> anyhow::Result<LinearIssue> {
    let live_issue = linear
        .find_managed_issue(managed_project, fingerprint)
        .await?;
    if let Some(issue) = live_issue.filter(|issue| {
        issue.id == record.managed_issue_id || issue.identifier == record.managed_issue_identifier
    }) {
        return Ok(issue);
    }

    Ok(LinearIssue {
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
    })
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
    skipped_blocker_reason: Option<SkippedBlockerReason>,
) -> String {
    let mut evidence = format!(
        "{summary}\nrelation_mode: {relation_mode}",
        relation_mode = relation_mode.as_str()
    );
    if let Some(reason) = skipped_blocker_reason {
        evidence.push_str("\nskipped_blocker_reason: ");
        evidence.push_str(reason.as_str());
    }
    evidence
}

pub(super) fn bounded_line(input: &str) -> String {
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
    fn for_failure(failure: &FailureRecord) -> Option<Self> {
        let fingerprint = failure
            .fingerprint
            .as_deref()
            .unwrap_or(failure.kind.as_str());
        Some(match fingerprint {
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
            "live_acceptance_related_only" => Self::p2(failure_kind_category(failure)),
            "dashboard_projection_gap_hides_live_execution" => {
                Self::p2(failure_kind_category(failure))
            }
            _ if failure.kind == "malformed_handoff" => Self::p0("handoff"),
            _ => return None,
        })
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

pub(super) fn failure_kind_category(failure: &FailureRecord) -> &'static str {
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
    skipped_blocker_reason: Option<SkippedBlockerReason>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SkippedBlockerReason {
    SameIssue,
    ActiveSymphonySelfDeadlock,
    RelationCyclePrevention,
}

impl SkippedBlockerReason {
    const fn as_str(self) -> &'static str {
        match self {
            Self::SameIssue => "same_issue",
            Self::ActiveSymphonySelfDeadlock => "active_symphony_self_deadlock_prevention",
            Self::RelationCyclePrevention => "relation_cycle_prevention",
        }
    }
}

fn self_defect_relation(
    project: &ProjectConfig,
    managed_project: &ProjectConfig,
    source_issue: &LinearIssue,
    managed_issue: &LinearIssue,
) -> SelfDefectRelation {
    if source_issue.id == managed_issue.id {
        return related_only_relation(
            &source_issue.id,
            &managed_issue.id,
            SkippedBlockerReason::SameIssue,
        );
    }

    if project.id == managed_project.id && source_issue.state == "In Progress" {
        return related_only_relation(
            &source_issue.id,
            &managed_issue.id,
            SkippedBlockerReason::ActiveSymphonySelfDeadlock,
        );
    }

    if issue_is_blocked_by(managed_issue, source_issue) {
        return related_only_relation(
            &source_issue.id,
            &managed_issue.id,
            SkippedBlockerReason::RelationCyclePrevention,
        );
    }

    SelfDefectRelation {
        issue_id: managed_issue.id.clone(),
        related_issue_id: source_issue.id.clone(),
        kind: ManagedLinearRelation::Blocks,
        mode: SelfDefectRelationMode::Blocking,
        skipped_blocker_reason: None,
    }
}

fn related_only_relation(
    source_issue_id: &str,
    managed_issue_id: &str,
    reason: SkippedBlockerReason,
) -> SelfDefectRelation {
    SelfDefectRelation {
        issue_id: source_issue_id.to_owned(),
        related_issue_id: managed_issue_id.to_owned(),
        kind: ManagedLinearRelation::Related,
        mode: SelfDefectRelationMode::RelatedOnly,
        skipped_blocker_reason: Some(reason),
    }
}

fn issue_is_blocked_by(issue: &LinearIssue, blocker: &LinearIssue) -> bool {
    issue.blocked_by.iter().any(|candidate| {
        candidate.id.as_deref() == Some(blocker.id.as_str())
            || candidate.identifier.as_deref() == Some(blocker.identifier.as_str())
    })
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, sync::Mutex};

    use crate::{
        config::{BranchPolicy, ConcurrencyConfig, EvalDefaults},
        linear::{LinearBlocker, LinearClientError, LinearMilestone, LinearProjectConfig},
        opencode::{OpenCodeRuntimeConfig, PermissionPolicy},
        state::{LifecycleStage, OpenCodeStage, SelfDefectRecommendationConfidence},
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
        assert!(linear.relations().is_empty());
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
        assert!(
            evidence[0]
                .1
                .body
                .contains("skipped_blocker_reason: same_issue")
        );
    }

    #[tokio::test]
    async fn active_symphony_source_issue_gets_related_only_relation() {
        let store = test_store().await;
        let project = test_project();
        let source = linear_issue_with_state("source-issue", "SYM-55", "In Progress");
        let managed = linear_issue("managed-issue", "SYM-60");
        let linear = SameIssueLinearClient::new(managed);
        let failure = failure_record("active-sym-self-defect");
        let session = session_record(&project, &source);

        let record = record_runtime_self_defect(
            &project,
            &project,
            &store,
            &linear,
            RuntimeSelfDefectInput {
                issue: &source,
                evidence_kind: "runtime_defect",
                message: "active source should not be blocked by managed self-defect",
                failure: &failure,
                session: &session,
            },
        )
        .await
        .expect("record self-defect");

        assert_eq!(record.relation_mode, SelfDefectRelationMode::RelatedOnly);
        assert!(
            record
                .latest_evidence_summary
                .contains("skipped_blocker_reason: active_symphony_self_deadlock_prevention")
        );
        assert_eq!(
            linear.relations(),
            vec![(
                "source-issue".into(),
                "managed-issue".into(),
                ManagedLinearRelation::Related
            )]
        );
    }

    #[test]
    fn evidence_summary_names_blocking_relation() {
        let summary = runtime_self_defect_evidence_summary(
            "fingerprint: launch_failed\nsource_project: symphony",
            SelfDefectRelationMode::Blocking,
            None,
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

    #[tokio::test]
    async fn non_symphony_source_issue_keeps_blocking_relation() {
        let store = test_store().await;
        let project = other_project("nerva");
        let managed_project = test_project();
        let source = linear_issue_with_state("source-issue", "NRV-10", "In Progress");
        let managed = linear_issue("managed-issue", "SYM-60");
        let linear = SameIssueLinearClient::new(managed);
        let failure = failure_record("non-sym-self-defect");
        let session = session_record(&project, &source);

        let record = record_runtime_self_defect(
            &project,
            &managed_project,
            &store,
            &linear,
            RuntimeSelfDefectInput {
                issue: &source,
                evidence_kind: "runtime_defect",
                message: "non-symphony source may remain blocking",
                failure: &failure,
                session: &session,
            },
        )
        .await
        .expect("record self-defect");

        assert_eq!(record.relation_mode, SelfDefectRelationMode::Blocking);
        assert!(
            !record
                .latest_evidence_summary
                .contains("skipped_blocker_reason")
        );
        assert_eq!(
            linear.relations(),
            vec![(
                "managed-issue".into(),
                "source-issue".into(),
                ManagedLinearRelation::Blocks
            )]
        );
    }

    #[tokio::test]
    async fn relation_cycle_prevention_uses_related_only_relation() {
        let store = test_store().await;
        let project = test_project();
        let source = linear_issue("source-issue", "SYM-55");
        let managed = linear_issue("managed-issue", "SYM-60").blocked_by(vec![LinearBlocker {
            id: Some(source.id.clone()),
            identifier: Some(source.identifier.clone()),
            state: Some(source.state.clone()),
        }]);
        let linear = SameIssueLinearClient::new(managed);
        let failure = failure_record("cycle-self-defect");
        let session = session_record(&project, &source);

        let record = record_runtime_self_defect(
            &project,
            &project,
            &store,
            &linear,
            RuntimeSelfDefectInput {
                issue: &source,
                evidence_kind: "runtime_defect",
                message: "avoid visible blocker cycle",
                failure: &failure,
                session: &session,
            },
        )
        .await
        .expect("record self-defect");

        assert_eq!(record.relation_mode, SelfDefectRelationMode::RelatedOnly);
        assert!(
            record
                .latest_evidence_summary
                .contains("skipped_blocker_reason: relation_cycle_prevention")
        );
        assert_eq!(
            linear.relations(),
            vec![(
                "source-issue".into(),
                "managed-issue".into(),
                ManagedLinearRelation::Related
            )]
        );
    }

    #[tokio::test]
    async fn persisted_open_self_defect_reuse_keeps_cycle_prevention() {
        let store = test_store().await;
        let project = test_project();
        let source = linear_issue("source-issue", "SYM-55");
        let managed = linear_issue("managed-issue", "SYM-60").blocked_by(vec![LinearBlocker {
            id: Some(source.id.clone()),
            identifier: Some(source.identifier.clone()),
            state: Some(source.state.clone()),
        }]);
        let linear = SameIssueLinearClient::new(managed);
        let failure = failure_record("persisted-cycle-self-defect");
        let session = session_record(&project, &source);

        store
            .record_self_defect_occurrence(&SelfDefectOccurrenceRecord {
                fingerprint: "persisted-cycle-self-defect".into(),
                defect_kind: "malformed_handoff".into(),
                category: "handoff".into(),
                severity: "p0".into(),
                initial_routing_decision: "managed_self_defect".into(),
                source_project_id: project.id.clone(),
                source_issue_id: "previous-source".into(),
                source_issue_identifier: "SYM-1".into(),
                source_session_id: Some("previous-session".into()),
                source_process_id: None,
                managed_issue_id: "managed-issue".into(),
                managed_issue_identifier: "SYM-60".into(),
                latest_evidence_summary: "previous occurrence".into(),
                relation_mode: SelfDefectRelationMode::Blocking,
            })
            .await
            .expect("seed open self-defect");

        let record = record_runtime_self_defect(
            &project,
            &project,
            &store,
            &linear,
            RuntimeSelfDefectInput {
                issue: &source,
                evidence_kind: "runtime_defect",
                message: "reused registry row must still inspect live blockers",
                failure: &failure,
                session: &session,
            },
        )
        .await
        .expect("record self-defect");

        assert_eq!(record.relation_mode, SelfDefectRelationMode::RelatedOnly);
        assert!(
            record
                .latest_evidence_summary
                .contains("skipped_blocker_reason: relation_cycle_prevention")
        );
        assert_eq!(
            linear.relations(),
            vec![(
                "source-issue".into(),
                "managed-issue".into(),
                ManagedLinearRelation::Related
            )]
        );
    }

    #[tokio::test]
    async fn resolved_self_defect_recurrence_suppresses_new_linear_bug() {
        let store = test_store().await;
        let project = test_project();
        let source = linear_issue("source-issue", "MNE-202");
        let failure = failure_record("launch_failed");
        let session = session_record(&project, &source);
        let first = store
            .record_self_defect_occurrence(&SelfDefectOccurrenceRecord {
                fingerprint: "launch_failed".into(),
                defect_kind: failure.kind.clone(),
                category: "runtime".into(),
                severity: "p0".into(),
                initial_routing_decision: "managed_self_defect".into(),
                source_project_id: project.id.clone(),
                source_issue_id: "previous-source".into(),
                source_issue_identifier: "MNE-1".into(),
                source_session_id: Some("previous-session".into()),
                source_process_id: None,
                managed_issue_id: "deleted-managed-issue".into(),
                managed_issue_identifier: "SYM-90".into(),
                latest_evidence_summary: "previous occurrence".into(),
                relation_mode: SelfDefectRelationMode::Blocking,
            })
            .await
            .expect("seed self-defect");
        store
            .mark_self_defect_managed_issue_resolved(
                "deleted-managed-issue",
                crate::state::SelfDefectResolutionState::Done,
            )
            .await
            .expect("resolve self-defect");

        let record = record_runtime_self_defect(
            &project,
            &project,
            &store,
            &NoLinearWritesClient,
            RuntimeSelfDefectInput {
                issue: &source,
                evidence_kind: "runtime_defect",
                message: "OpenCode launch failed after Linear transition",
                failure: &failure,
                session: &session,
            },
        )
        .await
        .expect("record duplicate");

        assert_eq!(record.registry_id, first.registry_id);
        assert_eq!(record.managed_issue_identifier, "SYM-90");
        assert_eq!(record.occurrence_count, 2);
        assert!(
            record
                .latest_evidence_summary
                .contains("duplicate_policy: existing_self_defect_fingerprint_suppressed")
        );
        assert_eq!(
            store
                .self_defects_by_fingerprint("launch_failed")
                .await
                .expect("query")
                .len(),
            1
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
                "live_acceptance_related_only",
                "runtime_defect",
                "p2",
                ManagedLinearIssueState::Backlog,
                3,
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
            })
            .expect("known deterministic self-defect policy");
            assert_eq!(policy.severity, severity, "{fingerprint}");
            assert_eq!(policy.state, state, "{fingerprint}");
            assert_eq!(policy.priority, priority, "{fingerprint}");
        }
    }

    #[tokio::test]
    async fn ambiguous_self_defect_records_recommendation_without_linear_writes() {
        let store = test_store().await;
        let project = test_project();
        let source = linear_issue("source-issue", "SYM-55");
        let linear = SameIssueLinearClient::new(linear_issue("managed-issue", "SYM-60"));
        let failure = FailureRecord {
            kind: "ambiguous_runtime_signal".into(),
            message: "transient runtime warning without deterministic fingerprint".into(),
            fingerprint: Some("ambiguous-runtime-warning".into()),
            occurrence_count: 1,
        };
        let session = session_record(&project, &source);

        let record = record_runtime_self_defect(
            &project,
            &project,
            &store,
            &linear,
            RuntimeSelfDefectInput {
                issue: &source,
                evidence_kind: "runtime_defect",
                message: "transient runtime warning without deterministic fingerprint",
                failure: &failure,
                session: &session,
            },
        )
        .await
        .expect("record recommendation");

        assert_eq!(record.initial_routing_decision, "recommendation_only");
        assert_eq!(record.managed_issue_identifier, "recommendation-only");
        assert_eq!(record.severity, "low");
        assert_eq!(record.relation_mode, SelfDefectRelationMode::RelatedOnly);
        assert!(linear.relations().is_empty());
        assert!(linear.evidence().is_empty());
    }

    #[tokio::test]
    async fn high_confidence_ambiguous_self_defect_persists_typed_recommendation_only() {
        let store = test_store().await;
        let project = test_project();
        let source = linear_issue("source-issue", "SYM-55");
        let linear = SameIssueLinearClient::new(linear_issue("managed-issue", "SYM-60"));
        let failure = FailureRecord {
            kind: "ambiguous_runtime_signal".into(),
            message: "high confidence reproducible runtime recommendation evidence".into(),
            fingerprint: Some("ambiguous-high-confidence-runtime".into()),
            occurrence_count: 1,
        };
        let session = session_record(&project, &source);

        let record = record_runtime_self_defect(
            &project,
            &project,
            &store,
            &linear,
            RuntimeSelfDefectInput {
                issue: &source,
                evidence_kind: "runtime_defect",
                message: "high confidence reproducible runtime recommendation evidence",
                failure: &failure,
                session: &session,
            },
        )
        .await
        .expect("record high-confidence recommendation");
        let recommendation = store
            .open_self_defect_recommendation_by_evidence(&record.fingerprint)
            .await
            .expect("lookup recommendation")
            .expect("persisted recommendation");

        assert_eq!(record.initial_routing_decision, "recommendation_only");
        assert_eq!(record.managed_issue_identifier, "recommendation-only");
        assert_eq!(record.severity, "high");
        assert_eq!(record.relation_mode, SelfDefectRelationMode::RelatedOnly);
        assert_eq!(
            recommendation.confidence,
            SelfDefectRecommendationConfidence::High
        );
        assert!(
            recommendation
                .evidence_refs
                .contains(&"project:symphony".into())
        );
        assert!(
            recommendation
                .evidence_refs
                .contains(&"issue:SYM-55".into())
        );
        assert!(
            recommendation
                .evidence_refs
                .contains(&"session:oc-session".into())
        );
        assert!(
            recommendation
                .evidence_refs
                .contains(&"fingerprint:ambiguous-high-confidence-runtime".into())
        );
        assert!(linear.relations().is_empty());
        assert!(linear.evidence().is_empty());
    }

    #[tokio::test]
    async fn ambiguous_self_defect_dedupes_identical_evidence() {
        let store = test_store().await;
        let project = test_project();
        let source = linear_issue("source-issue", "SYM-55");
        let linear = SameIssueLinearClient::new(linear_issue("managed-issue", "SYM-60"));
        let failure = FailureRecord {
            kind: "ambiguous_runtime_signal".into(),
            message: "same ambiguous warning".into(),
            fingerprint: Some("ambiguous-runtime-warning".into()),
            occurrence_count: 1,
        };
        let session = session_record(&project, &source);

        let first = record_runtime_self_defect(
            &project,
            &project,
            &store,
            &linear,
            RuntimeSelfDefectInput {
                issue: &source,
                evidence_kind: "runtime_defect",
                message: "same ambiguous warning",
                failure: &failure,
                session: &session,
            },
        )
        .await
        .expect("first recommendation");
        let second = record_runtime_self_defect(
            &project,
            &project,
            &store,
            &linear,
            RuntimeSelfDefectInput {
                issue: &source,
                evidence_kind: "runtime_defect",
                message: "same ambiguous warning",
                failure: &failure,
                session: &session,
            },
        )
        .await
        .expect("second recommendation");

        assert_eq!(second.registry_id, first.registry_id);
        assert_eq!(second.occurrence_count, 2);
        assert!(linear.relations().is_empty());
        assert!(linear.evidence().is_empty());
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

    struct NoLinearWritesClient;

    #[async_trait::async_trait]
    impl LinearClient for NoLinearWritesClient {
        async fn fetch_candidate_issues(
            &self,
            _project: &ProjectConfig,
        ) -> Result<Vec<LinearIssue>, LinearClientError> {
            Err(LinearClientError::Message(
                "terminal duplicate must not query Linear".into(),
            ))
        }

        async fn transition_issue(
            &self,
            _issue_id: &str,
            _transition: crate::linear::LinearTransition,
        ) -> Result<(), LinearClientError> {
            Err(LinearClientError::Message(
                "terminal duplicate must not transition Linear".into(),
            ))
        }

        async fn record_issue_evidence(
            &self,
            _issue_id: &str,
            _evidence: LinearIssueEvidence,
        ) -> Result<(), LinearClientError> {
            Err(LinearClientError::Message(
                "terminal duplicate must not write Linear evidence".into(),
            ))
        }

        async fn create_managed_issue(
            &self,
            _project: &ProjectConfig,
            _request: ManagedLinearIssueCreate,
        ) -> Result<LinearIssue, LinearClientError> {
            Err(LinearClientError::Message(
                "terminal duplicate must not create Linear issue".into(),
            ))
        }

        async fn create_issue_relation(
            &self,
            _source_issue_id: &str,
            _managed_issue_id: &str,
            _relation: ManagedLinearRelation,
        ) -> Result<(), LinearClientError> {
            Err(LinearClientError::Message(
                "terminal duplicate must not create Linear relation".into(),
            ))
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
        other_project("symphony")
    }

    fn other_project(id: &str) -> ProjectConfig {
        ProjectConfig {
            id: id.into(),
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
            blocked_by: Vec::<LinearBlocker>::new(),
            has_new_owner_answer: false,
            owner_answer_created_at: None,
            created_at: None,
            updated_at: None,
        }
    }

    fn failure_record(fingerprint: &str) -> FailureRecord {
        FailureRecord {
            kind: "malformed_handoff".into(),
            message: fingerprint.into(),
            fingerprint: Some(fingerprint.into()),
            occurrence_count: 1,
        }
    }

    fn session_record(project: &ProjectConfig, issue: &LinearIssue) -> OpenCodeSessionRecord {
        OpenCodeSessionRecord {
            project_id: project.id.clone(),
            issue_id: issue.id.clone(),
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
        }
    }
}
