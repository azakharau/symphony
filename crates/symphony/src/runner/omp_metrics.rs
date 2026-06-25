use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde_json::Value;
use tokio::fs;

use super::{RunnerError, RunnerSessionTreeMetrics};

const OMP_SESSION_DIR: &str = ".omp/agent/sessions";

#[derive(Default)]
struct UsageStatusTracker {
    file_count: u64,
    available_count: u64,
    partial_count: u64,
    missing_count: u64,
}

impl UsageStatusTracker {
    fn record_file(&mut self, message_count: u64, usage: Option<&NormalizedUsage>) {
        if message_count == 0 {
            return;
        }
        self.file_count = self.file_count.saturating_add(1);
        match usage {
            Some(usage) if usage.is_complete() => {
                self.available_count = self.available_count.saturating_add(1);
            }
            Some(_) => {
                self.partial_count = self.partial_count.saturating_add(1);
            }
            None => {
                self.missing_count = self.missing_count.saturating_add(1);
            }
        }
    }

    fn status(&self) -> &'static str {
        if self.file_count == 0 || self.missing_count == self.file_count {
            "missing"
        } else if self.available_count == self.file_count {
            "available"
        } else if self.partial_count > 0 {
            "partial"
        } else {
            "mixed"
        }
    }
}

#[derive(Clone, Default)]
struct NormalizedUsage {
    input: Option<u64>,
    output: Option<u64>,
    reasoning: Option<u64>,
    cache_read: Option<u64>,
    cache_write: Option<u64>,
    reported_total: Option<u64>,
}

impl NormalizedUsage {
    fn from_value(usage: &Value) -> Self {
        Self {
            input: usage_u64(
                usage,
                &[
                    "input",
                    "inputTokens",
                    "input_tokens",
                    "input-tokens",
                    "promptTokens",
                    "prompt_tokens",
                    "prompt-tokens",
                ],
            ),
            output: usage_u64(
                usage,
                &[
                    "output",
                    "outputTokens",
                    "output_tokens",
                    "output-tokens",
                    "completionTokens",
                    "completion_tokens",
                    "completion-tokens",
                ],
            ),
            reasoning: usage_u64(
                usage,
                &[
                    "reasoningTokens",
                    "reasoning_tokens",
                    "reasoning-tokens",
                    "reasoning",
                ],
            ),
            cache_read: usage_u64(
                usage,
                &[
                    "cacheRead",
                    "cache_read",
                    "cache-read",
                    "cacheReadTokens",
                    "cache_read_tokens",
                    "cache-read-tokens",
                ],
            ),
            cache_write: usage_u64(
                usage,
                &[
                    "cacheWrite",
                    "cache_write",
                    "cache-write",
                    "cacheWriteTokens",
                    "cache_write_tokens",
                    "cache-write-tokens",
                ],
            ),
            reported_total: usage_u64(
                usage,
                &[
                    "totalTokens",
                    "total_tokens",
                    "total-tokens",
                    "total",
                    "reportedTotal",
                    "reported_total",
                    "reported-total",
                    "reportedTotalTokens",
                    "reported_total_tokens",
                    "reported-total-tokens",
                ],
            ),
        }
    }

    fn accounted_total(&self) -> u64 {
        self.reported_total.unwrap_or_else(|| {
            self.input
                .unwrap_or(0)
                .saturating_add(self.output.unwrap_or(0))
                .saturating_add(self.reasoning.unwrap_or(0))
                .saturating_add(self.cache_read.unwrap_or(0))
                .saturating_add(self.cache_write.unwrap_or(0))
        })
    }

    fn is_complete(&self) -> bool {
        self.input.is_some()
            && self.output.is_some()
            && self.cache_read.is_some()
            && self.cache_write.is_some()
            && self.reported_total.is_some()
    }
}

pub async fn read_omp_session_tree_metrics(
    session_id: &str,
) -> Result<Option<RunnerSessionTreeMetrics>, RunnerError> {
    let Some(root) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Ok(None);
    };
    read_omp_session_tree_metrics_from_root(root.join(OMP_SESSION_DIR), session_id).await
}

pub async fn read_omp_session_tree_metrics_from_root(
    sessions_root: impl AsRef<Path>,
    session_id: &str,
) -> Result<Option<RunnerSessionTreeMetrics>, RunnerError> {
    let Some(root_file) = find_omp_root_session_file(sessions_root.as_ref(), session_id).await?
    else {
        return Ok(None);
    };

    let mut metrics = RunnerSessionTreeMetrics {
        root_session_id: session_id.to_owned(),
        session_count: 1,
        active_agent: Some("build".into()),
        ..RunnerSessionTreeMetrics::default()
    };
    let mut usage_status = UsageStatusTracker::default();
    ingest_jsonl_file(&root_file, &mut metrics, &mut usage_status).await?;

    let Some(session_dir_name) = root_file
        .file_stem()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
    else {
        metrics.usage_status = usage_status.status().to_owned();
        return Ok(Some(metrics));
    };
    let session_dir = root_file.with_file_name(session_dir_name);
    ingest_subagent_files(&session_dir, &mut metrics, &mut usage_status).await?;
    metrics.usage_status = usage_status.status().to_owned();
    Ok(Some(metrics))
}

async fn find_omp_root_session_file(
    sessions_root: &Path,
    session_id: &str,
) -> Result<Option<PathBuf>, RunnerError> {
    let mut projects = match fs::read_dir(sessions_root).await {
        Ok(projects) => projects,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(RunnerError::Io(error)),
    };
    let suffix = format!("_{session_id}.jsonl");
    while let Some(project) = projects.next_entry().await.map_err(RunnerError::Io)? {
        let file_type = project.file_type().await.map_err(RunnerError::Io)?;
        if !file_type.is_dir() {
            continue;
        }
        let mut entries = fs::read_dir(project.path())
            .await
            .map_err(RunnerError::Io)?;
        while let Some(entry) = entries.next_entry().await.map_err(RunnerError::Io)? {
            let file_type = entry.file_type().await.map_err(RunnerError::Io)?;
            if !file_type.is_file() {
                continue;
            }
            let matches = entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.ends_with(&suffix));
            if matches {
                return Ok(Some(entry.path()));
            }
        }
    }
    Ok(None)
}

async fn ingest_subagent_files(
    session_dir: &Path,
    metrics: &mut RunnerSessionTreeMetrics,
    usage_status: &mut UsageStatusTracker,
) -> Result<(), RunnerError> {
    let mut entries = match fs::read_dir(session_dir).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(RunnerError::Io(error)),
    };
    let mut newest_subagent = None::<(u64, String)>;
    while let Some(entry) = entries.next_entry().await.map_err(RunnerError::Io)? {
        let file_type = entry.file_type().await.map_err(RunnerError::Io)?;
        if !file_type.is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("jsonl") {
            continue;
        }
        let Some(agent) = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .filter(|stem| !stem.trim().is_empty())
            .map(str::to_owned)
        else {
            continue;
        };
        let before = metrics.last_updated_ms.unwrap_or(0);
        ingest_jsonl_file(&path, metrics, usage_status).await?;
        let after = metrics.last_updated_ms.unwrap_or(before);
        newest_subagent = match newest_subagent {
            Some((previous, _)) if previous > after => newest_subagent,
            _ => Some((after, agent)),
        };
        metrics.subagent_count = metrics.subagent_count.saturating_add(1);
        metrics.session_count = metrics.session_count.saturating_add(1);
    }
    if let Some((_, agent)) = newest_subagent {
        metrics.active_agent = Some(agent);
    }
    Ok(())
}

async fn ingest_jsonl_file(
    path: &Path,
    metrics: &mut RunnerSessionTreeMetrics,
    usage_status: &mut UsageStatusTracker,
) -> Result<(), RunnerError> {
    let content = fs::read_to_string(path).await.map_err(RunnerError::Io)?;
    let mut latest_usage = None::<NormalizedUsage>;
    let mut latest_cost_micros = 0_u64;
    let mut message_count = 0_u64;

    for line in content.lines().filter(|line| !line.trim().is_empty()) {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some((usage, cost_micros)) = ingest_omp_jsonl_record(&value, metrics) {
            latest_usage = Some(usage);
            latest_cost_micros = cost_micros;
        }
        if value.get("type").and_then(Value::as_str) == Some("message") {
            message_count = message_count.saturating_add(1);
        }
    }

    usage_status.record_file(message_count, latest_usage.as_ref());
    if let Some(usage) = latest_usage {
        metrics.tokens_input = metrics
            .tokens_input
            .saturating_add(usage.input.unwrap_or(0));
        metrics.tokens_output = metrics
            .tokens_output
            .saturating_add(usage.output.unwrap_or(0));
        metrics.tokens_reasoning = metrics
            .tokens_reasoning
            .saturating_add(usage.reasoning.unwrap_or(0));
        metrics.tokens_cache_read = metrics
            .tokens_cache_read
            .saturating_add(usage.cache_read.unwrap_or(0));
        metrics.tokens_cache_write = metrics
            .tokens_cache_write
            .saturating_add(usage.cache_write.unwrap_or(0));
        metrics.tokens_reported_total = metrics
            .tokens_reported_total
            .saturating_add(usage.reported_total.unwrap_or(0));
        metrics.tokens_total = metrics.tokens_total.saturating_add(usage.accounted_total());
        metrics.cost_micros = metrics.cost_micros.saturating_add(latest_cost_micros);
    }
    Ok(())
}

fn ingest_omp_jsonl_record(
    value: &Value,
    metrics: &mut RunnerSessionTreeMetrics,
) -> Option<(NormalizedUsage, u64)> {
    if value.get("type").and_then(Value::as_str) != Some("message") {
        return None;
    }
    metrics.message_count = metrics.message_count.saturating_add(1);
    metrics.part_count = metrics
        .part_count
        .saturating_add(message_part_count(value).unwrap_or(1));
    if let Some(timestamp_ms) = timestamp_ms(value) {
        metrics.started_at_ms = Some(
            metrics
                .started_at_ms
                .map_or(timestamp_ms, |started| started.min(timestamp_ms)),
        );
        metrics.last_updated_ms = Some(metrics.last_updated_ms.unwrap_or(0).max(timestamp_ms));
    }
    let message = value.get("message");
    if let Some(model) = value
        .get("model")
        .or_else(|| message.and_then(|message| message.get("model")))
        .and_then(Value::as_str)
    {
        metrics.active_model = Some(model.to_owned());
    }
    let usage_value = value
        .get("usage")
        .or_else(|| message.and_then(|message| message.get("usage")))?;
    Some((
        NormalizedUsage::from_value(usage_value),
        cost_micros(usage_value.get("cost")),
    ))
}

fn message_part_count(value: &Value) -> Option<u64> {
    let content = value.get("message")?.get("content")?;
    if let Some(parts) = content.as_array() {
        return Some(parts.len() as u64);
    }
    Some(1)
}

fn timestamp_ms(value: &Value) -> Option<u64> {
    let timestamp = value.get("timestamp")?.as_str()?;
    let parsed = DateTime::parse_from_rfc3339(timestamp)
        .ok()?
        .with_timezone(&Utc);
    parsed.timestamp_millis().try_into().ok()
}

fn usage_u64(value: &Value, fields: &[&str]) -> Option<u64> {
    fields
        .iter()
        .find_map(|field| normalized_u64(value.get(*field)?))
}

fn normalized_u64(value: &Value) -> Option<u64> {
    if value.as_bool().is_some() {
        return None;
    }
    if let Some(value) = value.as_u64() {
        return Some(value);
    }
    if let Some(value) = value.as_i64() {
        return Some(value.max(0) as u64);
    }
    if let Some(value) = value.as_f64() {
        return value.is_finite().then_some(value.max(0.0) as u64);
    }
    value
        .as_str()
        .and_then(|value| value.parse::<i64>().ok())
        .map(|value| value.max(0) as u64)
}

fn cost_micros(value: Option<&Value>) -> u64 {
    let Some(cost) = value else {
        return 0;
    };
    if let Some(total) = cost.get("total").and_then(Value::as_f64) {
        return (total.max(0.0) * 1_000_000.0).round() as u64;
    }
    cost.as_f64()
        .map(|value| (value.max(0.0) * 1_000_000.0).round() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn write_root_session(
        sessions_root: &std::path::Path,
        session_id: &str,
        content: &str,
    ) -> std::path::PathBuf {
        let project = sessions_root.join("-shared-symphony-home-workspaces-omp-recall-MNE-235");
        fs::create_dir_all(&project).await.expect("project dir");
        let root = project.join(format!("2026-06-22T13-47-14-809Z_{session_id}.jsonl"));
        fs::write(&root, content).await.expect("root file");
        project
    }

    #[tokio::test]
    async fn reads_omp_root_and_subagent_metrics() {
        let dir = tempfile::tempdir().expect("tempdir");
        let project = dir
            .path()
            .join("-shared-symphony-home-workspaces-omp-recall-MNE-235");
        fs::create_dir_all(&project).await.expect("project dir");
        let root = project.join("2026-06-22T13-47-14-809Z_019eef95.jsonl");
        fs::write(
            &root,
            r#"{"type":"message","timestamp":"2026-06-22T13:47:15.000Z","message":{"role":"user","content":[{"type":"text","text":"prompt"}],"model":"gpt-5.5","usage":{"input":10,"output":2,"cacheRead":4,"cacheWrite":1,"totalTokens":17,"reasoningTokens":3,"cost":{"total":0.01}}}}"#,
        )
        .await
        .expect("root file");
        let subdir = project.join("2026-06-22T13-47-14-809Z_019eef95");
        fs::create_dir_all(&subdir).await.expect("subdir");
        fs::write(
            subdir.join("McpSurfaceScout.jsonl"),
            r#"{"type":"message","timestamp":"2026-06-22T13:48:15.000Z","message":{"role":"assistant","content":[{"type":"text","text":"done"}],"model":"gpt-5.5","usage":{"input":20,"output":5,"cacheRead":6,"cacheWrite":0,"totalTokens":31,"reasoningTokens":0}}}"#,
        )
        .await
        .expect("subagent file");

        let metrics = read_omp_session_tree_metrics_from_root(dir.path(), "019eef95")
            .await
            .expect("metrics")
            .expect("found");

        assert_eq!(metrics.session_count, 2);
        assert_eq!(metrics.subagent_count, 1);
        assert_eq!(metrics.message_count, 2);
        assert_eq!(metrics.tokens_total, 48);
        assert_eq!(metrics.tokens_reported_total, 48);
        assert_eq!(metrics.tokens_cache_read, 10);
        assert_eq!(metrics.usage_status, "available");
        assert_eq!(metrics.active_agent.as_deref(), Some("McpSurfaceScout"));
        assert_eq!(metrics.active_model.as_deref(), Some("gpt-5.5"));
        assert_eq!(metrics.started_at_ms, Some(1_782_136_035_000));
        assert_eq!(metrics.last_updated_ms, Some(1_782_136_095_000));
    }

    #[tokio::test]
    async fn uses_latest_omp_usage_snapshot_instead_of_summing_cumulative_records() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_root_session(
            dir.path(),
            "cumulative113",
            r#"{"type":"message","timestamp":"2026-06-22T13:47:15.000Z","message":{"role":"assistant","content":[{"type":"text","text":"first"}],"model":"gpt-5.5","usage":{"input":100,"output":20,"reasoningTokens":5,"cacheRead":80,"cacheWrite":0,"totalTokens":200}}}
{"type":"message","timestamp":"2026-06-22T13:48:15.000Z","message":{"role":"assistant","content":[{"type":"text","text":"second"}],"model":"gpt-5.5","usage":{"input":150,"output":30,"reasoningTokens":7,"cacheRead":120,"cacheWrite":0,"totalTokens":300}}}"#,
        )
        .await;

        let metrics = read_omp_session_tree_metrics_from_root(dir.path(), "cumulative113")
            .await
            .expect("metrics")
            .expect("found");

        assert_eq!(metrics.message_count, 2);
        assert_eq!(metrics.tokens_total, 300);
        assert_eq!(metrics.tokens_input, 150);
        assert_eq!(metrics.tokens_output, 30);
        assert_eq!(metrics.tokens_reasoning, 7);
        assert_eq!(metrics.tokens_cache_read, 120);
        assert_eq!(metrics.usage_status, "available");
    }

    #[tokio::test]
    async fn fallback_total_includes_reasoning_when_omp_omits_reported_total() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_root_session(
            dir.path(),
            "nototal113",
            r#"{"type":"message","timestamp":"2026-06-22T13:47:15.000Z","message":{"role":"assistant","content":[{"type":"text","text":"split only"}],"model":"gpt-5.5","usage":{"input":100,"output":20,"reasoningTokens":5,"cacheRead":80,"cacheWrite":1}}}"#,
        )
        .await;

        let metrics = read_omp_session_tree_metrics_from_root(dir.path(), "nototal113")
            .await
            .expect("metrics")
            .expect("found");

        assert_eq!(metrics.tokens_total, 206);
        assert_eq!(metrics.tokens_reasoning, 5);
        assert_eq!(metrics.tokens_cache_read, 80);
        assert_eq!(metrics.tokens_cache_write, 1);
        assert_eq!(metrics.usage_status, "partial");
    }

    #[tokio::test]
    async fn normalizes_omp_usage_aliases_and_numeric_values_like_recall_benchmark() {
        let dir = tempfile::tempdir().expect("tempdir");
        let project = write_root_session(
            dir.path(),
            "alias113",
            r#"{"type":"message","timestamp":"2026-06-22T13:47:15.000Z","usage":{"inputTokens":"12","output_tokens":4.9,"reasoning":"3","cache-read-tokens":"7","cache_write_tokens":"2","total":"999"},"message":{"role":"assistant","content":[{"type":"text","text":"root"}],"model":"gpt-5.5"}}"#,
        )
        .await;
        let subdir = project.join("2026-06-22T13-47-14-809Z_alias113");
        fs::create_dir_all(&subdir).await.expect("subdir");
        fs::write(
            subdir.join("rust-engineer.jsonl"),
            r#"{"type":"message","timestamp":"2026-06-22T13:48:15.000Z","message":{"role":"assistant","content":[{"type":"text","text":"child"}],"model":"gpt-5.5","usage":{"prompt_tokens":5,"completionTokens":"6","reasoning_tokens":1,"cache_read":8,"cacheWriteTokens":0,"reported_total_tokens":"123"}}}"#,
        )
        .await
        .expect("subagent file");

        let metrics = read_omp_session_tree_metrics_from_root(dir.path(), "alias113")
            .await
            .expect("metrics")
            .expect("found");

        assert_eq!(metrics.tokens_input, 17);
        assert_eq!(metrics.tokens_output, 10);
        assert_eq!(metrics.tokens_reasoning, 4);
        assert_eq!(metrics.tokens_cache_read, 15);
        assert_eq!(metrics.tokens_cache_write, 2);
        assert_eq!(metrics.tokens_reported_total, 1122);
        assert_eq!(metrics.tokens_total, 1122);
        assert_eq!(metrics.subagent_count, 1);
        assert_eq!(metrics.usage_status, "available");
    }

    #[tokio::test]
    async fn reports_missing_partial_and_mixed_usage_statuses() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_root_session(
            dir.path(),
            "missing113",
            r#"{"type":"message","timestamp":"2026-06-22T13:47:15.000Z","message":{"role":"assistant","content":[{"type":"text","text":"no usage"}],"model":"gpt-5.5"}}"#,
        )
        .await;
        let missing = read_omp_session_tree_metrics_from_root(dir.path(), "missing113")
            .await
            .expect("metrics")
            .expect("found");
        assert_eq!(missing.usage_status, "missing");
        assert_eq!(missing.tokens_total, 0);

        write_root_session(
            dir.path(),
            "partial113",
            r#"{"type":"message","timestamp":"2026-06-22T13:47:15.000Z","message":{"role":"assistant","content":[{"type":"text","text":"reported only"}],"model":"gpt-5.5","usage":{"totalTokens":42}}}"#,
        )
        .await;
        let partial = read_omp_session_tree_metrics_from_root(dir.path(), "partial113")
            .await
            .expect("metrics")
            .expect("found");
        assert_eq!(partial.usage_status, "partial");
        assert_eq!(partial.tokens_reported_total, 42);
        assert_eq!(partial.tokens_total, 42);

        write_root_session(
            dir.path(),
            "mixed113",
            r#"{"type":"message","timestamp":"2026-06-22T13:47:15.000Z","message":{"role":"assistant","content":[{"type":"text","text":"with usage"}],"model":"gpt-5.5","usage":{"input":1,"output":2,"reasoningTokens":3,"cacheRead":4,"cacheWrite":5,"totalTokens":99}}}
{"type":"message","timestamp":"2026-06-22T13:48:15.000Z","message":{"role":"assistant","content":[{"type":"text","text":"missing usage"}],"model":"gpt-5.5"}}"#,
        )
        .await;
        let mixed = read_omp_session_tree_metrics_from_root(dir.path(), "mixed113")
            .await
            .expect("metrics")
            .expect("found");
        assert_eq!(mixed.usage_status, "available");
        assert_eq!(mixed.message_count, 2);
        assert_eq!(mixed.tokens_total, 99);

        let subdir = dir
            .path()
            .join("-shared-symphony-home-workspaces-omp-recall-MNE-235")
            .join("2026-06-22T13-47-14-809Z_mixed113");
        fs::create_dir_all(&subdir).await.expect("subdir");
        fs::write(
            subdir.join("missing-agent.jsonl"),
            r#"{"type":"message","timestamp":"2026-06-22T13:49:15.000Z","message":{"role":"assistant","content":[{"type":"text","text":"missing child usage"}],"model":"gpt-5.5"}}"#,
        )
        .await
        .expect("subagent file");
        let mixed_with_missing_file =
            read_omp_session_tree_metrics_from_root(dir.path(), "mixed113")
                .await
                .expect("metrics")
                .expect("found");
        assert_eq!(mixed_with_missing_file.usage_status, "mixed");
    }
}
