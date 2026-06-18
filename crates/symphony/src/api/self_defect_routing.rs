use serde::{Deserialize, Serialize};

use crate::{
    state::{
        IssueStateRecord, SelfDefectRecommendationConfidence, SelfDefectRecommendationRecord,
        SelfDefectRecord, SelfDefectRelationMode,
    },
    storage::{SqliteStore, StorageError},
};

use super::IssueDetailResponse;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SelfDefectRouteSummary {
    pub source_issue_id: String,
    pub source_issue_identifier: String,
    pub managed_issue_id: String,
    pub managed_issue_identifier: String,
    pub managed_issue_url: Option<String>,
    pub fingerprint: String,
    pub defect_kind: String,
    pub severity: String,
    pub relation_mode: SelfDefectRelationMode,
    pub occurrence_count: u32,
    pub first_seen_at: String,
    pub last_seen_at: String,
    pub next_action: String,
    pub skipped_blocker_reason: Option<String>,
    pub deadlock_skipped_blocker: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SelfDefectRoutingProjection {
    pub managed_bug: ManagedSelfDefectProjection,
    pub source_context: SelfDefectSourceContext,
    pub fingerprint: String,
    pub severity: String,
    pub defect_kind: String,
    pub category: String,
    pub occurrence_count: u32,
    pub first_seen_at: String,
    pub last_seen_at: String,
    pub relation_mode: SelfDefectRelationMode,
    pub classifier_recommendation: Option<SelfDefectRecommendationProjection>,
    pub next_action: String,
    pub suppression_reason: Option<String>,
    pub skipped_blocker_reason: Option<String>,
    pub deadlock_skipped_blocker: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ManagedSelfDefectProjection {
    pub issue_id: String,
    pub identifier: String,
    pub url: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SelfDefectSourceContext {
    pub project_id: String,
    pub issue_id: String,
    pub issue_identifier: String,
    pub session_id: Option<String>,
    pub process_id: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SelfDefectRecommendationProjection {
    pub recommendation_id: String,
    pub evidence_fingerprint: String,
    pub defect_kind: String,
    pub defect_category: String,
    pub confidence: SelfDefectRecommendationConfidence,
    pub recommended_action: String,
    pub source_project_id: String,
    pub source_issue_id: String,
    pub source_issue_identifier: String,
    pub source_session_id: Option<String>,
    pub source_process_id: Option<u32>,
    pub occurrence_count: u32,
    pub first_seen_at: String,
    pub last_seen_at: String,
}

pub(crate) fn self_defect_route_summaries<'a>(
    issues: impl Iterator<Item = &'a IssueDetailResponse>,
) -> Vec<SelfDefectRouteSummary> {
    issues
        .filter_map(|issue| {
            let routing = issue.self_defect_routing.as_ref()?;
            Some(SelfDefectRouteSummary {
                source_issue_id: issue.issue_id.clone(),
                source_issue_identifier: issue.identifier.clone(),
                managed_issue_id: routing.managed_bug.issue_id.clone(),
                managed_issue_identifier: routing.managed_bug.identifier.clone(),
                managed_issue_url: routing.managed_bug.url.clone(),
                fingerprint: routing.fingerprint.clone(),
                defect_kind: routing.defect_kind.clone(),
                severity: routing.severity.clone(),
                relation_mode: routing.relation_mode,
                occurrence_count: routing.occurrence_count,
                first_seen_at: routing.first_seen_at.clone(),
                last_seen_at: routing.last_seen_at.clone(),
                next_action: routing.next_action.clone(),
                skipped_blocker_reason: routing.skipped_blocker_reason.clone(),
                deadlock_skipped_blocker: routing.deadlock_skipped_blocker,
            })
        })
        .collect()
}

pub(crate) async fn self_defect_routing_projection(
    store: &SqliteStore,
    issue: &IssueStateRecord,
) -> Result<Option<SelfDefectRoutingProjection>, StorageError> {
    let defects = store
        .open_self_defects_for_source_issue(&issue.project_id, &issue.issue_id)
        .await?;
    let recommendations = store
        .open_self_defect_recommendations_for_source_issue(&issue.project_id, &issue.issue_id)
        .await?;
    let recommendation = recommendations.first().map(recommendation_projection);

    if let Some(defect) = defects.first() {
        return Ok(Some(defect_routing_projection(defect, recommendation)));
    }

    Ok(recommendation.map(recommendation_routing_projection))
}

fn defect_routing_projection(
    defect: &SelfDefectRecord,
    recommendation: Option<SelfDefectRecommendationProjection>,
) -> SelfDefectRoutingProjection {
    let skipped_blocker_reason = skipped_blocker_reason(&defect.latest_evidence_summary);
    SelfDefectRoutingProjection {
        managed_bug: ManagedSelfDefectProjection {
            issue_id: defect.managed_issue_id.clone(),
            identifier: defect.managed_issue_identifier.clone(),
            url: managed_issue_url(&defect.managed_issue_identifier),
        },
        source_context: SelfDefectSourceContext {
            project_id: defect.source_project_id.clone(),
            issue_id: defect.source_issue_id.clone(),
            issue_identifier: defect.source_issue_identifier.clone(),
            session_id: defect.source_session_id.clone(),
            process_id: defect.source_process_id,
        },
        fingerprint: defect.fingerprint.clone(),
        severity: defect.severity.clone(),
        defect_kind: defect.defect_kind.clone(),
        category: defect.category.clone(),
        occurrence_count: defect.occurrence_count,
        first_seen_at: defect.first_seen_at.clone(),
        last_seen_at: defect.last_seen_at.clone(),
        relation_mode: defect.relation_mode,
        classifier_recommendation: recommendation,
        next_action: self_defect_next_action(defect.relation_mode).into(),
        suppression_reason: self_defect_suppression_reason(defect.relation_mode).map(str::to_owned),
        deadlock_skipped_blocker: skipped_blocker_reason.as_deref()
            == Some("active_symphony_self_deadlock_prevention"),
        skipped_blocker_reason,
    }
}

fn recommendation_routing_projection(
    recommendation: SelfDefectRecommendationProjection,
) -> SelfDefectRoutingProjection {
    SelfDefectRoutingProjection {
        managed_bug: ManagedSelfDefectProjection {
            issue_id: String::new(),
            identifier: "recommendation-only".into(),
            url: None,
        },
        source_context: SelfDefectSourceContext {
            project_id: recommendation.source_project_id.clone(),
            issue_id: recommendation.source_issue_id.clone(),
            issue_identifier: recommendation.source_issue_identifier.clone(),
            session_id: recommendation.source_session_id.clone(),
            process_id: recommendation.source_process_id,
        },
        fingerprint: recommendation.evidence_fingerprint.clone(),
        severity: recommendation.confidence.as_str().into(),
        defect_kind: recommendation.defect_kind.clone(),
        category: recommendation.defect_category.clone(),
        occurrence_count: recommendation.occurrence_count,
        first_seen_at: recommendation.first_seen_at.clone(),
        last_seen_at: recommendation.last_seen_at.clone(),
        relation_mode: SelfDefectRelationMode::RelatedOnly,
        classifier_recommendation: Some(recommendation),
        next_action: "review_classifier_recommendation".into(),
        suppression_reason: Some("recommendation_only".into()),
        skipped_blocker_reason: None,
        deadlock_skipped_blocker: false,
    }
}

fn recommendation_projection(
    recommendation: &SelfDefectRecommendationRecord,
) -> SelfDefectRecommendationProjection {
    SelfDefectRecommendationProjection {
        recommendation_id: recommendation.recommendation_id.clone(),
        evidence_fingerprint: recommendation.evidence_fingerprint.clone(),
        defect_kind: recommendation.defect_kind.clone(),
        defect_category: recommendation.defect_category.clone(),
        confidence: recommendation.confidence,
        recommended_action: recommendation.recommended_action.clone(),
        source_project_id: recommendation.source_project_id.clone(),
        source_issue_id: recommendation.source_issue_id.clone(),
        source_issue_identifier: recommendation.source_issue_identifier.clone(),
        source_session_id: recommendation.source_session_id.clone(),
        source_process_id: recommendation.source_process_id,
        occurrence_count: recommendation.occurrence_count,
        first_seen_at: recommendation.first_seen_at.clone(),
        last_seen_at: recommendation.last_seen_at.clone(),
    }
}

fn skipped_blocker_reason(summary: &str) -> Option<String> {
    summary
        .lines()
        .find_map(|line| line.trim().strip_prefix("skipped_blocker_reason: "))
        .map(str::to_owned)
}

fn managed_issue_url(identifier: &str) -> Option<String> {
    if identifier.is_empty() || identifier == "recommendation-only" {
        None
    } else {
        Some(format!("https://linear.app/issue/{identifier}"))
    }
}

const fn self_defect_next_action(relation_mode: SelfDefectRelationMode) -> &'static str {
    match relation_mode {
        SelfDefectRelationMode::Blocking => "repair_managed_self_defect",
        SelfDefectRelationMode::RelatedOnly => "monitor_related_self_defect",
    }
}

const fn self_defect_suppression_reason(
    relation_mode: SelfDefectRelationMode,
) -> Option<&'static str> {
    match relation_mode {
        SelfDefectRelationMode::Blocking => Some("source_issue_blocked_by_managed_self_defect"),
        SelfDefectRelationMode::RelatedOnly => None,
    }
}
