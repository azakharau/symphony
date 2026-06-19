use crate::{
    api::{
        AggregateDashboardResponse, IssueDetailResponse, OpenCodeSessionDetail,
        ProjectDashboardCard, ProjectDashboardResponse, RunningIssueSummary,
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
    body.push_str(&page_header("Symphony Operations"));
    body.push_str("<main class=\"page\"><section class=\"hero\"><div><p class=\"eyebrow\">Symphony operations</p><h1>Running work, tokens, and blockers</h1></div><div class=\"refresh\">Live refresh · 30s</div></section>");
    body.push_str("<section class=\"stat-grid\">");
    stat_card(
        &mut body,
        "Running",
        &aggregate.totals.running_issue_count.to_string(),
        &format!(
            "{} of {} slots available",
            aggregate.totals.available_sessions, aggregate.totals.max_sessions
        ),
    );
    stat_card(
        &mut body,
        "Active tokens",
        &format_tokens(aggregate.totals.running_tokens),
        "current OpenCode sessions",
    );
    stat_card(
        &mut body,
        "Recorded tokens",
        &format_tokens(aggregate.totals.recorded_tokens),
        "runtime history in SQLite",
    );
    stat_card(
        &mut body,
        "OpenCode cost",
        &format_cost_micros(aggregate.totals.recorded_cost_micros),
        "reported by OpenCode telemetry",
    );
    stat_card(
        &mut body,
        "Quota remaining",
        "not reported",
        "provider subscription data is not exposed",
    );
    body.push_str("</section>");
    aggregate_running_now(&mut body, aggregate);
    body.push_str("<section class=\"section-head\"><h2>Projects</h2><p>Execution state by configured project.</p></section><section class=\"project-grid\">");
    for project in &aggregate.projects {
        body.push_str("<article class=\"project-card\">");
        body.push_str(&format!(
            "<div class=\"card-top\"><h3><a href=\"/projects/{id}\">{name}</a></h3><span class=\"badge {health_class}\">{health}</span></div>",
            id = attr(&project.project_id),
            name = escape(&project.name),
            health_class = status_class(&project.runner_health),
            health = escape(&project.runner_health),
        ));
        body.push_str(&format!(
            "<p class=\"project-line\">capacity <strong>{}/{}</strong> · tokens <strong>{}</strong> · cleanup <strong>{}</strong></p>",
            project.capacity.running_sessions,
            project.capacity.max_sessions,
            format_tokens(project.recorded_tokens),
            cleanup_label(project.cleanup_status),
        ));
        project_running_preview(&mut body, project);
        reason_strip(&mut body, project);
        if !project.self_defect_routes.is_empty() {
            body.push_str("<ul class=\"compact-list\">");
            for route in &project.self_defect_routes {
                body.push_str(&format!(
                    "<li><strong>{}</strong> {} {} · count {} · {}</li>",
                    escape(&route.managed_issue_identifier),
                    escape(route.relation_mode.as_str()),
                    escape(&route.severity),
                    route.occurrence_count,
                    escape(&route.next_action),
                ));
            }
            body.push_str("</ul>");
        }
        if !project.enabled {
            body.push_str("<p class=\"warning\">Project disabled</p>");
        }
        body.push_str("</article>");
    }
    if aggregate.projects.is_empty() {
        body.push_str("<article class=\"project-card empty\">No projects configured.</article>");
    }
    body.push_str("</section></main>");
    finish_page(body)
}

pub(super) fn render_project(project: &ProjectDashboardResponse) -> String {
    let mut body = String::new();
    body.push_str(&page_header(&format!("{} · Symphony", project.name)));
    body.push_str(&format!(
        "<main class=\"page\"><nav><a href=\"/\">Dashboard</a></nav><section class=\"hero project-hero\"><div><p class=\"eyebrow\">Project drilldown</p><h1>{}</h1></div><span class=\"badge {}\">{}</span></section>",
        escape(&project.name),
        status_class(&project_response_health(project)),
        escape(&project_response_health(project)),
    ));
    body.push_str("<section class=\"stat-grid\">");
    stat_card(
        &mut body,
        "Running",
        &project.capacity.running_sessions.to_string(),
        &format!("{} slots total", project.capacity.max_sessions),
    );
    stat_card(
        &mut body,
        "Active tokens",
        &format_tokens(
            project
                .active_issues
                .iter()
                .filter(|issue| issue.lifecycle_stage == LifecycleStage::Running)
                .flat_map(|issue| issue.opencode_sessions.iter())
                .map(|session| session.token_count)
                .sum(),
        ),
        "current sessions",
    );
    stat_card(
        &mut body,
        "Cleanup",
        cleanup_label(project.cleanup_status),
        lifecycle_label(project.lifecycle_stage),
    );
    stat_card(
        &mut body,
        "Primary reason",
        &project.liveness.primary_reason_code,
        &project.liveness.primary_reason_detail,
    );
    body.push_str("</section>");
    project_current_execution(&mut body, project);
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

fn aggregate_running_now(body: &mut String, aggregate: &AggregateDashboardResponse) {
    let running = aggregate
        .projects
        .iter()
        .flat_map(|project| {
            project
                .running_issues
                .iter()
                .map(move |issue| (project, issue))
        })
        .collect::<Vec<_>>();

    body.push_str("<section class=\"section-head\"><h2>Running now</h2><p>Live OpenCode execution across all projects.</p></section>");
    if running.is_empty() {
        body.push_str("<section class=\"empty-state\"><h3>No active OpenCode session</h3><p>No project is currently consuming an execution slot. Project cards below show the current blocker or queue reason.</p></section>");
        return;
    }

    body.push_str("<section class=\"running-grid\">");
    for (project, issue) in running {
        running_issue_card(body, Some(project), issue);
    }
    body.push_str("</section>");
}

fn project_current_execution(body: &mut String, project: &ProjectDashboardResponse) {
    body.push_str("<section class=\"section-head\"><h2>Current execution</h2><p>OpenCode runner telemetry for this project.</p></section>");
    let running = project
        .active_issues
        .iter()
        .filter(|issue| issue.lifecycle_stage == LifecycleStage::Running)
        .map(|issue| project_issue_summary(project, issue))
        .collect::<Vec<_>>();

    if running.is_empty() {
        body.push_str(&format!(
            "<section class=\"empty-state\"><h3>No running task</h3><p><strong>{}</strong> · {}</p></section>",
            escape(&project.liveness.primary_reason_code),
            escape(&project.liveness.primary_reason_detail),
        ));
        return;
    }

    body.push_str("<section class=\"running-grid\">");
    for issue in &running {
        running_issue_card(body, None, issue);
    }
    body.push_str("</section>");
}

fn running_issue_card(
    body: &mut String,
    project: Option<&ProjectDashboardCard>,
    issue: &RunningIssueSummary,
) {
    body.push_str("<article class=\"running-card\">");
    body.push_str("<div class=\"running-title\"><div>");
    if let Some(project) = project {
        body.push_str(&format!(
            "<p class=\"eyebrow\">{}</p>",
            escape(&project.name)
        ));
    }
    body.push_str(&format!(
        "<h3><a href=\"/projects/{project_id}/issues/{issue_id}\">{identifier}: {title}</a></h3>",
        project_id = attr(&issue.project_id),
        issue_id = attr(&issue.issue_id),
        identifier = escape(&issue.identifier),
        title = escape(&issue.title),
    ));
    body.push_str("</div>");
    body.push_str(&format!(
        "<span class=\"badge {}\">{}</span>",
        status_class(&issue.display_status),
        escape(&issue.display_status),
    ));
    body.push_str("</div><dl class=\"metrics focused\">");
    metric(body, "tokens", format_tokens(issue.token_count));
    metric(body, "cost", format_cost_micros(issue.cost_micros));
    metric(body, "subagents", issue.subagents_used);
    metric(
        body,
        "tools",
        format!("{} running", issue.running_tool_count),
    );
    body.push_str("</dl><dl class=\"facts compact-facts\">");
    fact(
        body,
        "session",
        issue.session_id.as_deref().unwrap_or("not attached"),
    );
    fact(
        body,
        "stage",
        issue.stage.map(open_code_stage_label).unwrap_or("unknown"),
    );
    fact(
        body,
        "process",
        &format!(
            "{} · {}",
            issue
                .process_id
                .map(|process_id| process_id.to_string())
                .unwrap_or_else(|| "no pid".into()),
            process_classification(issue.process_alive)
        ),
    );
    fact(
        body,
        "agent/model",
        &format!(
            "{} / {}",
            issue
                .active_agent
                .as_deref()
                .or(issue.agent.as_deref())
                .unwrap_or("unknown"),
            issue
                .active_model
                .as_deref()
                .or(issue.model.as_deref())
                .unwrap_or("unknown")
        ),
    );
    fact(
        body,
        "last event",
        issue.last_event.as_deref().unwrap_or("none"),
    );
    fact(
        body,
        "worktree",
        &trim_middle(issue.worktree_path.as_deref().unwrap_or("none"), 96),
    );
    body.push_str("</dl></article>");
}

fn project_running_preview(body: &mut String, project: &ProjectDashboardCard) {
    if project.running_issues.is_empty() {
        body.push_str("<p class=\"project-current\">No active task</p>");
        return;
    }

    for issue in &project.running_issues {
        body.push_str(&format!(
            "<p class=\"project-current\"><strong>{}</strong> · {} · {} tokens · {}</p>",
            escape(&issue.identifier),
            escape(&issue.display_status),
            format_tokens(issue.token_count),
            issue.stage.map(open_code_stage_label).unwrap_or("unknown"),
        ));
    }
}

fn reason_strip(body: &mut String, project: &ProjectDashboardCard) {
    body.push_str(&format!(
        "<p class=\"reason\"><span>{}</span>{}</p>",
        escape(&project.liveness.primary_reason_code),
        escape(&project.liveness.primary_reason_detail),
    ));
}

fn project_issue_summary(
    project: &ProjectDashboardResponse,
    issue: &IssueDetailResponse,
) -> RunningIssueSummary {
    let session = issue.opencode_sessions.last();
    let activity = session.and_then(|session| session.activity.as_ref());

    RunningIssueSummary {
        project_id: project.project_id.clone(),
        project_name: project.name.clone(),
        issue_id: issue.issue_id.clone(),
        identifier: issue.identifier.clone(),
        title: issue.title.clone(),
        display_status: issue.display_status.clone(),
        session_id: session.map(|session| session.opencode_session_id.clone()),
        process_id: session.and_then(|session| session.process_id),
        process_alive: session.and_then(|session| session.process_alive),
        stage: session.map(|session| session.current_stage),
        agent: session.map(|session| session.agent.clone()),
        model: session.and_then(|session| session.model.clone()),
        active_agent: session.and_then(|session| session.active_agent.clone()),
        active_model: session.and_then(|session| session.active_model.clone()),
        token_count: session.map_or(0, |session| session.token_count),
        cost_micros: session.map_or(0, |session| session.cost_micros),
        subagents_used: session.map_or(0, |session| session.subagents_used),
        running_tool_count: activity.map_or(0, |activity| activity.running_tool_count),
        pending_tool_count: activity.map_or(0, |activity| activity.pending_tool_count),
        todo_count: session.map_or(0, |session| session.todo_count),
        last_event: session
            .and_then(|session| session.last_event.clone())
            .or_else(|| issue.last_runner_event.clone()),
        worktree_path: session.map(|session| session.worktree_path.clone()),
    }
}

fn project_response_health(project: &ProjectDashboardResponse) -> String {
    if project
        .active_issues
        .iter()
        .any(|issue| issue.display_status == "repair loop")
    {
        return "repair loop".into();
    }
    if project
        .active_issues
        .iter()
        .any(|issue| issue.display_status == "provider/infra blocker")
    {
        return "provider/infra blocker".into();
    }
    if project
        .active_issues
        .iter()
        .any(|issue| issue.display_status == "eval running")
    {
        return "eval running".into();
    }
    if project
        .active_issues
        .iter()
        .any(|issue| issue.lifecycle_stage == LifecycleStage::Running)
    {
        "active".into()
    } else if project.active_issues.is_empty() {
        "idle".into()
    } else {
        "parked".into()
    }
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
        let last = issue.self_defect_routing.as_ref().map_or_else(
            || {
                issue
                    .last_runner_event
                    .as_deref()
                    .unwrap_or("no runner event")
                    .to_string()
            },
            |routing| {
                format!(
                    "self-defect {} {} count {}",
                    routing.managed_bug.identifier,
                    routing.relation_mode.as_str(),
                    routing.occurrence_count
                )
            },
        );
        body.push_str(&format!(
            "<a class=\"issue-row\" href=\"/projects/{project_id}/issues/{issue_id}\"><strong>{identifier}</strong><span>{title}</span><span class=\"badge {status_class}\">{status}</span><span>{last}</span></a>",
            project_id = attr(&issue.project_id),
            issue_id = attr(&issue.issue_id),
            identifier = escape(&issue.identifier),
            title = escape(&issue.title),
            status_class = status_class(&issue.display_status),
            status = escape(&issue.display_status),
            last = escape(&last),
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
    if let Some(defect) = &issue.runtime_defect {
        fact(body, "runtime defect", &defect.classification);
        fact(
            body,
            "runtime fingerprint",
            defect.fingerprint.as_deref().unwrap_or("none"),
        );
        fact(
            body,
            "repair attempts",
            &defect.repair_attempt_count.to_string(),
        );
        fact(body, "next action", &defect.next_action);
    }
    if let Some(routing) = &issue.self_defect_routing {
        fact(body, "managed self bug", &routing.managed_bug.identifier);
        fact(body, "managed self bug id", &routing.managed_bug.issue_id);
        fact(
            body,
            "managed self bug url",
            routing.managed_bug.url.as_deref().unwrap_or("none"),
        );
        fact(
            body,
            "source context",
            &format!(
                "{} {} session {} process {}",
                routing.source_context.project_id,
                routing.source_context.issue_identifier,
                routing
                    .source_context
                    .session_id
                    .as_deref()
                    .unwrap_or("none"),
                routing
                    .source_context
                    .process_id
                    .map(|id| id.to_string())
                    .unwrap_or_else(|| "none".into())
            ),
        );
        fact(body, "self-defect fingerprint", &routing.fingerprint);
        fact(body, "self-defect kind", &routing.defect_kind);
        fact(body, "self-defect severity", &routing.severity);
        fact(body, "self-defect relation", routing.relation_mode.as_str());
        fact(
            body,
            "self-defect occurrences",
            &routing.occurrence_count.to_string(),
        );
        fact(body, "self-defect first seen", &routing.first_seen_at);
        fact(body, "self-defect last seen", &routing.last_seen_at);
        fact(body, "self-defect next action", &routing.next_action);
        fact(
            body,
            "self-defect suppression",
            routing.suppression_reason.as_deref().unwrap_or("none"),
        );
        fact(
            body,
            "deadlock skipped blocker",
            bool_label(routing.deadlock_skipped_blocker),
        );
        if let Some(recommendation) = &routing.classifier_recommendation {
            fact(
                body,
                "classifier recommendation",
                &recommendation.recommended_action,
            );
            fact(
                body,
                "classifier confidence",
                recommendation.confidence.as_str(),
            );
        }
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

fn stat_card(body: &mut String, label: &str, value: &str, detail: &str) {
    body.push_str(&format!(
        "<article class=\"stat-card\"><span>{}</span><strong>{}</strong><small>{}</small></article>",
        escape(label),
        escape(value),
        escape(detail),
    ));
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

fn format_tokens(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}k", tokens as f64 / 1_000.0)
    } else {
        tokens.to_string()
    }
}

fn format_cost_micros(cost_micros: u64) -> String {
    if cost_micros == 0 {
        return "$0.00".into();
    }
    format!("${:.4}", cost_micros as f64 / 1_000_000.0)
}

fn status_class(status: &str) -> &'static str {
    match status {
        "running" | "eval running" | "clean" | "complete" | "completed cleanup" | "done" => "ok",
        "blocked" | "provider/infra blocker" | "owner input" | "repair loop" | "runtime repair" => {
            "warn"
        }
        "failed" | "activity_error" | "runtime defect" => "bad",
        "queued" | "idle" => "idle",
        _ => "neutral",
    }
}

const fn stage_class(stage: OpenCodeStage) -> &'static str {
    match stage {
        OpenCodeStage::Running | OpenCodeStage::Eval | OpenCodeStage::Completed => "ok",
        OpenCodeStage::Failed | OpenCodeStage::Silent => "bad",
        OpenCodeStage::Starting | OpenCodeStage::Review | OpenCodeStage::Handoff => "neutral",
    }
}

const fn issue_status_rank(stage: LifecycleStage) -> u8 {
    match stage {
        LifecycleStage::Running => 0,
        LifecycleStage::Blocked => 1,
        LifecycleStage::Queued => 2,
        LifecycleStage::Failed => 3,
        LifecycleStage::Canceled | LifecycleStage::Completed => 4,
    }
}

const fn lifecycle_label(stage: LifecycleStage) -> &'static str {
    match stage {
        LifecycleStage::Queued => "queued",
        LifecycleStage::Running => "running",
        LifecycleStage::Blocked => "blocked",
        LifecycleStage::Failed => "failed",
        LifecycleStage::Canceled => "canceled",
        LifecycleStage::Completed => "completed",
    }
}

const fn cleanup_label(status: CleanupStatus) -> &'static str {
    match status {
        CleanupStatus::Clean => "clean",
        CleanupStatus::Pending => "pending",
        CleanupStatus::InProgress => "in progress",
        CleanupStatus::Complete => "complete",
        CleanupStatus::Failed => "failed",
    }
}

const fn open_code_stage_label(stage: OpenCodeStage) -> &'static str {
    stage.as_str()
}

const fn option_bool(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "true",
        Some(false) => "false",
        None => "unknown",
    }
}

const fn process_classification(process_alive: Option<bool>) -> &'static str {
    match process_alive {
        Some(true) => "live",
        Some(false) => "dead",
        None => "unknown",
    }
}

const fn bool_label(value: bool) -> &'static str {
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
:root{color-scheme:light;--bg:#f6f3ec;--surface:#fffdf8;--raised:#ffffff;--ink:#1d2522;--muted:#6e746f;--line:#d9d2c4;--line-strong:#b8ae9d;--brand:#0f766e;--brand-soft:#dff3ef;--ok:#16784b;--ok-bg:#dff5e8;--warn:#946200;--warn-bg:#fff0bf;--bad:#b4233a;--bad-bg:#ffe0e6;--idle:#315b9d;--idle-bg:#e3edff;--neutral:#565f67;--neutral-bg:#e8e7e2;--shadow:0 12px 30px rgba(31,41,35,.08)}*{box-sizing:border-box}body{margin:0;background:linear-gradient(180deg,#fbfaf6 0,#f1ede3 100%);color:var(--ink);font:14px/1.45 "Aptos","IBM Plex Sans","Segoe UI",sans-serif}.page{width:min(1480px,calc(100vw - 32px));margin:0 auto;padding:24px 0 40px}a{color:#0f5f68;text-decoration:none}a:hover{text-decoration:underline}.hero{display:flex;align-items:flex-end;justify-content:space-between;gap:24px;margin-bottom:18px;padding:26px 28px;border:1px solid var(--line);border-radius:8px;background:var(--surface);box-shadow:var(--shadow)}.project-hero{align-items:center}.eyebrow{margin:0 0 6px;color:var(--brand);font-size:12px;font-weight:800;text-transform:uppercase}.hero h1{margin:0;font-size:34px;line-height:1.05}.refresh{padding:8px 12px;border:1px solid var(--line);border-radius:999px;background:#f3efe5;color:var(--muted);white-space:nowrap}.stat-grid{display:grid;grid-template-columns:repeat(5,minmax(0,1fr));gap:12px;margin-bottom:22px}.stat-card{min-width:0;padding:16px;border:1px solid var(--line);border-radius:8px;background:var(--raised)}.stat-card span{display:block;margin-bottom:8px;color:var(--muted);font-size:12px;font-weight:800;text-transform:uppercase}.stat-card strong{display:block;font-size:28px;line-height:1;overflow-wrap:anywhere}.stat-card small{display:block;margin-top:8px;color:var(--muted)}.section-head{display:flex;align-items:end;justify-content:space-between;gap:16px;margin:22px 0 10px}.section-head h2{margin:0;font-size:20px}.section-head p{margin:0;color:var(--muted)}.running-grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(430px,1fr));gap:14px;margin-bottom:20px}.running-card,.project-card,.card,.empty-state{border:1px solid var(--line);border-radius:8px;background:var(--raised);box-shadow:var(--shadow)}.running-card{padding:18px}.running-title,.card-top{display:flex;align-items:flex-start;justify-content:space-between;gap:12px}.running-title h3,.card-top h3{margin:0;font-size:18px;line-height:1.2}.project-grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(360px,1fr));gap:14px}.project-card{padding:18px;min-height:230px}.project-line,.project-current,.reason{margin:12px 0 0;color:var(--muted)}.reason{display:flex;gap:10px;align-items:flex-start;padding-top:12px;border-top:1px solid var(--line)}.reason span{flex:0 0 auto;padding:3px 7px;border-radius:6px;background:var(--neutral-bg);color:var(--neutral);font-size:12px;font-weight:800}.empty-state{padding:22px;margin-bottom:20px}.empty-state h3{margin:0 0 6px}.empty-state p{margin:0;color:var(--muted)}.console-head,.card{padding:16px;margin-bottom:14px}.grid{display:grid;gap:14px}.two{grid-template-columns:repeat(auto-fit,minmax(360px,1fr));margin-bottom:14px}.metrics{display:grid;grid-template-columns:repeat(4,minmax(0,1fr));gap:8px;margin:14px 0}.metrics.focused{grid-template-columns:repeat(4,minmax(96px,1fr))}.metrics.compact{grid-template-columns:repeat(auto-fit,minmax(96px,1fr))}.metrics div{min-width:0;padding:10px;border:1px solid var(--line);border-radius:8px;background:#faf7ef}dt{color:var(--muted);font-size:12px;text-transform:uppercase}dd{margin:0;font-weight:800;overflow-wrap:anywhere}.facts{display:grid;grid-template-columns:minmax(130px,190px) 1fr;gap:7px 12px}.compact-facts{grid-template-columns:minmax(92px,130px) 1fr}.badge{display:inline-flex;align-items:center;min-height:28px;border:1px solid var(--line);border-radius:7px;padding:3px 8px;font-size:12px;font-weight:900;white-space:nowrap}.badge.ok{color:var(--ok);background:var(--ok-bg);border-color:#a8dfbf}.badge.warn{color:var(--warn);background:var(--warn-bg);border-color:#e8c96d}.badge.bad{color:var(--bad);background:var(--bad-bg);border-color:#f3a5b3}.badge.idle{color:var(--idle);background:var(--idle-bg);border-color:#b6cdf7}.badge.neutral{color:var(--neutral);background:var(--neutral-bg)}.muted{color:var(--muted)}.warning{color:#8a5400}.issue-list{display:grid;gap:8px}.issue-row{display:grid;grid-template-columns:110px minmax(160px,1fr) auto minmax(180px,1fr);gap:12px;align-items:center;padding:10px 12px;border:1px solid var(--line);border-radius:8px;background:#faf7ef;overflow-wrap:anywhere}.session{margin:10px 0 0;padding:12px;border:1px solid var(--line);border-radius:8px;background:#faf7ef}.activity{margin-top:10px;padding:12px;border-left:4px solid var(--brand);background:var(--brand-soft)}.dense,.compact-list{display:grid;gap:6px;padding-left:18px}.empty{text-align:center;color:var(--muted)}nav{margin-bottom:12px}.card h2,.card h3,.card h4,.card h5,.card p{margin-top:0}@media(max-width:1020px){.stat-grid{grid-template-columns:repeat(2,minmax(0,1fr))}.running-grid{grid-template-columns:1fr}}@media(max-width:760px){.page{width:min(100vw - 20px,1480px);padding-top:14px}.hero,.section-head,.running-title,.card-top{display:block}.hero h1{font-size:28px}.project-grid{grid-template-columns:1fr}.issue-row{grid-template-columns:1fr}.facts,.compact-facts{grid-template-columns:1fr}.metrics,.metrics.focused{grid-template-columns:repeat(2,minmax(0,1fr))}.stat-grid{grid-template-columns:1fr}}
"#;
