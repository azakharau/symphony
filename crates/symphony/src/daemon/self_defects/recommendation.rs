use crate::{
    config::ProjectConfig,
    linear::LinearIssue,
    state::{
        FailureRecord, OpenCodeSessionRecord, SelfDefectRecommendationConfidence,
        SelfDefectRecommendationRecord, SelfDefectRecord, SelfDefectRelationMode,
        SelfDefectResolutionState,
    },
    storage::SqliteStore,
};

pub(super) async fn record_ambiguous_self_defect_recommendation(
    project: &ProjectConfig,
    store: &SqliteStore,
    fingerprint: &str,
    message: &str,
    failure: &FailureRecord,
    session: &OpenCodeSessionRecord,
    issue: &LinearIssue,
) -> anyhow::Result<SelfDefectRecord> {
    let recommendation =
        classify_ambiguous_self_defect(project, fingerprint, message, failure, session, issue);
    let recommendation = store
        .record_self_defect_recommendation(&recommendation)
        .await?;

    Ok(SelfDefectRecord {
        registry_id: recommendation.recommendation_id.clone(),
        fingerprint: recommendation.evidence_fingerprint.clone(),
        defect_kind: recommendation.defect_kind.clone(),
        category: recommendation.defect_category.clone(),
        severity: recommendation.confidence.as_str().into(),
        initial_routing_decision: "recommendation_only".into(),
        source_project_id: recommendation.source_project_id.clone(),
        source_issue_id: recommendation.source_issue_id.clone(),
        source_issue_identifier: recommendation.source_issue_identifier.clone(),
        source_session_id: recommendation.source_session_id.clone(),
        source_process_id: recommendation.source_process_id,
        managed_issue_id: String::new(),
        managed_issue_identifier: "recommendation-only".into(),
        occurrence_count: recommendation.occurrence_count,
        first_seen_at: recommendation.first_seen_at.clone(),
        last_seen_at: recommendation.last_seen_at.clone(),
        latest_evidence_summary: recommendation.rationale.clone(),
        resolution_state: SelfDefectResolutionState::Open,
        relation_mode: SelfDefectRelationMode::RelatedOnly,
    })
}

fn classify_ambiguous_self_defect(
    project: &ProjectConfig,
    fingerprint: &str,
    message: &str,
    failure: &FailureRecord,
    session: &OpenCodeSessionRecord,
    issue: &LinearIssue,
) -> SelfDefectRecommendationRecord {
    let evidence_fingerprint = ambiguous_evidence_fingerprint(fingerprint, failure, session, issue);
    let (defect_category, confidence, recommended_action, rationale) =
        ambiguous_recommendation_fields(message, failure);

    SelfDefectRecommendationRecord {
        recommendation_id: format!("recommendation:{evidence_fingerprint}"),
        evidence_fingerprint,
        defect_kind: failure.kind.clone(),
        defect_category: defect_category.into(),
        confidence,
        evidence_refs: vec![
            format!("project:{}", project.id),
            format!("issue:{}", issue.identifier),
            format!("session:{}", session.session_id),
            format!("fingerprint:{fingerprint}"),
        ],
        recommended_action: recommended_action.into(),
        rationale: format!(
            "{rationale}; summary: {message}",
            message = super::bounded_line(message)
        ),
        source_project_id: project.id.clone(),
        source_issue_id: issue.id.clone(),
        source_issue_identifier: issue.identifier.clone(),
        source_session_id: Some(session.session_id.clone()),
        source_process_id: session.process_id,
        occurrence_count: 0,
        first_seen_at: String::new(),
        last_seen_at: String::new(),
    }
}

fn ambiguous_recommendation_fields(
    message: &str,
    failure: &FailureRecord,
) -> (
    &'static str,
    SelfDefectRecommendationConfidence,
    &'static str,
    &'static str,
) {
    let text = format!("{} {}", failure.message, message).to_ascii_lowercase();
    if text.contains("high confidence") || text.contains("reproducible") {
        return (
            super::failure_kind_category(failure),
            SelfDefectRecommendationConfidence::High,
            "operator_review_recommendation",
            "high-confidence ambiguous evidence should persist a typed recommendation without deterministic Linear mutation",
        );
    }
    if text.contains("handoff") || text.contains("sidecar") {
        return (
            "handoff",
            SelfDefectRecommendationConfidence::Medium,
            "backlog_recommendation",
            "ambiguous handoff evidence should be reviewed without deterministic Linear mutation",
        );
    }
    if text.contains("cleanup") || text.contains("worktree") {
        return (
            "cleanup",
            SelfDefectRecommendationConfidence::Medium,
            "backlog_recommendation",
            "ambiguous cleanup evidence should be reviewed without deterministic Linear mutation",
        );
    }
    (
        super::failure_kind_category(failure),
        SelfDefectRecommendationConfidence::Low,
        "backlog_recommendation",
        "low-confidence ambiguous runtime evidence is recommendation-only and non-executable",
    )
}

fn ambiguous_evidence_fingerprint(
    fingerprint: &str,
    failure: &FailureRecord,
    session: &OpenCodeSessionRecord,
    issue: &LinearIssue,
) -> String {
    stable_hash(&format!(
        "kind={};fingerprint={};message={};project={};issue={};session={}",
        failure.kind,
        fingerprint,
        failure.message,
        session.project_id,
        issue.id,
        session.session_id
    ))
}

fn stable_hash(input: &str) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in input.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}
