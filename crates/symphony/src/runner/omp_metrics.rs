use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde_json::Value;
use tokio::fs;

use super::{RunnerError, RunnerSessionTreeMetrics};

const OMP_SESSION_DIR: &str = ".omp/agent/sessions";

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
    ingest_jsonl_file(&root_file, &mut metrics).await?;

    let Some(session_dir_name) = root_file
        .file_stem()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
    else {
        return Ok(Some(metrics));
    };
    let session_dir = root_file.with_file_name(session_dir_name);
    ingest_subagent_files(&session_dir, &mut metrics).await?;
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
        ingest_jsonl_file(&path, metrics).await?;
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
) -> Result<(), RunnerError> {
    let content = fs::read_to_string(path).await.map_err(RunnerError::Io)?;
    for line in content.lines().filter(|line| !line.trim().is_empty()) {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        ingest_omp_jsonl_record(&value, metrics);
    }
    Ok(())
}

fn ingest_omp_jsonl_record(value: &Value, metrics: &mut RunnerSessionTreeMetrics) {
    if value.get("type").and_then(Value::as_str) != Some("message") {
        return;
    }
    metrics.message_count = metrics.message_count.saturating_add(1);
    metrics.part_count = metrics
        .part_count
        .saturating_add(message_part_count(value).unwrap_or(1));
    if let Some(timestamp_ms) = timestamp_ms(value) {
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
    let Some(usage) = value
        .get("usage")
        .or_else(|| message.and_then(|message| message.get("usage")))
    else {
        return;
    };
    metrics.tokens_input = metrics
        .tokens_input
        .saturating_add(u64_field(usage, "input"));
    metrics.tokens_output = metrics
        .tokens_output
        .saturating_add(u64_field(usage, "output"));
    metrics.tokens_reasoning = metrics
        .tokens_reasoning
        .saturating_add(u64_field(usage, "reasoningTokens"));
    metrics.tokens_cache_read = metrics
        .tokens_cache_read
        .saturating_add(u64_field(usage, "cacheRead"));
    metrics.tokens_cache_write = metrics
        .tokens_cache_write
        .saturating_add(u64_field(usage, "cacheWrite"));
    metrics.tokens_total = metrics
        .tokens_total
        .saturating_add(u64_field(usage, "totalTokens"));
    metrics.cost_micros = metrics
        .cost_micros
        .saturating_add(cost_micros(usage.get("cost")));
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

fn u64_field(value: &Value, field: &str) -> u64 {
    value.get(field).and_then(Value::as_u64).unwrap_or(0)
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
            r#"{"type":"message","timestamp":"2026-06-22T13:48:15.000Z","message":{"role":"assistant","content":[{"type":"text","text":"done"}],"model":"gpt-5.5","usage":{"input":20,"output":5,"cacheRead":6,"totalTokens":31}}}"#,
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
        assert_eq!(metrics.tokens_cache_read, 10);
        assert_eq!(metrics.active_agent.as_deref(), Some("McpSurfaceScout"));
        assert_eq!(metrics.active_model.as_deref(), Some("gpt-5.5"));
        assert!(metrics.last_updated_ms.is_some_and(|value| value > 0));
    }
}
