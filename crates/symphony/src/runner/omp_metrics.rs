use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use serde_json::Value;
use tokio::fs;

use super::{
    RunnerError, RunnerSessionActivity, RunnerSessionTreeActivity, RunnerSessionTreeMetrics,
    RunnerTimelineEvent, RunnerTodoActivity,
};

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

struct OmpFileActivity {
    session: RunnerSessionActivity,
    todos: Vec<RunnerTodoActivity>,
    timeline: Vec<RunnerTimelineEvent>,
}

struct OmpFileAccumulator {
    requested_session_id: String,
    parent_session_id: Option<String>,
    fallback_title: String,
    session_id: Option<String>,
    cwd: Option<String>,
    model: Option<String>,
    created_ms: Option<u64>,
    updated_ms: Option<u64>,
    latest_usage: Option<NormalizedUsage>,
    latest_cost_micros: u64,
    timeline: Vec<RunnerTimelineEvent>,
    tool_event_by_call_id: HashMap<String, usize>,
    latest_todo_snapshot: Vec<RunnerTodoActivity>,
}

impl OmpFileAccumulator {
    fn new(session_id: &str, parent_session_id: Option<&str>, fallback_title: String) -> Self {
        Self {
            requested_session_id: session_id.to_owned(),
            parent_session_id: parent_session_id.map(str::to_owned),
            fallback_title,
            session_id: None,
            cwd: None,
            model: None,
            created_ms: None,
            updated_ms: None,
            latest_usage: None,
            latest_cost_micros: 0,
            timeline: Vec::new(),
            tool_event_by_call_id: HashMap::new(),
            latest_todo_snapshot: Vec::new(),
        }
    }

    fn ingest_record(&mut self, value: &Value, line_number: usize) {
        let record_type = json_string(value, "type").unwrap_or_else(|| "unknown".into());
        let record_id = json_string(value, "id").unwrap_or_else(|| format!("line-{line_number}"));
        let record_timestamp_ms = timestamp_ms(value);
        if let Some(timestamp_ms) = record_timestamp_ms {
            self.created_ms = Some(
                self.created_ms
                    .map_or(timestamp_ms, |created| created.min(timestamp_ms)),
            );
            self.updated_ms = Some(
                self.updated_ms
                    .map_or(timestamp_ms, |updated| updated.max(timestamp_ms)),
            );
        }

        match record_type.as_str() {
            "session" => {
                self.session_id = json_string(value, "id").or_else(|| self.session_id.take());
                self.cwd = json_string(value, "cwd").or_else(|| self.cwd.take());
            }
            "model_change" => {
                self.model = json_string(value, "model").or_else(|| self.model.take());
            }
            "message" => self.ingest_message(value, &record_id, record_timestamp_ms.unwrap_or(0)),
            _ => {}
        }
    }

    fn ingest_message(&mut self, value: &Value, record_id: &str, timestamp_ms: u64) {
        if let Some(message) = value.get("message") {
            self.model = json_string(message, "model").or_else(|| self.model.take());
            if let Some(usage_value) = value.get("usage").or_else(|| message.get("usage")) {
                self.latest_usage = Some(NormalizedUsage::from_value(usage_value));
                self.latest_cost_micros = cost_micros(usage_value.get("cost"));
            }
            if json_string(message, "role").as_deref() == Some("toolResult") {
                self.complete_tool_event(record_id, timestamp_ms, message);
                return;
            }
            self.ingest_message_content(record_id, timestamp_ms, message);
        }
    }

    fn ingest_message_content(&mut self, record_id: &str, timestamp_ms: u64, message: &Value) {
        let Some(parts) = message.get("content").and_then(Value::as_array) else {
            if let Some(text) = message.get("content").and_then(Value::as_str) {
                self.push_message_event(record_id, timestamp_ms, message, text);
            }
            return;
        };

        for (index, part) in parts.iter().enumerate() {
            match json_string(part, "type").as_deref() {
                Some("text") => {
                    if let Some(text) = json_string(part, "text") {
                        self.push_message_event(
                            &format!("{record_id}:text:{index}"),
                            timestamp_ms,
                            message,
                            &text,
                        );
                    }
                }
                Some("toolCall") => {
                    self.push_tool_call_event(
                        &format!("{record_id}:tool:{index}"),
                        timestamp_ms,
                        part,
                    );
                }
                Some("toolResult") => {
                    self.complete_tool_event(
                        &format!("{record_id}:tool-result:{index}"),
                        timestamp_ms,
                        part,
                    );
                }
                _ => {}
            }
        }
    }

    fn push_message_event(
        &mut self,
        part_id: &str,
        timestamp_ms: u64,
        message: &Value,
        text: &str,
    ) {
        let Some(summary) = bounded_summary(text) else {
            return;
        };
        let role = json_string(message, "role").unwrap_or_else(|| "message".into());
        self.timeline.push(RunnerTimelineEvent {
            session_id: self.effective_session_id(),
            part_id: part_id.to_owned(),
            time_created_ms: timestamp_ms,
            time_updated_ms: timestamp_ms,
            kind: "message".into(),
            tool: None,
            status: Some("done".into()),
            title: Some(human_title(&role)),
            summary,
        });
    }

    fn push_tool_call_event(&mut self, part_id: &str, timestamp_ms: u64, part: &Value) {
        let call_id = json_string(part, "id").unwrap_or_else(|| part_id.to_owned());
        let tool = json_string(part, "name")
            .or_else(|| json_string(part, "toolName"))
            .unwrap_or_else(|| "tool".into());
        let summary = tool_call_summary(&tool, part.get("arguments"));
        let index = self.timeline.len();
        self.timeline.push(RunnerTimelineEvent {
            session_id: self.effective_session_id(),
            part_id: call_id.clone(),
            time_created_ms: timestamp_ms,
            time_updated_ms: timestamp_ms,
            kind: "tool".into(),
            tool: Some(tool.clone()),
            status: Some("running".into()),
            title: Some(human_title(&tool)),
            summary,
        });
        self.tool_event_by_call_id.insert(call_id, index);
    }

    fn complete_tool_event(&mut self, part_id: &str, timestamp_ms: u64, part: &Value) {
        let tool = json_string(part, "toolName")
            .or_else(|| json_string(part, "name"))
            .unwrap_or_else(|| "tool".into());
        let summary = tool_result_summary(part)
            .unwrap_or_else(|| format!("{} completed", human_title(&tool)));
        if tool == "todo" {
            if let Some(text) = tool_result_text(part) {
                let todos = todo_snapshot_from_tool_result(
                    &self.effective_session_id(),
                    &text,
                    timestamp_ms,
                );
                if !todos.is_empty() {
                    self.latest_todo_snapshot = todos;
                }
            }
        }
        if let Some(index) = json_string(part, "toolCallId")
            .and_then(|tool_call_id| self.tool_event_by_call_id.get(&tool_call_id).copied())
        {
            let event = &mut self.timeline[index];
            event.status = Some("done".into());
            event.time_updated_ms = timestamp_ms;
            event.summary = summary;
            return;
        }
        self.timeline.push(RunnerTimelineEvent {
            session_id: self.effective_session_id(),
            part_id: part_id.to_owned(),
            time_created_ms: timestamp_ms,
            time_updated_ms: timestamp_ms,
            kind: "tool".into(),
            tool: Some(tool.clone()),
            status: Some("done".into()),
            title: Some(human_title(&tool)),
            summary,
        });
    }

    fn effective_session_id(&self) -> String {
        self.session_id
            .clone()
            .unwrap_or_else(|| self.requested_session_id.clone())
    }

    fn finish(self) -> OmpFileActivity {
        let session_id = self.effective_session_id();
        let is_subagent = self.parent_session_id.is_some();
        let usage = self.latest_usage.unwrap_or_default();
        let created_ms = self.created_ms.unwrap_or(0);
        let updated_ms = self.updated_ms.unwrap_or(created_ms);
        OmpFileActivity {
            session: RunnerSessionActivity {
                session_id: session_id.clone(),
                parent_session_id: self.parent_session_id,
                title: self.fallback_title,
                directory: self.cwd.unwrap_or_default(),
                agent: None,
                model: self.model,
                is_subagent,
                tokens_input: usage.input.unwrap_or(0),
                tokens_output: usage.output.unwrap_or(0),
                tokens_reasoning: usage.reasoning.unwrap_or(0),
                tokens_cache_read: usage.cache_read.unwrap_or(0),
                tokens_cache_write: usage.cache_write.unwrap_or(0),
                cost_micros: self.latest_cost_micros,
                time_created_ms: created_ms,
                time_updated_ms: updated_ms,
            },
            todos: self.latest_todo_snapshot,
            timeline: self.timeline,
        }
    }
}

pub async fn read_omp_session_tree_activity(
    session_id: &str,
    timeline_limit: usize,
) -> Result<Option<RunnerSessionTreeActivity>, RunnerError> {
    let Some(root) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Ok(None);
    };
    read_omp_session_tree_activity_from_root(root.join(OMP_SESSION_DIR), session_id, timeline_limit)
        .await
}

pub async fn read_omp_session_tree_activity_from_root(
    sessions_root: impl AsRef<Path>,
    session_id: &str,
    timeline_limit: usize,
) -> Result<Option<RunnerSessionTreeActivity>, RunnerError> {
    let Some(root_file) = find_omp_root_session_file(sessions_root.as_ref(), session_id).await?
    else {
        return Ok(None);
    };

    let root_activity =
        read_omp_activity_file(&root_file, session_id, None, "OMP root session".into()).await?;
    let root_session_id = root_activity.session.session_id.clone();
    let mut sessions = vec![root_activity.session];
    let mut todos = root_activity.todos;
    let mut timeline = root_activity.timeline;

    if let Some(session_dir_name) = root_file
        .file_stem()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
    {
        let session_dir = root_file.with_file_name(session_dir_name);
        for subagent in read_omp_subagent_activities(&session_dir, &root_session_id).await? {
            todos.extend(subagent.todos);
            timeline.extend(subagent.timeline);
            sessions.push(subagent.session);
        }
    }

    let subagents = sessions
        .iter()
        .filter(|session| session.parent_session_id.is_some())
        .cloned()
        .collect::<Vec<_>>();
    timeline.sort_by(|left, right| {
        right
            .time_updated_ms
            .cmp(&left.time_updated_ms)
            .then_with(|| right.time_created_ms.cmp(&left.time_created_ms))
            .then_with(|| left.part_id.cmp(&right.part_id))
    });
    if timeline.len() > timeline_limit {
        timeline.truncate(timeline_limit);
    }
    let running_tool_count = timeline
        .iter()
        .filter(|event| event.kind == "tool" && event.status.as_deref() == Some("running"))
        .count() as u64;
    let pending_tool_count = timeline
        .iter()
        .filter(|event| event.kind == "tool" && event.status.as_deref() == Some("pending"))
        .count() as u64;
    let last_updated_ms = sessions
        .iter()
        .map(|session| session.time_updated_ms)
        .chain(timeline.iter().map(|event| event.time_updated_ms))
        .chain(todos.iter().map(|todo| todo.time_updated_ms))
        .max();

    Ok(Some(RunnerSessionTreeActivity {
        root_session_id,
        sessions,
        subagents,
        todos,
        timeline,
        running_tool_count,
        pending_tool_count,
        last_updated_ms,
    }))
}

async fn read_omp_subagent_activities(
    session_dir: &Path,
    root_session_id: &str,
) -> Result<Vec<OmpFileActivity>, RunnerError> {
    let mut entries = match fs::read_dir(session_dir).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(RunnerError::Io(error)),
    };
    let mut activities = Vec::new();
    while let Some(entry) = entries.next_entry().await.map_err(RunnerError::Io)? {
        let file_type = entry.file_type().await.map_err(RunnerError::Io)?;
        if !file_type.is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("jsonl") {
            continue;
        }
        let Some(agent_name) = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .filter(|stem| !stem.trim().is_empty())
            .map(str::to_owned)
        else {
            continue;
        };
        activities.push(
            read_omp_activity_file(
                &path,
                &agent_name,
                Some(root_session_id),
                human_title(&agent_name),
            )
            .await?,
        );
    }
    Ok(activities)
}

async fn read_omp_activity_file(
    path: &Path,
    session_id: &str,
    parent_session_id: Option<&str>,
    fallback_title: String,
) -> Result<OmpFileActivity, RunnerError> {
    let content = fs::read_to_string(path).await.map_err(RunnerError::Io)?;
    let mut accumulator = OmpFileAccumulator::new(session_id, parent_session_id, fallback_title);
    for (index, line) in content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let value = serde_json::from_str::<Value>(line).map_err(|error| {
            RunnerError::Archive(format!(
                "parse failed: OMP JSONL {} line {}: {error}",
                path.display(),
                index + 1
            ))
        })?;
        accumulator.ingest_record(&value, index + 1);
    }
    Ok(accumulator.finish())
}

fn json_string(value: &Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
}

fn human_title(value: &str) -> String {
    let mut title = String::new();
    let mut previous_was_separator = true;
    for ch in value.chars() {
        if matches!(ch, '-' | '_' | '/' | '.') {
            if !title.ends_with(' ') {
                title.push(' ');
            }
            previous_was_separator = true;
            continue;
        }
        if ch.is_uppercase() && !previous_was_separator && !title.ends_with(' ') {
            title.push(' ');
        }
        title.push(ch);
        previous_was_separator = false;
    }
    let trimmed = title.trim();
    if trimmed.is_empty() {
        value.to_owned()
    } else {
        trimmed.to_owned()
    }
}

fn bounded_summary(text: &str) -> Option<String> {
    let summary = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?
        .chars()
        .take(180)
        .collect::<String>();
    Some(summary)
}

fn tool_call_summary(tool: &str, arguments: Option<&Value>) -> String {
    let Some(arguments) = arguments else {
        return format!("{} started", human_title(tool));
    };
    if tool == "todo" {
        let op = json_string(arguments, "op").unwrap_or_else(|| "update".into());
        return format!("todo {op}");
    }
    arguments
        .get("i")
        .and_then(Value::as_str)
        .filter(|intent| !intent.trim().is_empty())
        .map(|intent| format!("{}: {intent}", human_title(tool)))
        .unwrap_or_else(|| format!("{} started", human_title(tool)))
}

fn tool_result_summary(part: &Value) -> Option<String> {
    tool_result_text(part).and_then(|text| bounded_summary(&text))
}

fn tool_result_text(part: &Value) -> Option<String> {
    if let Some(text) = part.get("content").and_then(Value::as_str) {
        return Some(text.to_owned());
    }
    part.get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|value| json_string(value, "text"))
        .next()
}

fn todo_snapshot_from_tool_result(
    session_id: &str,
    text: &str,
    time_updated_ms: u64,
) -> Vec<RunnerTodoActivity> {
    let mut todos = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix("- ") else {
            continue;
        };
        let Some((content, status_and_phase)) = rest.rsplit_once(" [") else {
            continue;
        };
        let Some((status, phase_tail)) = status_and_phase.split_once(']') else {
            continue;
        };
        let phase = phase_tail
            .trim()
            .strip_prefix('(')
            .and_then(|value| value.strip_suffix(')'))
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("normal");
        let content = content.trim();
        if content.is_empty() || status.trim().is_empty() {
            continue;
        }
        todos.push(RunnerTodoActivity {
            session_id: session_id.to_owned(),
            content: content.to_owned(),
            status: status.trim().to_owned(),
            priority: phase.to_owned(),
            position: todos.len() as u64 + 1,
            time_updated_ms,
        });
    }
    todos
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
    async fn reads_omp_activity_timeline_tools_todos_and_subagents() {
        let dir = tempfile::tempdir().expect("tempdir");
        let project = write_root_session(
            dir.path(),
            "activity113",
            r#"{"type":"session","id":"activity113","timestamp":"2026-06-22T13:47:14.000Z","cwd":"/workspaces/symphony/SYM-134"}
{"type":"model_change","id":"model-root","timestamp":"2026-06-22T13:47:14.500Z","model":"openai-codex/gpt-5.5"}
{"type":"message","id":"m1","timestamp":"2026-06-22T13:47:15.000Z","message":{"role":"assistant","content":[{"type":"toolCall","id":"call-search","name":"search","arguments":{"i":"Finding placeholder text"}}],"usage":{"input":10,"output":2,"cacheRead":4,"cacheWrite":1,"totalTokens":17}}}
{"type":"message","id":"m2","timestamp":"2026-06-22T13:47:16.000Z","message":{"role":"toolResult","content":[{"type":"text","text":"found placeholders"}],"toolCallId":"call-search","toolName":"search"}}
{"type":"message","id":"m3","timestamp":"2026-06-22T13:47:17.000Z","message":{"role":"assistant","content":[{"type":"toolCall","id":"call-todo","name":"todo","arguments":{"op":"init"}}]}}
{"type":"message","id":"m4","timestamp":"2026-06-22T13:47:18.000Z","message":{"role":"toolResult","toolCallId":"call-todo","toolName":"todo","content":[{"type":"text","text":"Remaining items (2):\n  - Add OMP activity adapter [completed] (Implementation)\n  - Capture live screenshots [in_progress] (Validation)"}]}}"#,
        )
        .await;
        let subdir = project.join("2026-06-22T13-47-14-809Z_activity113");
        fs::create_dir_all(&subdir).await.expect("subdir");
        fs::write(
            subdir.join("FrontendScout.jsonl"),
            r#"{"type":"session","id":"sub-frontend","timestamp":"2026-06-22T13:48:00.000Z","cwd":"/workspaces/symphony/SYM-134"}
{"type":"model_change","id":"model-child","timestamp":"2026-06-22T13:48:01.000Z","model":"openai-codex/gpt-5.5"}
{"type":"message","id":"s1","timestamp":"2026-06-22T13:48:02.000Z","message":{"role":"assistant","content":[{"type":"toolCall","id":"call-read","name":"read","arguments":{"i":"Reading issue inspector"}}],"usage":{"input":20,"output":5,"cacheRead":6,"cacheWrite":0,"totalTokens":31}}}"#,
        )
        .await
        .expect("subagent file");

        let activity = read_omp_session_tree_activity_from_root(dir.path(), "activity113", 20)
            .await
            .expect("activity")
            .expect("found");

        assert_eq!(activity.root_session_id, "activity113");
        assert_eq!(activity.sessions.len(), 2);
        assert_eq!(activity.subagents.len(), 1);
        assert_eq!(activity.subagents[0].session_id, "sub-frontend");
        assert_eq!(
            activity.subagents[0].parent_session_id.as_deref(),
            Some("activity113")
        );
        assert_eq!(
            activity.sessions[0].directory,
            "/workspaces/symphony/SYM-134"
        );
        assert_eq!(activity.sessions[0].tokens_cache_read, 4);
        assert_eq!(activity.todos.len(), 2);
        assert_eq!(activity.todos[0].content, "Add OMP activity adapter");
        assert_eq!(activity.todos[0].status, "completed");
        assert_eq!(activity.todos[1].status, "in_progress");
        assert_eq!(activity.running_tool_count, 1);
        assert_eq!(activity.pending_tool_count, 0);
        assert!(
            activity
                .timeline
                .iter()
                .any(|event| event.tool.as_deref() == Some("search")
                    && event.status.as_deref() == Some("done")
                    && event.summary == "found placeholders")
        );
        assert!(
            activity
                .timeline
                .iter()
                .any(|event| event.tool.as_deref() == Some("read")
                    && event.status.as_deref() == Some("running"))
        );
        assert_eq!(activity.last_updated_ms, Some(1_782_136_082_000));
    }

    #[tokio::test]
    async fn reports_parse_failed_for_invalid_omp_activity_jsonl() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_root_session(dir.path(), "broken113", "{not-json").await;

        let error = read_omp_session_tree_activity_from_root(dir.path(), "broken113", 20)
            .await
            .expect_err("invalid jsonl should fail");

        assert!(error.to_string().contains("parse failed: OMP JSONL"));
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
