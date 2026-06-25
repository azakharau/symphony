import { describe, expect, test } from "bun:test";
import type React from "react";
import { renderToStaticMarkup } from "react-dom/server";

import { DefectsSurface, OverviewSurface, ProjectSurface, ProjectsSurface, QuotaSurface } from "@/src/components";
import { currentRunnerSession } from "@/src/current-runner-session";
import { formatDuration, LIVE_DURATION_REFRESH_MS, resolveLiveDurationMs } from "@/src/live-duration";
import { ActivityTimeline, AgentsTree, IssueInspector, ToolsByAgent } from "@/src/issue-inspector";
import {
  acceptanceDashboard,
  acceptanceProject,
  defectRoutesFromFixtures,
  emptyDashboard,
  failedProject,
  quotaNormal,
  quotaUnavailable,
} from "@/src/fixtures";

describe("dashboard surfaces", () => {
  test("overview renders empty state with idle reason", () => {
    const html = render(<OverviewSurface dashboard={emptyDashboard} quota={quotaUnavailable} />);

    expect(html).toContain("No runner sessions are running");
    expect(html).toContain("waiting for eligible issues");
    expect(html).toContain("unavailable");
  });

  test("overview renders operations-first order with compact quota link", () => {
    const html = render(<OverviewSurface dashboard={acceptanceDashboard} quota={quotaNormal} />);
    const runningIndex = html.indexOf("Running now");
    const healthIndex = html.indexOf("Project health and capacity");
    const blockersIndex = html.indexOf("Blockers and idle reasons");
    const running = sectionText(html, "Running now", "Project health and capacity");
    const health = sectionText(html, "Project health and capacity", "Blockers and idle reasons");
    const blockers = sectionText(html, "Blockers and idle reasons", "overview preserves OMP cacheRead");

    expect(runningIndex).toBeGreaterThanOrEqual(0);
    expect(healthIndex).toBeGreaterThan(runningIndex);
    expect(blockersIndex).toBeGreaterThan(healthIndex);
    expect(html).toContain("sessions 2/7");
    expect(html).toContain("4 slots available");
    expect(html).toContain("5h quota 76% remaining");
    expect(html).toContain('href="/quota"');
    expect(running).toContain("58,240 / 58,240 tokens");
    expect(running).toContain("55,130 non-cache");
    expect(running).toContain("3,110 cached (read 2,800 · write 310)");
    expect(running).toContain("metrics available");
    expect(running).toContain("38,210 / 38,210 total");
    expect(running).toContain("degraded split");
    expect(running).toContain("metrics degraded");
    expect(html).not.toContain(">Capacity</p>");
    expect(running).toContain("SYM-97");
    expect(running).toContain(">duration</th>");
    expect(running).toContain(">provider/state</th>");
    expect(running).toContain("1h 0m");
    expect(running).toContain("runner ACP");
    expect(running).toContain("OMP ACP");
    expect(running).toContain("provider auth unavailable");
    expect(running).toContain("pid 5321 stale/stopped");
    expect(running).toContain("5 ACP frames");
    expect(running).not.toContain("component tests passed");
    expect(running).not.toContain(">last event</th>");
    expect(running).not.toContain(">tools<");
    expect(running).not.toContain("running /");
    expect(running).not.toContain("worktree");
    expect(running).not.toContain("/home/agent/.symphony/workspaces");
    expect(health).toContain("running sessions / max slots");
    expect(health).toContain("waiting for quota reset");
    expect(blockers).toContain("Atlas");
    expect(blockers).toContain("blocked");
    expect(blockers).toContain("waiting for quota reset");
    expect(blockers).not.toContain("No blockers reported");
    expect(html).not.toContain("running/slots");
    expect(html).not.toContain(`${"co"}st`);
  });

  test("overview attention excludes active capacity and uses liveness copy", () => {
    const dashboard = JSON.parse(JSON.stringify(acceptanceDashboard)) as typeof acceptanceDashboard;
    const activeProject = dashboard.projects[0];
    activeProject.runner_health = "blocked";
    activeProject.last_event = "linear_terminal_reconciled";
    activeProject.capacity = { max_sessions: 4, running_sessions: 1, available_sessions: 3 };
    activeProject.liveness = {
      ...activeProject.liveness,
      status: "active",
      reason: "runner session is live",
      primary_reason_code: "active_runner_session",
      primary_reason_detail: "runner session is executing",
      capacity: activeProject.capacity,
    };
    dashboard.projects = [activeProject, dashboard.projects[2]];

    const html = render(<OverviewSurface dashboard={dashboard} quota={quotaNormal} />);
    const blockers = sectionText(html, "Blockers and idle reasons", "overview preserves OMP cacheRead");

    expect(blockers).not.toContain("Symphony");
    expect(blockers).toContain("Atlas");
    expect(blockers).toContain("waiting for quota reset");
    expect(blockers).toContain("provider quota exhausted");
    expect(blockers).not.toContain(">last event</th>");
    expect(blockers).not.toContain("linear_terminal_reconciled");
  });

  test("overview preserves OMP cacheRead/cacheWrite split instead of dropping cached tokens", () => {
    const dashboard = JSON.parse(JSON.stringify(acceptanceDashboard)) as typeof acceptanceDashboard;
    const ompMetrics = {
      accounted_total_token_count: 561,
      non_cached_token_count: 155,
      cached_token_count: 406,
      input_token_count: 120,
      output_token_count: 30,
      reasoning_token_count: 5,
      cache_read_token_count: 400,
      cache_write_token_count: 6,
      reported_total_token_count: 700,
      metrics_status: "available",
      metrics_source: "persisted_split_metrics",
      metrics_freshness: "fresh",
      metrics_reason: null,
    };
    const issue = dashboard.projects[0].running_issues[0];
    dashboard.totals.running_tokens = 561;
    dashboard.totals.running_cached_tokens = 406;
    dashboard.totals.token_metrics = ompMetrics;
    dashboard.projects = [dashboard.projects[0]];
    dashboard.projects[0].running_tokens = 561;
    dashboard.projects[0].running_cached_tokens = 406;
    dashboard.projects[0].token_metrics = ompMetrics;
    issue.token_count = 561;
    issue.cached_token_count = 406;
    issue.token_metrics = ompMetrics;

    const html = render(<OverviewSurface dashboard={dashboard} quota={quotaNormal} />);
    const running = sectionText(html, "Running now", "Project health and capacity");
    expect(running).toContain("561 / 700 tokens");
    expect(running).toContain("155 non-cache");
    expect(running).toContain("406 cached (read 400 · write 6)");
    expect(running).toContain("561 / 700 total");
    expect(running).toContain("155 non-cache");
    expect(running).toContain("406 cached (read 400 · write 6)");
    expect(running).toContain("metrics available");
  });

  test("overview does not imply zero cached tokens when token metrics are unavailable", () => {
    const dashboard = JSON.parse(JSON.stringify(acceptanceDashboard)) as typeof acceptanceDashboard;
    dashboard.totals.token_metrics = {
      accounted_total_token_count: 15030,
      non_cached_token_count: 15030,
      cached_token_count: 0,
      input_token_count: 0,
      output_token_count: 0,
      reasoning_token_count: 15030,
      cache_read_token_count: 0,
      cache_write_token_count: 0,
      reported_total_token_count: 15030,
      metrics_status: "unavailable",
      metrics_source: "test",
      metrics_freshness: "unavailable",
      metrics_reason: "no token metrics collected",
    };
    dashboard.projects[0].running_issues[0].token_metrics = undefined;
    dashboard.projects[0].running_issues[0].cached_token_count = undefined;

    const html = render(<OverviewSurface dashboard={dashboard} quota={quotaNormal} />);
    const running = sectionText(html, "Running now", "Project health and capacity");

    expect(html).toContain("unavailable split");
    expect(html).toContain("metrics unavailable");
    expect(running).toContain("metrics unavailable");
    expect(running).not.toContain("0 cached");
    expect(running).not.toContain("15,030 non-cache");
  });

  test("overview labels stale token metrics with freshness reason", () => {
    const dashboard = JSON.parse(JSON.stringify(acceptanceDashboard)) as typeof acceptanceDashboard;
    const staleMetrics = {
      accounted_total_token_count: 300,
      non_cached_token_count: 180,
      cached_token_count: 120,
      input_token_count: 150,
      output_token_count: 30,
      reasoning_token_count: 7,
      cache_read_token_count: 120,
      cache_write_token_count: 0,
      reported_total_token_count: 300,
      metrics_status: "available",
      metrics_source: "persisted_split_metrics",
      metrics_freshness: "stale",
      metrics_reason: "metrics are stale; latest runtime usage update is older than 10 minutes",
    };
    dashboard.totals.running_tokens = 300;
    dashboard.totals.running_cached_tokens = 120;
    dashboard.totals.token_metrics = staleMetrics;
    dashboard.projects = [dashboard.projects[0]];
    dashboard.projects[0].running_tokens = 300;
    dashboard.projects[0].running_cached_tokens = 120;
    dashboard.projects[0].token_metrics = staleMetrics;
    dashboard.projects[0].running_issues[0].token_count = 300;
    dashboard.projects[0].running_issues[0].cached_token_count = 120;
    dashboard.projects[0].running_issues[0].token_metrics = staleMetrics;

    const html = render(<OverviewSurface dashboard={dashboard} quota={quotaNormal} />);
    const running = sectionText(html, "Running now", "Project health and capacity");

    expect(running).toContain("180 non-cache");
    expect(running).toContain("120 cached (read 120 · write 0)");
    expect(running).toContain("metrics available · stale");
    expect(running).toContain("latest runtime usage update is older than 10 minutes");
  });

  test("overview renders live session duration instead of raw events", () => {
    const dashboard = JSON.parse(JSON.stringify(acceptanceDashboard)) as typeof acceptanceDashboard;
    dashboard.projects[0].running_issues[0].last_event = "opencode_db_updated:1781882984000";
    const html = render(<OverviewSurface dashboard={dashboard} quota={quotaNormal} />);

    const running = sectionText(html, "Running now", "Project health and capacity");
    expect(running).toContain("1h 0m");
    expect(running).not.toContain("OpenCode activity updated");
    expect(running).not.toContain("opencode_db_updated");
  });

  test("projects surface renders table-first comparison", () => {
    const html = render(<ProjectsSurface dashboard={acceptanceDashboard} />);

    expect(html).toContain(">slots</th>");
    expect(html).toContain("title=\"running/slots\"");
    expect(html).not.toContain(">running/slots</th>");
    expect(html).toContain("provider quota exhausted");
  });

  test("project detail prioritizes current execution and concise queue blockers", () => {
    const blocked = render(<ProjectSurface project={acceptanceProject} />);
    const failed = render(<ProjectSurface project={failedProject} />);
    const current = sectionText(blocked, "Symphony current execution", "Queue and blockers");
    const queue = sectionText(blocked, "Queue and blockers", "Runtime");

    expect(blocked.indexOf("Symphony current execution")).toBeLessThan(blocked.indexOf("Runtime"));
    expect(current).toContain("SYM-97");
    expect(current).toContain(">duration</th>");
    expect(current).toContain("1h 0m");
    expect(current).toContain(">operational detail</th>");
    expect(current).toContain("stage review");
    expect(current).not.toContain(">last event</th>");
    expect(current).not.toContain("component tests passed");
    expect(current).not.toContain("worktree");
    expect(queue).toContain("next SYM-97");
    expect(queue).toContain("next eligible");
    expect(queue).toContain("SYM-91");
    expect(countOccurrences(queue, "SYM-91")).toBe(1);
    expect(queue).toContain("provider quota exhausted");
    expect(queue).toContain("repair managed defect");
    expect(queue).toContain("provider/infra blocker");
    expect(queue).not.toContain("provider_blocker");
    expect(failed).toContain("runtime process exit");
    expect(failed).not.toContain("runtime_process_exit");
    expect(failed).toContain("restart supervised runner");
  });

  test("project detail bounds recent history by default", () => {
    const project = JSON.parse(JSON.stringify(acceptanceProject)) as typeof acceptanceProject;
    project.history_issues = Array.from({ length: 7 }, (_, index) => ({
      ...project.history_issues[0],
      issue_id: `sym-history-${index + 1}`,
      identifier: `SYM-H${index + 1}`,
      title: `Completed history ${index + 1}`,
    }));

    const html = render(<ProjectSurface project={project} />);
    const history = sectionText(html, "Recent run history", "Related defects");

    expect(history).toContain("5 of 7 shown");
    expect(history).toContain("Showing newest 5 of 7 terminal runs");
    expect(history).toContain("SYM-H1");
    expect(history).toContain("SYM-H5");
    expect(history).not.toContain("SYM-H6");
    expect(history).not.toContain("SYM-H7");
  });

  test("issue inspector renders an execution-model drilldown without placeholder cards", () => {
    const html = render(<IssueInspector issue={acceptanceProject.active_issues[0]} />);
    const hero = sectionText(html, "Build dashboard surfaces", "Current runner status");
    const inspector = sectionText(html, "Current runner status", "Lifecycle timeline");
    const timeline = sectionText(html, "Lifecycle timeline", "OMP workers");

    expect(html).toContain("Current runner status");
    expect(html).toContain("Open in Linear");
    expect(html).toContain("https://linear.app/alexey-zakharov/issue/SYM-97");
    expect(html).toContain("Open in runner");
    expect(html).toContain("https://runner.vestalink.net/L3dvcmtzcGFjZXMvc3ltcGhvbnkvU1lNLTk3/session/oc-sym-97");
    expect(html).not.toContain("https://runner.vestalink.net/session/oc-sym-97");
    expect(html).toContain("Lifecycle timeline");
    expect(html).toContain("OMP workers");
    expect(html).toContain("Todo activity");
    expect(html).toContain("Tool activity");
    expect(html).toContain("Eval state");
    expect(html).toContain("Git and worktree");
    expect(html).toContain("Debug details");
    expect(html).toContain("Raw issue JSON");
    expect(hero).toContain("typescript-engineer is executing review through runner ACP");
    expect(hero).not.toContain("Last event:");
    expect(hero).not.toContain("0 todos");
    expect(hero).not.toContain("0 agents");
    expect(hero).not.toContain("0 tool events");
    expect(hero).not.toContain("0 timeline events");
    expect(html).not.toContain(">Evidence</h2>");
    expect(html).toContain("Desktop and mobile route coverage in progress.");
    expect(timeline).toContain("stage starting");
    expect(timeline).toContain("stage running");
    expect(timeline).toContain("stage review");
    expect(timeline.indexOf("Scope accepted")).toBeLessThan(timeline.indexOf("Smoke"));
    expect(html).toContain("Capture smoke screenshots");
    expect(html).toContain("line-through");
    expect(html).toContain("animate-spin");
    expect(html).toContain("aria-label=\"pending\"");
    expect(inspector).toContain(">38,210 total</dd>");
    expect(inspector).toContain("35,100 non-cache");
    expect(inspector).toContain("3,110 cached");
    expect(inspector).toContain(">runner ACP</dd>");
    expect(inspector).toContain("provider runner-primary");
    expect(inspector).toContain(">18 frames</dd>");
    expect(inspector).toContain(">duration</dt>");
    expect(inspector).toContain("<span class=\"tabular-nums\">1h 0m</span>");
    expect(html).toContain("sym-97-dashboard-surfaces");
    expect(html).toContain("abc1234");
    expect(html).toContain("push/merge");
    expect(html).not.toContain("OpenCode activity updated</dd>");
    expect(html).not.toContain(">medium<");
  });

  test("issue inspector renders OMP ACP blocker telemetry with clear unavailable sources", () => {
    const html = render(<IssueInspector issue={failedProject.active_issues[0]} />);
    const blocker = sectionText(html, "Blocker and failure state", "Debug details");

    expect(html).toContain("OMP ACP");
    expect(html).toContain("provider omp-primary");
    expect(html).toContain("oc-atl-42");
    expect(html).toContain("pid 5321 stale/stopped");
    expect(html).toContain("provider auth unavailable");
    expect(html).toContain("session is quiet or stale");
    expect(html).toContain(">5 frames</dd>");
    expect(html).toContain("sdk:auth");
    expect(html).toContain("bounded activity unavailable for exited process");
    expect(html).toContain("No running, pending, or recent tool events were reported.");
    expect(blocker).toContain("Blocker and failure state");
    expect(blocker).toContain("restart supervised runner · runtime process exit");
    expect(blocker).not.toContain("runtime_process_exit");
    expect(blocker).not.toContain("runtime process exited</dd>");
  });

  test("issue inspector does not imply zero cached tokens when metrics are unavailable", () => {
    const issue = JSON.parse(JSON.stringify(failedProject.active_issues[0])) as typeof failedProject.active_issues[number];
    issue.runner_sessions[0].cached_token_count = null;
    issue.runner_sessions[0].token_metrics = {
      accounted_total_token_count: 15030,
      non_cached_token_count: 15030,
      cached_token_count: 0,
      input_token_count: 0,
      output_token_count: 0,
      reasoning_token_count: 15030,
      cache_read_token_count: 0,
      cache_write_token_count: 0,
      reported_total_token_count: 15030,
      metrics_status: "unavailable",
      metrics_source: "none",
      metrics_freshness: "unavailable",
      metrics_reason: "no token metrics collected",
    };

    const html = render(<IssueInspector issue={issue} />);
    const inspector = sectionText(html, "Current runner status", "Debug details");

    expect(inspector).toContain("unavailable split");
    expect(inspector).toContain("metrics unavailable");
    expect(inspector).toContain("no token metrics collected");
    expect(inspector).toContain(">unavailable</dd>");
    expect(inspector).not.toContain("0 cached");
  });

  test("issue inspector links runner sessions by persisted session directory", () => {
    const issue = JSON.parse(JSON.stringify(acceptanceProject.active_issues[0])) as typeof acceptanceProject.active_issues[number];
    issue.runner_sessions[0].worktree_path = "/runtime/stale/worktree";
    issue.runner_sessions[0].activity!.sessions[0].directory = "/actual/runner/session";
    const html = render(<IssueInspector issue={issue} />);

    expect(html).toContain("https://runner.vestalink.net/L2FjdHVhbC9ydW5uZXIvc2Vzc2lvbg/session/oc-sym-97");
    expect(html).not.toContain("L3J1bnRpbWUvc3RhbGUvd29ya3RyZWU");
  });

  test("project detail uses preferred runner session instead of a stale tail session", () => {
    const project = withStaleTailSession();
    const html = render(<ProjectSurface project={project} />);
    const currentExecution = sectionText(html, "Symphony current execution", "Queue and blockers");

    expect(currentRunnerSession(project.active_issues[0])?.runner_session_id).toBe("oc-sym-97");
    expect(currentExecution).toContain("oc-sym-97");
    expect(currentExecution).toContain("runner-primary");
    expect(currentExecution).toContain("typescript-engineer");
    expect(currentExecution).toContain("gpt-5.5");
    expect(currentExecution).toContain("1/2");
    expect(currentExecution).toContain(">5</td>");
    expect(currentExecution).toContain("stage review");
    expect(currentExecution).not.toContain("component tests passed");
    expect(currentExecution).not.toContain("/workspaces/symphony/SYM-97");
    expect(currentExecution).not.toContain("stale-tail");
    expect(currentExecution).not.toContain("stale-provider");
    expect(currentExecution).not.toContain("stale event");
    expect(currentExecution).not.toContain("/stale/worktree");
  });

  test("issue inspector sections use preferred runner session instead of a stale tail session", () => {
    const issue = withStaleTailSession().active_issues[0];
    const html = render(<IssueInspector issue={issue} />);
    const selectedSessionSurface = sectionText(html, "Current runner status", "Debug details");
    const agents = render(<AgentsTree issue={issue} />);
    const events = currentRunnerSession(issue)?.activity?.timeline.filter((event) => event.kind === "tool" || event.tool) ?? [];
    const tools = render(<ToolsByAgent issue={issue} events={events} />);

    expect(html).toContain("oc-sym-97");
    expect(html).toContain("runner-primary");
    expect(html).toContain("Capture smoke screenshots");
    expect(agents).toContain("Dashboard pages");
    expect(tools).toContain("typescript-engineer · 1 tool events");
    expect(selectedSessionSurface).not.toContain("stale-tail");
    expect(selectedSessionSurface).not.toContain("stale-provider");
    expect(selectedSessionSurface).not.toContain("stale-only todo");
    expect(agents).not.toContain("stale-agent");
    expect(tools).not.toContain("stale-agent");
  });

  test("live duration resolver refreshes from started_at_ms on one second cadence", () => {
    const startedAtMs = 10_000;
    const firstTick = resolveLiveDurationMs(startedAtMs, 500, startedAtMs + LIVE_DURATION_REFRESH_MS);
    const secondTick = resolveLiveDurationMs(startedAtMs, 500, startedAtMs + LIVE_DURATION_REFRESH_MS * 2);

    expect(formatDuration(firstTick)).toBe("1s");
    expect(formatDuration(secondTick)).toBe("2s");
    expect(resolveLiveDurationMs(null, 500, startedAtMs + LIVE_DURATION_REFRESH_MS)).toBe(500);
  });

  test("duration resolver displays elapsed time from OMP start evidence without fallback duration", () => {
    const startedAtMs = 1_781_880_000_000;
    const nowMs = startedAtMs + 3_600_000;

    expect(resolveLiveDurationMs(startedAtMs, null, nowMs)).toBe(3_600_000);
    expect(formatDuration(resolveLiveDurationMs(startedAtMs, null, nowMs))).toBe("1h 0m");
  });

  test("agent inspector renders a tree with active spinner", () => {
    const html = render(<AgentsTree issue={acceptanceProject.active_issues[0]} />);

    expect(html).toContain("Dashboard pages");
    expect(html).toContain("border-l border-slate-200");
    expect(html).toContain("aria-label=\"active agent\"");
    expect(html).toContain("animate-spin");
    expect(html).toContain("36,000 tokens · 2,210 cached");
    expect(html).toContain("14,000 tokens · 900 cached");
    expect(html).toContain(">root</span>");
    expect(html).toContain(">subagent</span>");
    expect(html).not.toContain("parent oc-sym-97");
  });

  test("timeline renders as a compact event stream", () => {
    const events = acceptanceProject.active_issues[0].runner_sessions[0].activity?.timeline ?? [];
    const html = render(<ActivityTimeline events={events} />);

    expect(html).toContain("border-l border-slate-200");
    expect(html).toContain("Desktop and mobile route coverage in progress.");
    expect(html).toContain("aria-label=\"running event\"");
    expect(html).toContain("animate-spin");
    expect(html).toContain("Jun 19, 2026");
    expect(html).not.toContain("<details");
    expect(html).not.toContain("status unknown");
  });

  test("tools render grouped by agent", () => {
    const issue = acceptanceProject.active_issues[0];
    const events = issue.runner_sessions[0].activity?.timeline.filter((event) => event.kind === "tool" || event.tool) ?? [];
    const html = render(<ToolsByAgent issue={issue} events={events} />);

    expect(html).toContain("Build dashboard surfaces");
    expect(html).toContain("Dashboard pages");
    expect(html).toContain("build · 1 tool events");
    expect(html).toContain("typescript-engineer · 1 tool events");
    expect(html).not.toContain("No running, pending, or recent tool events");
  });

  test("quota surface renders unavailable and normal windows", () => {
    const unavailable = render(<QuotaSurface quota={quotaUnavailable} />);
    const normal = render(<QuotaSurface quota={quotaNormal} />);

    expect(unavailable).toContain("Quota unavailable");
    expect(normal).toContain("5h window");
    expect(normal).toContain("weekly window");
    expect(normal).toContain("76% remaining");
    expect(normal).toContain("76% remaining · 24% used");
    expect(normal).toContain("Codex 5.3 Spark");
    expect(normal).toContain("general limit");
    expect(normal).toContain("model-specific limit");
    expect(normal).toContain("source ok · parse ok");
    expect(normal).toContain("ocu --localhost --plain");
    expect(normal).toContain("Jun 19, 2026");
    expect(normal).not.toContain("2026-06-19");
    expect(unavailable).toContain("source error · parse ok");
  });

  test("defects surface renders deduped defect table and empty state", () => {
    const populated = render(<DefectsSurface defects={defectRoutesFromFixtures()} />);
    const empty = render(<DefectsSurface defects={[]} />);

    expect(populated).toContain("runner-timeout:sym-91");
    expect(populated).toContain("1 fingerprints · 2 routed records");
    expect(populated).toContain("SYM-101, SYM-102");
    expect(populated).toContain("5");
    expect(populated).toContain("high");
    expect(populated).not.toContain("critical");
    expect(populated).toContain("repair managed defect");
    expect(populated).not.toContain("ignore resolved historical duplicate");
    expect(empty).toContain("No Symphony self/runtime defects");
  });
});

function withStaleTailSession(): typeof acceptanceProject {
  const project = JSON.parse(JSON.stringify(acceptanceProject)) as typeof acceptanceProject;
  const issue = project.active_issues[0];
  const preferredSession = issue.runner_sessions[0];

  issue.preferred_runner_session_id = preferredSession.runner_session_id;
  issue.runner_sessions.push({
    ...preferredSession,
    runner_session_id: "stale-tail",
    provider_id: "stale-provider",
    process_id: 9001,
    process_alive: false,
    active_agent: "stale-agent",
    active_model: "stale-model",
    todo_count: 99,
    started_at_ms: 1781870000000,
    duration_ms: 7_200_000,
    last_event: "stale event",
    worktree_path: "/stale/worktree",
    session_evidence_refs: ["stale:evidence"],
    activity: {
      root_session_id: "stale-tail",
      sessions: [
        {
          session_id: "stale-tail",
          parent_session_id: null,
          title: "Stale tail",
          directory: "/stale/worktree",
          agent: "stale-agent",
          model: "stale-model",
          is_subagent: false,
          tokens_input: 1,
          tokens_output: 1,
          tokens_reasoning: 1,
          tokens_cache_read: 0,
          tokens_cache_write: 0,
          time_created_ms: 1781870000000,
          time_updated_ms: 1781870001000,
        },
      ],
      subagents: [],
      todos: [{ session_id: "stale-tail", content: "stale-only todo", status: "in_progress", priority: "high", position: 1, time_updated_ms: 1781870001000 }],
      timeline: [{ session_id: "stale-tail", part_id: "stale-p1", time_created_ms: 1781870001000, time_updated_ms: 1781870001000, kind: "tool", tool: "stale-tool", status: "running", title: "Stale tool", summary: "stale event" }],
      running_tool_count: 9,
      pending_tool_count: 9,
      last_updated_ms: 1781870001000,
    },
  });

  return project;
}

function render(node: React.ReactElement): string {
  return renderToStaticMarkup(node);
}

function sectionText(html: string, start: string, end: string): string {
  const startIndex = html.indexOf(start);
  const endIndex = html.indexOf(end, startIndex);
  return html.slice(startIndex, endIndex < 0 ? undefined : endIndex);
}

function countOccurrences(value: string, needle: string): number {
  return value.split(needle).length - 1;
}
