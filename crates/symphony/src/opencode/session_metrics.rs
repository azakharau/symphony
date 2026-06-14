use crate::state::{OpenCodeSessionRecord, OpenCodeStage};

use super::archive::OpenCodeSessionTreeMetrics;
use super::types::OpenCodeSessionEvent;

pub fn ingest_session_event(session: &mut OpenCodeSessionRecord, event: OpenCodeSessionEvent) {
    if let Some(stage) = event.stage {
        session.stage = stage;
    }
    if let Some(agent) = event.active_agent {
        session.active_agent = Some(agent);
    }
    if let Some(model) = event.active_model {
        session.active_model = Some(model);
    }
    if let Some(eval_stage) = event.eval_stage {
        session.eval_stage = Some(eval_stage);
    }
    if let Some(marker) = event.lifecycle_marker {
        session.lifecycle_marker = Some(marker);
    }
    if let Some(last_event) = event.last_event {
        session.last_event = Some(last_event);
    }

    session.message_count = session.message_count.saturating_add(event.message_delta);
    session.todo_count = session.todo_count.saturating_add(event.todo_delta);
    session.part_count = session.part_count.saturating_add(event.part_delta);
    session.token_count = session.token_count.saturating_add(event.token_delta);
    session.cost_micros = session.cost_micros.saturating_add(event.cost_micros_delta);
    session.subagent_count = session.subagent_count.saturating_add(event.subagent_delta);
}

pub fn apply_session_tree_metrics(
    session: &mut OpenCodeSessionRecord,
    metrics: &OpenCodeSessionTreeMetrics,
) {
    if metrics.message_count > 0 || metrics.part_count > 0 || metrics.todo_count > 0 {
        session.stage = match session.stage {
            OpenCodeStage::Starting | OpenCodeStage::Silent => OpenCodeStage::Running,
            stage => stage,
        };
        session.silence_observed = false;
    }
    session.active_agent = metrics
        .active_agent
        .clone()
        .or_else(|| session.active_agent.clone());
    session.active_model = metrics
        .active_model
        .clone()
        .or_else(|| session.active_model.clone());
    session.message_count = metrics.message_count;
    session.todo_count = metrics.todo_count;
    session.part_count = metrics.part_count;
    session.token_count = metrics.tokens_total;
    session.cost_micros = metrics.cost_micros;
    session.subagent_count = metrics.subagent_count;
    session.lifecycle_marker = Some("opencode_db_activity".into());
    session.last_event = metrics
        .last_updated_ms
        .map(|updated| format!("opencode_db_updated:{updated}"))
        .or_else(|| Some("opencode_db_snapshot".into()));
}

pub fn apply_session_tree_metrics_preserving_marker(
    session: &mut OpenCodeSessionRecord,
    metrics: &OpenCodeSessionTreeMetrics,
    previous_last_event: Option<&str>,
    previous_marker: Option<&str>,
) {
    apply_session_tree_metrics(session, metrics);
    if session.last_event.as_deref() == previous_last_event {
        session.lifecycle_marker = previous_marker.map(ToOwned::to_owned);
    }
}

pub fn mark_session_silence(session: &mut OpenCodeSessionRecord, reason: &str) {
    session.stage = OpenCodeStage::Silent;
    session.silence_observed = true;
    session.last_event = Some(format!("silence:{reason}"));
}
