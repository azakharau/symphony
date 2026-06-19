use std::path::{Path, PathBuf};

use libsql::{Builder, Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::fs;
use tracing::info;

use crate::opencode::OpenCodeError;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenCodeSessionArchiveRequest {
    pub opencode_database_path: PathBuf,
    pub archive_root: PathBuf,
    pub project_id: String,
    pub issue_id: String,
    pub issue_identifier: String,
    pub root_session_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenCodeSessionArchiveReport {
    pub artifact_root: PathBuf,
    pub sessions_archived: u64,
    pub messages_archived: u64,
    pub parts_archived: u64,
    pub todos_archived: u64,
    pub sessions_deleted: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct OpenCodeSessionTreeMetrics {
    pub root_session_id: String,
    pub session_count: u64,
    pub subagent_count: u64,
    pub message_count: u64,
    pub part_count: u64,
    pub todo_count: u64,
    pub tokens_input: u64,
    pub tokens_output: u64,
    pub tokens_reasoning: u64,
    pub tokens_cache_read: u64,
    pub tokens_cache_write: u64,
    pub tokens_total: u64,
    pub cost_micros: u64,
    pub active_agent: Option<String>,
    pub active_model: Option<String>,
    pub last_updated_ms: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OpenCodeSessionTreeActivity {
    pub root_session_id: String,
    pub sessions: Vec<OpenCodeSessionActivity>,
    pub subagents: Vec<OpenCodeSessionActivity>,
    pub todos: Vec<OpenCodeTodoActivity>,
    pub timeline: Vec<OpenCodeTimelineEvent>,
    pub running_tool_count: u64,
    pub pending_tool_count: u64,
    pub last_updated_ms: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OpenCodeSessionActivity {
    pub session_id: String,
    pub parent_session_id: Option<String>,
    pub title: String,
    pub directory: String,
    pub agent: Option<String>,
    pub model: Option<String>,
    pub is_subagent: bool,
    pub tokens_input: u64,
    pub tokens_output: u64,
    pub tokens_reasoning: u64,
    pub tokens_cache_read: u64,
    pub tokens_cache_write: u64,
    pub cost_micros: u64,
    pub time_created_ms: u64,
    pub time_updated_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OpenCodeTodoActivity {
    pub session_id: String,
    pub content: String,
    pub status: String,
    pub priority: String,
    pub position: u64,
    pub time_updated_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OpenCodeTimelineEvent {
    pub session_id: String,
    pub part_id: String,
    pub time_created_ms: u64,
    pub time_updated_ms: u64,
    pub kind: String,
    pub tool: Option<String>,
    pub status: Option<String>,
    pub title: Option<String>,
    pub summary: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OpenCodeSessionMessageError {
    pub session_id: String,
    pub message_id: String,
    pub name: String,
    pub provider_id: Option<String>,
    pub message: String,
    pub time_updated_ms: u64,
}

#[derive(Clone, Debug, Serialize)]
struct SessionRow {
    id: String,
    parent_id: Option<String>,
    title: String,
    directory: String,
    agent: Option<String>,
    model: Option<String>,
    cost: f64,
    tokens_input: u64,
    tokens_output: u64,
    tokens_reasoning: u64,
    tokens_cache_read: u64,
    tokens_cache_write: u64,
    time_created: u64,
    time_updated: u64,
}

#[derive(Clone, Debug, Serialize)]
struct MessageRow {
    id: String,
    session_id: String,
    time_created: u64,
    time_updated: u64,
    data: Value,
}

#[derive(Clone, Debug, Serialize)]
struct PartRow {
    id: String,
    message_id: String,
    session_id: String,
    time_created: u64,
    time_updated: u64,
    data: Value,
}

#[derive(Clone, Debug, Serialize)]
struct SessionMessageRow {
    id: String,
    session_id: String,
    message_type: String,
    time_created: u64,
    time_updated: u64,
    data: Value,
}

#[derive(Clone, Debug, Serialize)]
struct TodoRow {
    session_id: String,
    content: String,
    status: String,
    priority: String,
    position: u64,
    time_created: u64,
    time_updated: u64,
}

struct ArchivePayload<'a> {
    metrics: &'a OpenCodeSessionTreeMetrics,
    sessions: &'a [SessionRow],
    messages: &'a [MessageRow],
    parts: &'a [PartRow],
    session_messages: &'a [SessionMessageRow],
    todos: &'a [TodoRow],
}

pub async fn read_session_tree_metrics(
    opencode_database_path: impl Into<PathBuf>,
    root_session_id: &str,
) -> Result<Option<OpenCodeSessionTreeMetrics>, OpenCodeError> {
    let conn = open_opencode_database(opencode_database_path.into()).await?;
    let sessions = session_tree(&conn, root_session_id).await?;
    if sessions.is_empty() {
        return Ok(None);
    }

    let message_count = count_rows_by_session(&conn, "message", root_session_id).await?;
    let part_count = count_rows_by_session(&conn, "part", root_session_id).await?;
    let todo_count = count_rows_by_session(&conn, "todo", root_session_id).await?;
    Ok(Some(metrics_from_rows(
        root_session_id,
        &sessions,
        message_count,
        part_count,
        todo_count,
    )))
}

pub async fn read_session_tree_activity(
    opencode_database_path: impl Into<PathBuf>,
    root_session_id: &str,
    timeline_limit: usize,
) -> Result<Option<OpenCodeSessionTreeActivity>, OpenCodeError> {
    let conn = open_opencode_database(opencode_database_path.into()).await?;
    let sessions = session_tree(&conn, root_session_id).await?;
    if sessions.is_empty() {
        return Ok(None);
    }

    let todos = todo_rows(&conn, root_session_id).await?;
    let timeline = timeline_events(&conn, root_session_id, timeline_limit).await?;
    let running_tool_count = timeline
        .iter()
        .filter(|event| event.status.as_deref() == Some("running"))
        .count() as u64;
    let pending_tool_count = timeline
        .iter()
        .filter(|event| event.status.as_deref() == Some("pending"))
        .count() as u64;
    let session_activity = sessions
        .iter()
        .map(session_activity_from_row)
        .collect::<Vec<_>>();
    let subagents = session_activity
        .iter()
        .filter(|session| session.is_subagent)
        .cloned()
        .collect::<Vec<_>>();
    let last_updated_ms = sessions
        .iter()
        .map(|session| session.time_updated)
        .chain(timeline.iter().map(|event| event.time_updated_ms))
        .chain(todos.iter().map(|todo| todo.time_updated))
        .max();

    Ok(Some(OpenCodeSessionTreeActivity {
        root_session_id: root_session_id.into(),
        sessions: session_activity,
        subagents,
        todos: todos.into_iter().map(todo_activity_from_row).collect(),
        timeline,
        running_tool_count,
        pending_tool_count,
        last_updated_ms,
    }))
}

pub async fn read_latest_session_tree_error(
    opencode_database_path: impl Into<PathBuf>,
    root_session_id: &str,
) -> Result<Option<OpenCodeSessionMessageError>, OpenCodeError> {
    let conn = open_opencode_database(opencode_database_path.into()).await?;
    let messages = message_rows(&conn, root_session_id).await?;
    Ok(messages
        .into_iter()
        .filter_map(message_error_from_row)
        .max_by(|left, right| {
            left.time_updated_ms
                .cmp(&right.time_updated_ms)
                .then_with(|| left.message_id.cmp(&right.message_id))
        }))
}

pub async fn archive_and_delete_session_tree(
    request: OpenCodeSessionArchiveRequest,
) -> Result<OpenCodeSessionArchiveReport, OpenCodeError> {
    let conn = open_opencode_database(request.opencode_database_path.clone()).await?;
    let sessions = session_tree(&conn, &request.root_session_id).await?;
    let artifact_root = request
        .archive_root
        .join(safe_path_segment(&request.project_id))
        .join(safe_path_segment(&request.issue_identifier))
        .join(safe_path_segment(&request.root_session_id));
    if sessions.is_empty() {
        return Ok(OpenCodeSessionArchiveReport {
            artifact_root,
            sessions_archived: 0,
            messages_archived: 0,
            parts_archived: 0,
            todos_archived: 0,
            sessions_deleted: 0,
        });
    }

    let session_ids = sessions
        .iter()
        .map(|session| session.id.as_str())
        .collect::<Vec<_>>();
    let messages = message_rows(&conn, &request.root_session_id).await?;
    let parts = part_rows(&conn, &request.root_session_id).await?;
    let session_messages = session_message_rows(&conn, &request.root_session_id).await?;
    let todos = todo_rows(&conn, &request.root_session_id).await?;
    let metrics = metrics_from_rows(
        &request.root_session_id,
        &sessions,
        messages.len() as u64,
        parts.len() as u64,
        todos.len() as u64,
    );

    write_archive(
        &artifact_root,
        &request,
        ArchivePayload {
            metrics: &metrics,
            sessions: &sessions,
            messages: &messages,
            parts: &parts,
            session_messages: &session_messages,
            todos: &todos,
        },
    )
    .await?;

    let sessions_deleted = delete_session_tree(&conn, &session_ids).await?;
    info!(
        root_session_id = %request.root_session_id,
        artifact_root = %artifact_root.display(),
        sessions_deleted,
        "archived and deleted OpenCode session tree"
    );
    Ok(OpenCodeSessionArchiveReport {
        artifact_root,
        sessions_archived: sessions.len() as u64,
        messages_archived: messages.len() as u64,
        parts_archived: parts.len() as u64,
        todos_archived: todos.len() as u64,
        sessions_deleted,
    })
}

async fn open_opencode_database(path: PathBuf) -> Result<Connection, OpenCodeError> {
    let database = Builder::new_local(path.display().to_string())
        .build()
        .await?;
    let conn = database.connect()?;
    conn.execute_batch(
        r#"
        PRAGMA foreign_keys = ON;
        PRAGMA busy_timeout = 5000;
        "#,
    )
    .await?;
    Ok(conn)
}

async fn session_tree(
    conn: &Connection,
    root_session_id: &str,
) -> Result<Vec<SessionRow>, OpenCodeError> {
    let mut rows = conn
        .query(
            r#"
            SELECT id, parent_id, title, directory, agent, model, cost,
                   tokens_input, tokens_output, tokens_reasoning, tokens_cache_read,
                   tokens_cache_write, time_created, time_updated
            FROM session
            WHERE id = ?1 OR parent_id = ?1
            ORDER BY parent_id IS NOT NULL ASC, time_created ASC, id ASC
            "#,
            params![root_session_id],
        )
        .await?;
    let mut sessions = Vec::new();
    while let Some(row) = rows.next().await? {
        sessions.push(SessionRow {
            id: row.get(0)?,
            parent_id: row.get(1)?,
            title: row.get(2)?,
            directory: row.get(3)?,
            agent: row.get(4)?,
            model: row.get(5)?,
            cost: row.get(6)?,
            tokens_input: get_u64(&row, 7)?,
            tokens_output: get_u64(&row, 8)?,
            tokens_reasoning: get_u64(&row, 9)?,
            tokens_cache_read: get_u64(&row, 10)?,
            tokens_cache_write: get_u64(&row, 11)?,
            time_created: get_u64(&row, 12)?,
            time_updated: get_u64(&row, 13)?,
        });
    }
    Ok(sessions)
}

async fn count_rows_by_session(
    conn: &Connection,
    table: &str,
    root_session_id: &str,
) -> Result<u64, OpenCodeError> {
    if !matches!(table, "message" | "part" | "todo") {
        return Err(OpenCodeError::Archive(format!(
            "unsupported OpenCode session table `{table}`"
        )));
    }
    let mut rows = conn
        .query(
            format!(
                r#"
                SELECT count(*)
                FROM {table}
                WHERE session_id IN (
                    SELECT id FROM session WHERE id = ?1 OR parent_id = ?1
                )
                "#
            )
            .as_str(),
            params![root_session_id],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(0);
    };
    get_u64(&row, 0)
}

async fn message_rows(
    conn: &Connection,
    root_session_id: &str,
) -> Result<Vec<MessageRow>, OpenCodeError> {
    let mut values = Vec::new();
    let mut rows = conn
        .query(
            r#"
            SELECT id, session_id, time_created, time_updated, data
            FROM message
            WHERE session_id IN (
                SELECT id FROM session WHERE id = ?1 OR parent_id = ?1
            )
            ORDER BY time_created ASC, session_id ASC, id ASC
            "#,
            params![root_session_id],
        )
        .await?;
    while let Some(row) = rows.next().await? {
        values.push(MessageRow {
            id: row.get(0)?,
            session_id: row.get(1)?,
            time_created: get_u64(&row, 2)?,
            time_updated: get_u64(&row, 3)?,
            data: parse_json_cell(row.get(4)?)?,
        });
    }
    Ok(values)
}

async fn part_rows(
    conn: &Connection,
    root_session_id: &str,
) -> Result<Vec<PartRow>, OpenCodeError> {
    let mut values = Vec::new();
    let mut rows = conn
        .query(
            r#"
            SELECT id, message_id, session_id, time_created, time_updated, data
            FROM part
            WHERE session_id IN (
                SELECT id FROM session WHERE id = ?1 OR parent_id = ?1
            )
            ORDER BY time_created ASC, session_id ASC, id ASC
            "#,
            params![root_session_id],
        )
        .await?;
    while let Some(row) = rows.next().await? {
        values.push(PartRow {
            id: row.get(0)?,
            message_id: row.get(1)?,
            session_id: row.get(2)?,
            time_created: get_u64(&row, 3)?,
            time_updated: get_u64(&row, 4)?,
            data: parse_json_cell(row.get(5)?)?,
        });
    }
    Ok(values)
}

async fn session_message_rows(
    conn: &Connection,
    root_session_id: &str,
) -> Result<Vec<SessionMessageRow>, OpenCodeError> {
    let mut values = Vec::new();
    let mut rows = conn
        .query(
            r#"
            SELECT id, session_id, type, time_created, time_updated, data
            FROM session_message
            WHERE session_id IN (
                SELECT id FROM session WHERE id = ?1 OR parent_id = ?1
            )
            ORDER BY time_created ASC, session_id ASC, id ASC
            "#,
            params![root_session_id],
        )
        .await?;
    while let Some(row) = rows.next().await? {
        values.push(SessionMessageRow {
            id: row.get(0)?,
            session_id: row.get(1)?,
            message_type: row.get(2)?,
            time_created: get_u64(&row, 3)?,
            time_updated: get_u64(&row, 4)?,
            data: parse_json_cell(row.get(5)?)?,
        });
    }
    Ok(values)
}

async fn todo_rows(
    conn: &Connection,
    root_session_id: &str,
) -> Result<Vec<TodoRow>, OpenCodeError> {
    let mut values = Vec::new();
    let mut rows = conn
        .query(
            r#"
            SELECT session_id, content, status, priority, position, time_created, time_updated
            FROM todo
            WHERE session_id IN (
                SELECT id FROM session WHERE id = ?1 OR parent_id = ?1
            )
            ORDER BY session_id ASC, position ASC
            "#,
            params![root_session_id],
        )
        .await?;
    while let Some(row) = rows.next().await? {
        values.push(TodoRow {
            session_id: row.get(0)?,
            content: row.get(1)?,
            status: row.get(2)?,
            priority: row.get(3)?,
            position: get_u64(&row, 4)?,
            time_created: get_u64(&row, 5)?,
            time_updated: get_u64(&row, 6)?,
        });
    }
    Ok(values)
}

async fn timeline_events(
    conn: &Connection,
    root_session_id: &str,
    timeline_limit: usize,
) -> Result<Vec<OpenCodeTimelineEvent>, OpenCodeError> {
    if timeline_limit == 0 {
        return Ok(Vec::new());
    }

    let mut values = Vec::new();
    let mut rows = conn
        .query(
            r#"
            SELECT id, session_id, time_created, time_updated, data
            FROM part
            WHERE session_id IN (
                SELECT id FROM session WHERE id = ?1 OR parent_id = ?1
            )
            ORDER BY time_updated DESC, time_created DESC, session_id ASC, id ASC
            LIMIT ?2
            "#,
            params![root_session_id, timeline_limit as i64],
        )
        .await?;
    while let Some(row) = rows.next().await? {
        let part_id: String = row.get(0)?;
        let session_id: String = row.get(1)?;
        let time_created = get_u64(&row, 2)?;
        let time_updated = get_u64(&row, 3)?;
        let data = parse_json_cell(row.get(4)?)?;
        values.push(timeline_event_from_part(
            part_id,
            session_id,
            time_created,
            time_updated,
            &data,
        ));
    }
    Ok(values)
}

fn metrics_from_rows(
    root_session_id: &str,
    sessions: &[SessionRow],
    message_count: u64,
    part_count: u64,
    todo_count: u64,
) -> OpenCodeSessionTreeMetrics {
    let mut metrics = OpenCodeSessionTreeMetrics {
        root_session_id: root_session_id.into(),
        session_count: sessions.len() as u64,
        subagent_count: sessions
            .iter()
            .filter(|session| session.parent_id.is_some())
            .count() as u64,
        message_count,
        part_count,
        todo_count,
        ..OpenCodeSessionTreeMetrics::default()
    };

    for session in sessions {
        metrics.tokens_input = metrics.tokens_input.saturating_add(session.tokens_input);
        metrics.tokens_output = metrics.tokens_output.saturating_add(session.tokens_output);
        metrics.tokens_reasoning = metrics
            .tokens_reasoning
            .saturating_add(session.tokens_reasoning);
        metrics.tokens_cache_read = metrics
            .tokens_cache_read
            .saturating_add(session.tokens_cache_read);
        metrics.tokens_cache_write = metrics
            .tokens_cache_write
            .saturating_add(session.tokens_cache_write);
        metrics.cost_micros = metrics
            .cost_micros
            .saturating_add((session.cost.max(0.0) * 1_000_000.0).round() as u64);
        if metrics
            .last_updated_ms
            .is_none_or(|last_updated| session.time_updated >= last_updated)
        {
            metrics.last_updated_ms = Some(session.time_updated);
            metrics.active_agent = session.agent.clone();
            metrics.active_model = model_id(session.model.as_deref());
        }
    }
    metrics.tokens_total = metrics
        .tokens_input
        .saturating_add(metrics.tokens_output)
        .saturating_add(metrics.tokens_reasoning)
        .saturating_add(metrics.tokens_cache_read)
        .saturating_add(metrics.tokens_cache_write);
    metrics
}

fn session_activity_from_row(session: &SessionRow) -> OpenCodeSessionActivity {
    OpenCodeSessionActivity {
        session_id: session.id.clone(),
        parent_session_id: session.parent_id.clone(),
        title: session.title.clone(),
        directory: session.directory.clone(),
        agent: session.agent.clone(),
        model: model_id(session.model.as_deref()),
        is_subagent: session.parent_id.is_some(),
        tokens_input: session.tokens_input,
        tokens_output: session.tokens_output,
        tokens_reasoning: session.tokens_reasoning,
        tokens_cache_read: session.tokens_cache_read,
        tokens_cache_write: session.tokens_cache_write,
        cost_micros: (session.cost.max(0.0) * 1_000_000.0).round() as u64,
        time_created_ms: session.time_created,
        time_updated_ms: session.time_updated,
    }
}

fn todo_activity_from_row(todo: TodoRow) -> OpenCodeTodoActivity {
    OpenCodeTodoActivity {
        session_id: todo.session_id,
        content: todo.content,
        status: todo.status,
        priority: todo.priority,
        position: todo.position,
        time_updated_ms: todo.time_updated,
    }
}

fn message_error_from_row(row: MessageRow) -> Option<OpenCodeSessionMessageError> {
    let error = row.data.get("error")?;
    let name = json_string(error, "name")?;
    let data = error.get("data");
    let provider_id = data
        .and_then(|data| json_string(data, "providerID"))
        .or_else(|| data.and_then(|data| json_string(data, "provider_id")));
    let message = data
        .and_then(|data| json_string(data, "message"))
        .or_else(|| json_string(error, "message"))
        .unwrap_or_else(|| name.clone());
    Some(OpenCodeSessionMessageError {
        session_id: row.session_id,
        message_id: row.id,
        name,
        provider_id,
        message,
        time_updated_ms: row.time_updated,
    })
}

fn timeline_event_from_part(
    part_id: String,
    session_id: String,
    time_created_ms: u64,
    time_updated_ms: u64,
    data: &Value,
) -> OpenCodeTimelineEvent {
    let kind = json_string(data, "type").unwrap_or_else(|| "unknown".into());
    let tool = json_string(data, "tool");
    let status = data
        .get("state")
        .and_then(|state| state.get("status"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let title = json_string(data, "title").filter(|title| !title.trim().is_empty());
    let summary = timeline_summary(data, &kind, tool.as_deref(), status.as_deref());

    OpenCodeTimelineEvent {
        session_id,
        part_id,
        time_created_ms,
        time_updated_ms,
        kind,
        tool,
        status,
        title,
        summary,
    }
}

fn timeline_summary(data: &Value, kind: &str, tool: Option<&str>, status: Option<&str>) -> String {
    let summary = match kind {
        "text" | "reasoning" => json_string(data, "text"),
        "tool" => {
            let mut parts = Vec::new();
            if let Some(tool) = tool {
                parts.push(tool.to_owned());
            }
            if let Some(status) = status {
                parts.push(status.to_owned());
            }
            if let Some(title) = json_string(data, "title")
                && !title.trim().is_empty()
            {
                parts.push(title);
            }
            (!parts.is_empty()).then(|| parts.join(" "))
        }
        "patch" => Some("patch applied".into()),
        _ => json_string(data, "title"),
    }
    .unwrap_or_else(|| kind.to_owned());
    truncate_summary(&summary)
}

fn json_string(data: &Value, key: &str) -> Option<String> {
    data.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn truncate_summary(input: &str) -> String {
    const MAX_CHARS: usize = 240;
    let mut chars = input.trim().chars();
    let summary = chars.by_ref().take(MAX_CHARS).collect::<String>();
    if chars.next().is_some() {
        format!("{summary}...")
    } else {
        summary
    }
}

async fn write_archive(
    artifact_root: &Path,
    request: &OpenCodeSessionArchiveRequest,
    payload: ArchivePayload<'_>,
) -> Result<(), OpenCodeError> {
    let raw_root = artifact_root.join("raw");
    fs::create_dir_all(&raw_root).await?;
    let manifest = json!({
        "schema_version": "symphony.opencode_session_archive.v1",
        "run_id": format!("opencode-session-{}", request.root_session_id),
        "artifact_root": artifact_root.display().to_string(),
        "project_id": request.project_id,
        "issue_id": request.issue_id,
        "issue_identifier": request.issue_identifier,
        "source": {
            "metrics_basis": "OpenCode SQLite session tables across root and child sessions",
            "raw_transcripts_retained_locally": true,
            "raw_transcripts_committed": false
        },
        "metrics": payload.metrics,
        "artifact_registry": [
            {"artifact_id": "manifest", "artifact_type": "manifest", "path": artifact_root.join("manifest.json").display().to_string(), "durability": "durable"},
            {"artifact_id": "sessions", "artifact_type": "derived_metrics", "path": artifact_root.join("sessions.json").display().to_string(), "durability": "durable"},
            {"artifact_id": "messages", "artifact_type": "raw_local_export", "path": raw_root.join("messages.json").display().to_string(), "durability": "local"},
            {"artifact_id": "parts", "artifact_type": "raw_local_export", "path": raw_root.join("parts.json").display().to_string(), "durability": "local"},
            {"artifact_id": "session_messages", "artifact_type": "raw_local_export", "path": raw_root.join("session_messages.json").display().to_string(), "durability": "local"},
            {"artifact_id": "todos", "artifact_type": "raw_local_export", "path": raw_root.join("todos.json").display().to_string(), "durability": "local"}
        ]
    });
    write_json(artifact_root.join("manifest.json"), &manifest).await?;
    write_json(artifact_root.join("sessions.json"), payload.sessions).await?;
    write_json(raw_root.join("messages.json"), payload.messages).await?;
    write_json(raw_root.join("parts.json"), payload.parts).await?;
    write_json(
        raw_root.join("session_messages.json"),
        payload.session_messages,
    )
    .await?;
    write_json(raw_root.join("todos.json"), payload.todos).await?;
    Ok(())
}

async fn write_json<T>(path: PathBuf, value: &T) -> Result<(), OpenCodeError>
where
    T: Serialize + Sync + ?Sized,
{
    let body = serde_json::to_vec_pretty(value)?;
    fs::write(path, body).await?;
    Ok(())
}

async fn delete_session_tree(
    conn: &Connection,
    session_ids: &[&str],
) -> Result<u64, OpenCodeError> {
    let mut deleted = 0_u64;
    conn.execute_batch("BEGIN IMMEDIATE").await?;
    for session_id in session_ids {
        for table in ["todo", "part", "message", "session_message"] {
            conn.execute(
                format!("DELETE FROM {table} WHERE session_id = ?1").as_str(),
                params![*session_id],
            )
            .await?;
        }
        conn.execute(
            "DELETE FROM event WHERE aggregate_id = ?1",
            params![*session_id],
        )
        .await?;
        conn.execute(
            "DELETE FROM event_sequence WHERE aggregate_id = ?1",
            params![*session_id],
        )
        .await?;
        deleted = deleted.saturating_add(
            conn.execute("DELETE FROM session WHERE id = ?1", params![*session_id])
                .await?,
        );
    }
    conn.execute_batch("COMMIT").await?;
    Ok(deleted)
}

fn parse_json_cell(input: String) -> Result<Value, OpenCodeError> {
    serde_json::from_str(&input).map_err(OpenCodeError::from)
}

fn model_id(model: Option<&str>) -> Option<String> {
    let model = model?;
    serde_json::from_str::<Value>(model)
        .ok()
        .and_then(|value| value.get("id").and_then(Value::as_str).map(str::to_owned))
        .or_else(|| Some(model.to_owned()))
}

fn safe_path_segment(input: &str) -> String {
    input
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '_'
            }
        })
        .collect()
}

fn get_u64(row: &libsql::Row, index: i32) -> Result<u64, OpenCodeError> {
    let value: i64 = row.get(index)?;
    Ok(value.max(0) as u64)
}
