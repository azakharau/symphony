use crate::{
    api::{
        AggregateDashboardResponse, IssueDetailResponse, OpenCodeSessionDetail,
        ProjectDashboardResponse,
    },
    opencode::{
        OpenCodeSessionActivity, OpenCodeSessionTreeActivity, OpenCodeTimelineEvent,
        OpenCodeTodoActivity,
    },
    state::{CleanupStatus, LifecycleStage, OpenCodeStage},
};

const REFRESH_SECONDS: u64 = 30;

pub(super) fn render_aggregate(aggregate: &AggregateDashboardResponse) -> String {
    let mut body = String::new();
    body.push_str(&page_header("Symphony Runtime Dashboard"));
    body.push_str("<main class=\"page\"><section class=\"console-head\"><h1>Operational dashboard</h1><p>Read-only live view of project health, capacity, active work, and cleanup state. Auto-refresh 30s.</p></section>");
    body.push_str("<section class=\"grid cards\">");
    for project in &aggregate.projects {
        body.push_str("<article class=\"card project-card\">");
        body.push_str(&format!(
            "<div class=\"card-top\"><h2><a href=\"/projects/{id}\">{name}</a></h2><span class=\"badge {health_class}\">{health}</span></div>",
            id = attr(&project.project_id),
            name = escape(&project.name),
            health_class = status_class(&project.runner_health),
            health = escape(&project.runner_health),
        ));
        body.push_str("<dl class=\"metrics\">");
        metric(&mut body, "active", project.active_count);
        metric(&mut body, "parked", project.parked_count);
        metric(&mut body, "history", project.terminal_count);
        metric(&mut body, "available", project.capacity.available_sessions);
        body.push_str("</dl>");
        body.push_str(&format!(
            "<p class=\"muted\">capacity {}/{} · cleanup {} · last {}</p>",
            project.capacity.running_sessions,
            project.capacity.max_sessions,
            cleanup_label(project.cleanup_status),
            escape(&project.last_event),
        ));
        if !project.enabled {
            body.push_str("<p class=\"warning\">Project disabled</p>");
        }
        body.push_str("</article>");
    }
    if aggregate.projects.is_empty() {
        body.push_str("<article class=\"card empty\">No projects configured.</article>");
    }
    body.push_str("</section></main>");
    finish_page(body)
}

pub(super) fn render_project(project: &ProjectDashboardResponse) -> String {
    let mut body = String::new();
    body.push_str(&page_header(&format!("{} · Symphony", project.name)));
    body.push_str(&format!(
        "<main class=\"page\"><nav><a href=\"/\">Aggregate</a></nav><section class=\"console-head\"><h1>{}</h1><p>{} · cleanup {} · capacity {}/{}</p></section>",
        escape(&project.name),
        lifecycle_label(project.lifecycle_stage),
        cleanup_label(project.cleanup_status),
        project.capacity.running_sessions,
        project.capacity.max_sessions,
    ));
    issue_table(&mut body, "Active issues", &project.active_issues, true);
    issue_table(&mut body, "Recent history", &project.history_issues, false);
    body.push_str("</main>");
    finish_page(body)
}

pub(super) fn render_issue(issue: &IssueDetailResponse) -> String {
    let mut body = String::new();
    body.push_str(&page_header(&format!("{} · Symphony", issue.identifier)));
    body.push_str(&format!(
        "<main class=\"page\"><nav><a href=\"/\">Aggregate</a> / <a href=\"/projects/{project_id}\">Project</a></nav><section class=\"console-head\"><h1>{identifier}: {title}</h1><p><span class=\"badge {status_class}\">{status}</span> {stage} · cleanup {cleanup}</p></section>",
        project_id = attr(&issue.project_id),
        identifier = escape(&issue.identifier),
        title = escape(&issue.title),
        status_class = status_class(&issue.display_status),
        status = escape(&issue.display_status),
        stage = lifecycle_label(issue.lifecycle_stage),
        cleanup = cleanup_label(issue.cleanup_status),
    ));
    evidence_panel(&mut body, issue);
    session_panels(&mut body, issue);
    eval_panel(&mut body, issue);
    body.push_str("</main>");
    finish_page(body)
}

pub(super) fn render_not_found(path: &str) -> String {
    finish_page(format!(
        "{}<main class=\"page\"><section class=\"card\"><h1>Not found</h1><p>{}</p><p><a href=\"/\">Back to dashboard</a></p></section></main>",
        page_header("Not found · Symphony"),
        escape(path),
    ))
}

fn issue_table(body: &mut String, title: &str, issues: &[IssueDetailResponse], active_first: bool) {
    body.push_str(&format!(
        "<section class=\"card\"><h2>{}</h2>",
        escape(title)
    ));
    if issues.is_empty() {
        body.push_str("<p class=\"muted\">No issues in this lane.</p></section>");
        return;
    }
    body.push_str("<div class=\"issue-list\">");
    let mut ordered = issues.iter().collect::<Vec<_>>();
    if active_first {
        ordered.sort_by_key(|issue| issue_status_rank(issue.lifecycle_stage));
    }
    for issue in ordered {
        body.push_str(&format!(
            "<a class=\"issue-row\" href=\"/projects/{project_id}/issues/{issue_id}\"><strong>{identifier}</strong><span>{title}</span><span class=\"badge {status_class}\">{status}</span><span>{last}</span></a>",
            project_id = attr(&issue.project_id),
            issue_id = attr(&issue.issue_id),
            identifier = escape(&issue.identifier),
            title = escape(&issue.title),
            status_class = status_class(&issue.display_status),
            status = escape(&issue.display_status),
            last = escape(issue.last_runner_event.as_deref().unwrap_or("no runner event")),
        ));
    }
    body.push_str("</div></section>");
}

fn evidence_panel(body: &mut String, issue: &IssueDetailResponse) {
    body.push_str("<section class=\"grid two\"><article class=\"card\"><h2>Issue evidence</h2><dl class=\"facts\">");
    fact(
        body,
        "stop reason",
        issue.stop_reason.as_deref().unwrap_or("none"),
    );
    fact(
        body,
        "last runner event",
        issue.last_runner_event.as_deref().unwrap_or("none"),
    );
    if let Some(blocker) = &issue.blocker {
        fact(
            body,
            "blocker",
            &format!("{}: {}", blocker.kind, blocker.message),
        );
    }
    if let Some(failure) = &issue.failure {
        fact(
            body,
            "failure",
            &format!("{}: {}", failure.kind, failure.message),
        );
    }
    body.push_str(
        "</dl></article><article class=\"card\"><h2>Worktree / git</h2><dl class=\"facts\">",
    );
    if let Some(git) = &issue.git_ref {
        fact(body, "branch", &git.branch);
        fact(body, "worktree", &git.worktree_path);
        fact(body, "head", git.head_sha.as_deref().unwrap_or("none"));
        fact(body, "PR", git.pr_url.as_deref().unwrap_or("none"));
    } else {
        fact(body, "git", "no git ref recorded");
    }
    body.push_str("</dl></article></section>");
}

fn session_panels(body: &mut String, issue: &IssueDetailResponse) {
    body.push_str("<section class=\"card\"><h2>OpenCode sessions</h2>");
    if issue.opencode_sessions.is_empty() {
        body.push_str("<p class=\"muted\">No OpenCode sessions recorded.</p></section>");
        return;
    }
    for session in &issue.opencode_sessions {
        session_panel(body, session);
    }
    body.push_str("</section>");
}

fn session_panel(body: &mut String, session: &OpenCodeSessionDetail) {
    body.push_str(&format!(
        "<article class=\"session\"><h3>{id}</h3><p><span class=\"badge {stage_class}\">{stage}</span> process: <strong>{process_class}</strong> · process_alive: <strong>{alive}</strong> · root session: <strong>{id}</strong></p>",
        id = escape(&session.opencode_session_id),
        stage_class = stage_class(session.current_stage),
        stage = open_code_stage_label(session.current_stage),
        process_class = process_classification(session.process_alive),
        alive = option_bool(session.process_alive),
    ));
    body.push_str("<dl class=\"metrics compact\">");
    metric(body, "messages", session.message_count);
    metric(body, "todos", session.todo_count);
    metric(body, "parts", session.part_count);
    metric(body, "tokens", session.token_count);
    metric(body, "cost µ", session.cost_micros);
    body.push_str("</dl><dl class=\"facts\">");
    fact(body, "agent", &session.agent);
    fact(body, "model", session.model.as_deref().unwrap_or("none"));
    fact(body, "worktree", &trim_middle(&session.worktree_path, 96));
    fact(
        body,
        "active agent",
        session.active_agent.as_deref().unwrap_or("none"),
    );
    fact(
        body,
        "active model",
        session.active_model.as_deref().unwrap_or("none"),
    );
    fact(
        body,
        "eval stage",
        session.eval_stage.as_deref().unwrap_or("none"),
    );
    fact(
        body,
        "lifecycle marker",
        session.lifecycle_marker.as_deref().unwrap_or("none"),
    );
    fact(
        body,
        "last event",
        session.last_event.as_deref().unwrap_or("none"),
    );
    fact(
        body,
        "silence observed",
        bool_label(session.silence_observed),
    );
    fact(body, "stage history", &stage_history(session));
    body.push_str("</dl>");
    if let Some(error) = &session.activity_error {
        body.push_str(&format!(
            "<p class=\"warning\">activity_error: {}</p>",
            escape(error)
        ));
    }
    if let Some(activity) = &session.activity {
        activity_panel(body, activity);
    } else {
        body.push_str("<p class=\"muted\">No tree activity available.</p>");
    }
    body.push_str("</article>");
}

fn activity_panel(body: &mut String, activity: &OpenCodeSessionTreeActivity) {
    body.push_str(&format!(
        "<div class=\"activity\"><h4>Tree activity</h4><p>root {} · running tools {} · pending tools {} · last updated {}</p>",
        escape(&activity.root_session_id),
        activity.running_tool_count,
        activity.pending_tool_count,
        activity
            .last_updated_ms
            .map(|value| value.to_string())
            .unwrap_or_else(|| "none".into())
    ));
    activity_sessions(body, "Root sessions", &activity.sessions);
    activity_sessions(body, "Subagents", &activity.subagents);
    todos(body, &activity.todos);
    timeline(body, &activity.timeline);
    body.push_str("</div>");
}

fn activity_sessions(body: &mut String, title: &str, sessions: &[OpenCodeSessionActivity]) {
    body.push_str(&format!("<h5>{}</h5>", escape(title)));
    if sessions.is_empty() {
        body.push_str("<p class=\"muted\">None.</p>");
        return;
    }
    body.push_str("<ul class=\"dense\">");
    for session in sessions {
        body.push_str(&format!(
            "<li><strong>{}</strong> title: {} · updated: {} · {} {} · {}</li>",
            escape(&session.session_id),
            escape(&session.title),
            session.time_updated_ms,
            escape(session.agent.as_deref().unwrap_or("agent unknown")),
            escape(session.model.as_deref().unwrap_or("model unknown")),
            escape(&trim_middle(&session.directory, 80)),
        ));
    }
    body.push_str("</ul>");
}

fn todos(body: &mut String, todos: &[OpenCodeTodoActivity]) {
    body.push_str("<h5>Todos</h5>");
    if todos.is_empty() {
        body.push_str("<p class=\"muted\">No todos.</p>");
        return;
    }
    body.push_str("<ul class=\"dense\">");
    for todo in todos {
        body.push_str(&format!(
            "<li><span class=\"badge {}\">{}</span> {} · {}</li>",
            status_class(&todo.status),
            escape(&todo.status),
            escape(&todo.priority),
            escape(&todo.content),
        ));
    }
    body.push_str("</ul>");
}

fn timeline(body: &mut String, timeline: &[OpenCodeTimelineEvent]) {
    body.push_str("<h5>Timeline</h5>");
    if timeline.is_empty() {
        body.push_str("<p class=\"muted\">No timeline events.</p>");
        return;
    }
    body.push_str("<ol class=\"dense\">");
    for event in timeline {
        body.push_str(&format!(
            "<li><strong>{}</strong> time: {} · {} {} {}</li>",
            escape(&event.kind),
            event.time_created_ms,
            escape(event.tool.as_deref().unwrap_or("")),
            escape(event.status.as_deref().unwrap_or("")),
            escape(&event.summary),
        ));
    }
    body.push_str("</ol>");
}

fn eval_panel(body: &mut String, issue: &IssueDetailResponse) {
    body.push_str("<section class=\"card\"><h2>Evaluation runs</h2>");
    if issue.eval_results.is_empty() {
        body.push_str("<p class=\"muted\">No eval runs recorded.</p></section>");
        return;
    }
    body.push_str("<ul class=\"dense\">");
    for eval in &issue.eval_results {
        body.push_str(&format!(
            "<li><strong>{}</strong> {} <span class=\"badge {}\">{}</span> {}</li>",
            escape(&eval.run_id),
            escape(&eval.suite),
            status_class(&eval.status),
            escape(&eval.status),
            escape(eval.details_json.as_deref().unwrap_or("")),
        ));
    }
    body.push_str("</ul></section>");
}

fn page_header(title: &str) -> String {
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><meta http-equiv=\"refresh\" content=\"{REFRESH_SECONDS}\"><title>{}</title><style>{}</style></head><body>",
        escape(title),
        CSS,
    )
}

fn finish_page(mut body: String) -> String {
    body.push_str("</body></html>");
    body
}

fn metric<T: std::fmt::Display>(body: &mut String, label: &str, value: T) {
    body.push_str(&format!(
        "<div><dt>{}</dt><dd>{}</dd></div>",
        escape(label),
        escape(&value.to_string())
    ));
}

fn fact(body: &mut String, label: &str, value: &str) {
    body.push_str(&format!(
        "<dt>{}</dt><dd>{}</dd>",
        escape(label),
        escape(value)
    ));
}

pub(super) fn escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

fn attr(value: &str) -> String {
    escape(value)
}

pub(super) fn trim_middle(value: &str, max_chars: usize) -> String {
    let char_count = value.chars().count();
    if char_count <= max_chars || max_chars < 3 {
        return value.to_owned();
    }
    let front = (max_chars - 1) / 2;
    let back = max_chars - 1 - front;
    let prefix = value.chars().take(front).collect::<String>();
    let suffix = value
        .chars()
        .skip(char_count.saturating_sub(back))
        .collect::<String>();
    format!("{prefix}…{suffix}")
}

fn status_class(status: &str) -> &'static str {
    match status {
        "running" | "eval running" | "clean" | "complete" | "completed cleanup" | "done" => "ok",
        "blocked" | "provider/infra blocker" | "owner input" | "repair loop" => "warn",
        "failed" | "activity_error" => "bad",
        "queued" | "idle" => "idle",
        _ => "neutral",
    }
}

fn stage_class(stage: OpenCodeStage) -> &'static str {
    match stage {
        OpenCodeStage::Running | OpenCodeStage::Eval | OpenCodeStage::Completed => "ok",
        OpenCodeStage::Failed | OpenCodeStage::Silent => "bad",
        OpenCodeStage::Starting | OpenCodeStage::Review | OpenCodeStage::Handoff => "neutral",
    }
}

fn issue_status_rank(stage: LifecycleStage) -> u8 {
    match stage {
        LifecycleStage::Running => 0,
        LifecycleStage::Blocked => 1,
        LifecycleStage::Queued => 2,
        LifecycleStage::Failed => 3,
        LifecycleStage::Completed => 4,
    }
}

fn lifecycle_label(stage: LifecycleStage) -> &'static str {
    match stage {
        LifecycleStage::Queued => "queued",
        LifecycleStage::Running => "running",
        LifecycleStage::Blocked => "blocked",
        LifecycleStage::Failed => "failed",
        LifecycleStage::Completed => "completed",
    }
}

fn cleanup_label(status: CleanupStatus) -> &'static str {
    match status {
        CleanupStatus::Clean => "clean",
        CleanupStatus::Pending => "pending",
        CleanupStatus::InProgress => "in progress",
        CleanupStatus::Complete => "complete",
        CleanupStatus::Failed => "failed",
    }
}

fn open_code_stage_label(stage: OpenCodeStage) -> &'static str {
    stage.as_str()
}

fn option_bool(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "true",
        Some(false) => "false",
        None => "unknown",
    }
}

fn process_classification(process_alive: Option<bool>) -> &'static str {
    match process_alive {
        Some(true) => "live",
        Some(false) => "dead",
        None => "unknown",
    }
}

fn bool_label(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}

fn stage_history(session: &OpenCodeSessionDetail) -> String {
    session
        .stage_history
        .iter()
        .map(|stage| open_code_stage_label(*stage))
        .collect::<Vec<_>>()
        .join(" → ")
}

const CSS: &str = r#"
:root{color-scheme:dark;--bg:#0b1020;--panel:#121a2e;--panel2:#17213a;--text:#edf2ff;--muted:#9aa8c7;--line:#263653;--ok:#4ade80;--warn:#facc15;--bad:#fb7185;--idle:#93c5fd}*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--text);font:13px/1.4 ui-monospace,SFMono-Regular,Menlo,Monaco,Consolas,"Liberation Mono",monospace}.page{width:min(1440px,calc(100vw - 24px));margin:0 auto;padding:12px 0 32px}a{color:#bfdbfe;text-decoration:none}a:hover{text-decoration:underline}.console-head,.card{border:1px solid var(--line);background:var(--panel);padding:12px;margin-bottom:12px}h1,h2,h3,h4,h5,p{margin-top:0}.grid{display:grid;gap:12px}.cards{grid-template-columns:repeat(auto-fit,minmax(280px,1fr))}.two{grid-template-columns:repeat(auto-fit,minmax(360px,1fr));margin-bottom:12px}.card-top{display:flex;align-items:start;justify-content:space-between;gap:12px}.metrics{display:grid;grid-template-columns:repeat(4,minmax(0,1fr));gap:8px;margin:10px 0}.metrics.compact{grid-template-columns:repeat(auto-fit,minmax(90px,1fr))}.metrics div{padding:8px;border:1px solid var(--line);background:var(--panel2)}dt{color:var(--muted);font-size:12px;text-transform:uppercase;letter-spacing:.04em}dd{margin:0;font-weight:700;overflow-wrap:anywhere}.facts{display:grid;grid-template-columns:minmax(130px,190px) 1fr;gap:6px 10px}.badge{display:inline-block;border:1px solid var(--line);padding:2px 6px;font-size:12px;font-weight:800;white-space:nowrap}.badge.ok{color:#052e16;background:var(--ok);border-color:var(--ok)}.badge.warn{color:#422006;background:var(--warn);border-color:var(--warn)}.badge.bad{color:#450a0a;background:var(--bad);border-color:var(--bad)}.badge.idle{color:#082f49;background:var(--idle);border-color:var(--idle)}.badge.neutral{color:var(--text);background:#334155}.muted{color:var(--muted)}.warning{color:#fde68a}.issue-list{display:grid;gap:6px}.issue-row{display:grid;grid-template-columns:110px minmax(160px,1fr) auto minmax(180px,1fr);gap:10px;align-items:center;padding:8px 10px;border:1px solid var(--line);background:var(--panel2);overflow-wrap:anywhere}.session{margin:10px 0 0;padding:10px;border:1px solid var(--line);background:var(--panel2)}.activity{margin-top:10px;padding:10px;border-left:3px solid var(--idle);background:#0f172a}.dense{display:grid;gap:5px;padding-left:20px}.empty{text-align:center;color:var(--muted)}nav{margin-bottom:10px}@media(max-width:760px){.console-head,.card-top{display:block}.issue-row{grid-template-columns:1fr}.facts{grid-template-columns:1fr}.metrics{grid-template-columns:repeat(2,minmax(0,1fr))}}
"#;
