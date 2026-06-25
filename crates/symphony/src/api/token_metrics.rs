use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::state::RunnerSessionRecord;

const TOKEN_METRICS_STALE_AFTER_MS: u64 = 10 * 60 * 1000;
const PLAUSIBLE_EPOCH_MS: u64 = 1_600_000_000_000;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DashboardTokenMetrics {
    pub accounted_total_token_count: u64,
    pub non_cached_token_count: u64,
    pub cached_token_count: u64,
    pub input_token_count: u64,
    pub output_token_count: u64,
    pub reasoning_token_count: u64,
    pub cache_read_token_count: u64,
    pub cache_write_token_count: u64,
    pub reported_total_token_count: u64,
    pub metrics_status: String,
    pub metrics_source: String,
    pub metrics_freshness: String,
    pub metrics_reason: Option<String>,
}

impl DashboardTokenMetrics {
    pub(super) fn unavailable() -> Self {
        Self {
            accounted_total_token_count: 0,
            non_cached_token_count: 0,
            cached_token_count: 0,
            input_token_count: 0,
            output_token_count: 0,
            reasoning_token_count: 0,
            cache_read_token_count: 0,
            cache_write_token_count: 0,
            reported_total_token_count: 0,
            metrics_status: "unavailable".into(),
            metrics_source: "none".into(),
            metrics_freshness: "unavailable".into(),
            metrics_reason: Some("no token metrics collected".into()),
        }
    }

    pub(super) fn from_session(session: &RunnerSessionRecord) -> Self {
        let cached_token_count = session
            .tokens_cache_read
            .saturating_add(session.tokens_cache_write);
        let non_cached_token_count = session
            .token_count
            .checked_sub(cached_token_count)
            .unwrap_or_else(|| {
                session
                    .tokens_input
                    .saturating_add(session.tokens_output)
                    .saturating_add(session.tokens_reasoning)
            });
        let metrics_status = dashboard_token_metrics_status(
            session.token_usage_status.as_str(),
            session.token_count,
            non_cached_token_count,
            cached_token_count,
            session.tokens_reported_total,
        )
        .to_owned();
        let metrics_source = dashboard_token_metrics_source(session.token_usage_source.as_str());
        let (metrics_freshness, freshness_reason) =
            dashboard_token_metrics_freshness(session, metrics_status.as_str());
        let metrics_reason = dashboard_token_metrics_reason(
            session.token_usage_status.as_str(),
            metrics_status.as_str(),
            metrics_source.as_str(),
        )
        .or(freshness_reason);

        Self {
            accounted_total_token_count: session.token_count,
            non_cached_token_count,
            cached_token_count,
            input_token_count: session.tokens_input,
            output_token_count: session.tokens_output,
            reasoning_token_count: session.tokens_reasoning,
            cache_read_token_count: session.tokens_cache_read,
            cache_write_token_count: session.tokens_cache_write,
            reported_total_token_count: session.tokens_reported_total,
            metrics_status,
            metrics_source,
            metrics_freshness,
            metrics_reason,
        }
    }
}

fn dashboard_token_metrics_status(
    usage_status: &str,
    accounted_total: u64,
    non_cached_total: u64,
    cached_total: u64,
    reported_total: u64,
) -> &'static str {
    match usage_status {
        "available" => "available",
        "missing"
            if accounted_total == 0
                && non_cached_total == 0
                && cached_total == 0
                && reported_total == 0 =>
        {
            "unavailable"
        }
        "missing" | "unknown" | "partial" | "mixed" => "degraded",
        _ if accounted_total > 0
            || non_cached_total > 0
            || cached_total > 0
            || reported_total > 0 =>
        {
            "degraded"
        }
        _ => "unavailable",
    }
}

fn dashboard_token_metrics_source(source: &str) -> String {
    match source {
        "none" => "none".into(),
        "acp_event" => "runtime_event_total".into(),
        "legacy_single_total" => "legacy_total".into(),
        "runner_archive" | "omp_jsonl" => "persisted_split_metrics".into(),
        other => other.into(),
    }
}

fn dashboard_token_metrics_freshness(
    session: &RunnerSessionRecord,
    metrics_status: &str,
) -> (String, Option<String>) {
    if metrics_status == "unavailable" {
        return ("unavailable".into(), None);
    }
    let Some(updated_ms) = session.last_event.as_deref().and_then(metric_update_ms) else {
        return (
            "unknown".into(),
            Some("metrics freshness timestamp unavailable".into()),
        );
    };
    if updated_ms < PLAUSIBLE_EPOCH_MS {
        return ("unknown".into(), None);
    }
    let Ok(now) = SystemTime::now().duration_since(UNIX_EPOCH) else {
        return (
            "unknown".into(),
            Some("system clock unavailable for metrics freshness".into()),
        );
    };
    let now_ms = now.as_millis().min(u128::from(u64::MAX)) as u64;
    if now_ms.saturating_sub(updated_ms) > TOKEN_METRICS_STALE_AFTER_MS {
        return (
            "stale".into(),
            Some("metrics are stale; latest runtime usage update is older than 10 minutes".into()),
        );
    }
    ("fresh".into(), None)
}

fn metric_update_ms(last_event: &str) -> Option<u64> {
    last_event
        .strip_prefix("omp_jsonl_updated:")
        .or_else(|| last_event.strip_prefix("runner_archive_updated:"))
        .and_then(|value| value.parse().ok())
}

fn dashboard_token_metrics_reason(
    usage_status: &str,
    metrics_status: &str,
    metrics_source: &str,
) -> Option<String> {
    match metrics_status {
        "unavailable" => Some(format!(
            "no token metrics collected from {}",
            humanize_metrics_source(metrics_source)
        )),
        "degraded" => Some(match usage_status {
            "partial" => "token usage split is incomplete".into(),
            "mixed" => "some session metrics are unavailable".into(),
            "unknown" => "only aggregate token events are available".into(),
            "missing" => "token usage records are missing".into(),
            other => format!("token metrics are degraded ({other})"),
        }),
        _ => None,
    }
}

fn humanize_metrics_source(source: &str) -> &str {
    match source {
        "none" => "runtime output",
        "runtime_event_total" => "runtime event totals",
        "legacy_total" => "legacy totals",
        "persisted_split_metrics" => "persisted split metrics",
        "multiple" => "multiple sources",
        other => other,
    }
}

pub(super) fn aggregate_token_metrics<'a>(
    metrics: impl Iterator<Item = &'a DashboardTokenMetrics>,
) -> DashboardTokenMetrics {
    let mut total = DashboardTokenMetrics::unavailable();
    let mut count = 0_u64;
    let mut available_count = 0_u64;
    let mut unavailable_count = 0_u64;
    let mut fresh_count = 0_u64;
    let mut stale_count = 0_u64;
    let mut unknown_freshness_count = 0_u64;
    let mut first_reason: Option<&str> = None;
    let mut mixed_reason = false;
    let mut first_source: Option<&str> = None;
    let mut mixed_source = false;

    for item in metrics {
        count = count.saturating_add(1);
        available_count =
            available_count.saturating_add(u64::from(item.metrics_status == "available"));
        unavailable_count =
            unavailable_count.saturating_add(u64::from(item.metrics_status == "unavailable"));
        fresh_count = fresh_count.saturating_add(u64::from(item.metrics_freshness == "fresh"));
        stale_count = stale_count.saturating_add(u64::from(item.metrics_freshness == "stale"));
        unknown_freshness_count =
            unknown_freshness_count.saturating_add(u64::from(item.metrics_freshness == "unknown"));
        total.accounted_total_token_count = total
            .accounted_total_token_count
            .saturating_add(item.accounted_total_token_count);
        total.non_cached_token_count = total
            .non_cached_token_count
            .saturating_add(item.non_cached_token_count);
        total.cached_token_count = total
            .cached_token_count
            .saturating_add(item.cached_token_count);
        total.input_token_count = total
            .input_token_count
            .saturating_add(item.input_token_count);
        total.output_token_count = total
            .output_token_count
            .saturating_add(item.output_token_count);
        total.reasoning_token_count = total
            .reasoning_token_count
            .saturating_add(item.reasoning_token_count);
        total.cache_read_token_count = total
            .cache_read_token_count
            .saturating_add(item.cache_read_token_count);
        total.cache_write_token_count = total
            .cache_write_token_count
            .saturating_add(item.cache_write_token_count);
        total.reported_total_token_count = total
            .reported_total_token_count
            .saturating_add(item.reported_total_token_count);

        match first_source {
            Some(source) if source != item.metrics_source => mixed_source = true,
            Some(_) => {}
            None => first_source = Some(item.metrics_source.as_str()),
        }
        if let Some(reason) = item.metrics_reason.as_deref() {
            match first_reason {
                Some(existing) if existing != reason => mixed_reason = true,
                Some(_) => {}
                None => first_reason = Some(reason),
            }
        }
    }

    total.metrics_status = if count == 0 || unavailable_count == count {
        "unavailable".into()
    } else if available_count == count {
        "available".into()
    } else {
        "degraded".into()
    };
    total.metrics_source = if mixed_source {
        "multiple".into()
    } else {
        first_source.unwrap_or("none").into()
    };
    total.metrics_freshness = if count == 0 || unavailable_count == count {
        "unavailable".into()
    } else if stale_count > 0 {
        "stale".into()
    } else if fresh_count == count {
        "fresh".into()
    } else {
        let _ = unknown_freshness_count;
        "unknown".into()
    };
    total.metrics_reason = if mixed_reason {
        Some("multiple metric states".into())
    } else {
        first_reason.map(str::to_owned)
    };
    if total.metrics_status == "unavailable" && total.metrics_reason.is_none() {
        total.metrics_reason = Some("no token metrics collected".into());
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{LifecycleStage, RunnerStage, RuntimeProviderMode};

    #[test]
    fn dashboard_token_metrics_project_omp_available_degraded_unavailable_and_stale_states() {
        let mut available = test_omp_session();
        available.token_count = 300;
        available.tokens_input = 150;
        available.tokens_output = 30;
        available.tokens_reasoning = 7;
        available.tokens_cache_read = 120;
        available.tokens_reported_total = 300;
        available.token_usage_status = "available".into();
        available.token_usage_source = "omp_jsonl".into();
        available.last_event = Some(format!(
            "omp_jsonl_updated:{}",
            current_epoch_ms().saturating_sub(1_000)
        ));

        let metrics = DashboardTokenMetrics::from_session(&available);
        assert_eq!(metrics.accounted_total_token_count, 300);
        assert_eq!(metrics.cached_token_count, 120);
        assert_eq!(metrics.non_cached_token_count, 180);
        assert_eq!(metrics.reasoning_token_count, 7);
        assert_eq!(metrics.metrics_status, "available");
        assert_eq!(metrics.metrics_source, "persisted_split_metrics");
        assert_eq!(metrics.metrics_freshness, "fresh");
        assert_eq!(metrics.metrics_reason, None);

        let mut degraded = available.clone();
        degraded.token_usage_status = "partial".into();
        degraded.tokens_cache_read = 0;
        degraded.token_count = 300;
        let degraded_metrics = DashboardTokenMetrics::from_session(&degraded);
        assert_eq!(degraded_metrics.metrics_status, "degraded");
        assert_eq!(
            degraded_metrics.metrics_reason.as_deref(),
            Some("token usage split is incomplete")
        );

        let mut unavailable = test_omp_session();
        unavailable.token_usage_status = "missing".into();
        unavailable.token_usage_source = "none".into();
        let unavailable_metrics = DashboardTokenMetrics::from_session(&unavailable);
        assert_eq!(unavailable_metrics.metrics_status, "unavailable");
        assert_eq!(unavailable_metrics.metrics_freshness, "unavailable");
        assert_eq!(
            unavailable_metrics.metrics_reason.as_deref(),
            Some("no token metrics collected from runtime output")
        );

        let mut stale = available;
        stale.last_event = Some(format!(
            "omp_jsonl_updated:{}",
            current_epoch_ms().saturating_sub(TOKEN_METRICS_STALE_AFTER_MS + 1_000)
        ));
        let stale_metrics = DashboardTokenMetrics::from_session(&stale);
        assert_eq!(stale_metrics.metrics_status, "available");
        assert_eq!(stale_metrics.metrics_freshness, "stale");
        assert_eq!(
            stale_metrics.metrics_reason.as_deref(),
            Some("metrics are stale; latest runtime usage update is older than 10 minutes")
        );
    }

    fn current_epoch_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after unix epoch")
            .as_millis()
            .min(u128::from(u64::MAX)) as u64
    }

    fn test_omp_session() -> RunnerSessionRecord {
        RunnerSessionRecord {
            project_id: "symphony".into(),
            issue_id: "issue".into(),
            session_id: "session".into(),
            provider_mode: RuntimeProviderMode::OmpAcp,
            provider_id: Some("omp".into()),
            agent: "build".into(),
            model: Some("gpt-5.5".into()),
            worktree_path: "/tmp/worktree".into(),
            process_id: None,
            lifecycle_stage: LifecycleStage::Running,
            stage: RunnerStage::Running,
            active_agent: None,
            active_model: None,
            message_count: 0,
            todo_count: 0,
            part_count: 0,
            token_count: 0,
            tokens_input: 0,
            tokens_output: 0,
            tokens_reasoning: 0,
            tokens_cache_read: 0,
            tokens_cache_write: 0,
            tokens_reported_total: 0,
            token_usage_status: "missing".into(),
            token_usage_source: "none".into(),
            cost_micros: 0,
            subagent_count: 0,
            eval_stage: None,
            lifecycle_marker: None,
            last_event: None,
            runtime_failure_kind: None,
            acp_frame_count: 0,
            session_evidence_refs: Vec::new(),
            silence_observed: false,
        }
    }
}
