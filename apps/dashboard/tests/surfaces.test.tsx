import { describe, expect, test } from "bun:test";
import type React from "react";
import { renderToStaticMarkup } from "react-dom/server";

import { DefectsSurface, OverviewSurface, ProjectSurface, ProjectsSurface, QuotaSurface } from "@/src/components";
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

  test("overview renders running operations first", () => {
    const html = render(<OverviewSurface dashboard={acceptanceDashboard} quota={quotaNormal} />);

    expect(html).toContain("Running now");
    expect(html).toContain("Sessions");
    expect(html).toContain("4 slots available");
    expect(html).toContain("55,130 tokens");
    expect(html).toContain("3,110 cached");
    expect(html).not.toContain(">Capacity</p>");
    expect(html).toContain("SYM-97");
    expect(sectionText(html, "Running now", "Blockers and idle reasons")).toContain(">duration</th>");
    expect(sectionText(html, "Running now", "Blockers and idle reasons")).toContain(">provider/state</th>");
    expect(sectionText(html, "Running now", "Blockers and idle reasons")).toContain("1h 0m");
    expect(sectionText(html, "Running now", "Blockers and idle reasons")).toContain("runner ACP");
    expect(sectionText(html, "Running now", "Blockers and idle reasons")).toContain("OMP ACP");
    expect(sectionText(html, "Running now", "Blockers and idle reasons")).toContain("provider auth unavailable");
    expect(sectionText(html, "Running now", "Blockers and idle reasons")).toContain("pid 5321 stale/stopped");
    expect(sectionText(html, "Running now", "Blockers and idle reasons")).toContain("5 ACP frames");
    expect(sectionText(html, "Running now", "Blockers and idle reasons")).not.toContain("component tests passed");
    expect(sectionText(html, "Running now", "Blockers and idle reasons")).not.toContain(">last event</th>");
    expect(sectionText(html, "Running now", "Blockers and idle reasons")).not.toContain(">tools<");
    expect(sectionText(html, "Running now", "Blockers and idle reasons")).not.toContain("running /");
    expect(sectionText(html, "Running now", "Blockers and idle reasons")).not.toContain("worktree");
    expect(sectionText(html, "Running now", "Blockers and idle reasons")).not.toContain("/home/agent/.symphony/workspaces");
    expect(sectionText(html, "Blockers and idle reasons", "Project health")).toContain("No blockers reported");
    expect(sectionText(html, "Blockers and idle reasons", "Project health")).not.toContain("two runner sessions are executing");
    expect(sectionText(html, "Blockers and idle reasons", "Project health")).not.toContain("running/slots");
    expect(sectionText(html, "Blockers and idle reasons", "Project health")).not.toContain(">active<");
    expect(sectionText(html, "Blockers and idle reasons", "Project health")).not.toContain(">blocked<");
    expect(html).not.toContain(`${"co"}st`);
  });

  test("overview renders live session duration instead of raw events", () => {
    const dashboard = JSON.parse(JSON.stringify(acceptanceDashboard)) as typeof acceptanceDashboard;
    dashboard.projects[0].running_issues[0].last_event = "runner_db_updated:1781882984000";
    const html = render(<OverviewSurface dashboard={dashboard} quota={quotaNormal} />);

    const running = sectionText(html, "Running now", "Blockers and idle reasons");
    expect(running).toContain("1h 0m");
    expect(running).not.toContain("runner activity updated");
    expect(running).not.toContain("runner_db_updated");
  });

  test("projects surface renders table-first comparison", () => {
    const html = render(<ProjectsSurface dashboard={acceptanceDashboard} />);

    expect(html).toContain(">slots</th>");
    expect(html).toContain("title=\"running/slots\"");
    expect(html).not.toContain(">running/slots</th>");
    expect(html).toContain("waiting for quota reset");
    expect(html).not.toContain(">last event</th>");
    expect(html).not.toContain("provider quota exhausted");
  });

  test("project detail renders blocked and runtime-defect states", () => {
    const blocked = render(<ProjectSurface project={acceptanceProject} />);
    const failed = render(<ProjectSurface project={failedProject} />);

    expect(sectionText(blocked, "Symphony current execution", "Queue and blockers")).toContain("SYM-97");
    expect(sectionText(blocked, "Symphony current execution", "Queue and blockers")).toContain(">duration</th>");
    expect(sectionText(blocked, "Symphony current execution", "Queue and blockers")).toContain("1h 0m");
    expect(sectionText(blocked, "Symphony current execution", "Queue and blockers")).not.toContain(">last event</th>");
    expect(sectionText(blocked, "Symphony current execution", "Queue and blockers")).not.toContain("SYM-91");
    expect(blocked).toContain("provider quota exhausted");
    expect(failed).toContain("runtime_process_exit");
    expect(failed).toContain("restart supervised runner");
  });

  test("issue inspector renders bounded operational tabs", () => {
    const html = render(<IssueInspector issue={acceptanceProject.active_issues[0]} />);

    expect(html).toContain("runner session inspector");
    expect(html).toContain("Open in Linear");
    expect(html).toContain("https://linear.app/alexey-zakharov/issue/SYM-97");
    expect(html).toContain("Open in runner");
    expect(html).toContain("https://runner.vestalink.net/L3dvcmtzcGFjZXMvc3ltcGhvbnkvU1lNLTk3/session/oc-sym-97");
    expect(html).not.toContain("https://runner.vestalink.net/session/oc-sym-97");
    expect(html).toContain("Todos");
    expect(html).toContain("Timeline");
    expect(html).toContain("Evidence");
    expect(countOccurrences(html, ">running</span>")).toBe(1);
    expect(html).not.toContain(">review</span>");
    expect(html).toContain("Capture smoke screenshots");
    expect(html).toContain("line-through");
    expect(html).toContain("animate-spin");
    expect(html).toContain("aria-label=\"pending\"");
    expect(html).toContain(">35,100</dd>");
    expect(html).toContain(">3,110 cached</dd>");
    expect(html).toContain(">runner ACP</dd>");
    expect(html).toContain("provider runner-primary");
    expect(html).toContain(">18 frames</dd>");
    expect(html).toContain(">duration</dt>");
    expect(html).toContain("<span class=\"tabular-nums\">1h 0m</span>");
    expect(html).not.toContain(">cached</dt>");
    expect(html).not.toContain(">last event</dt>");
    expect(html).not.toContain(">process</dt>");
    expect(html).not.toContain("runner activity updated</dd>");
    expect(html).not.toContain("updated 178");
    expect(html).not.toContain(">medium<");
  });

  test("issue inspector renders OMP ACP blocker telemetry without raw event dumps", () => {
    const html = render(<IssueInspector issue={failedProject.active_issues[0]} />);

    expect(html).toContain("OMP ACP");
    expect(html).toContain("provider omp-primary");
    expect(html).toContain("oc-atl-42");
    expect(html).toContain("pid 5321 stale/stopped");
    expect(html).toContain("provider auth unavailable");
    expect(html).toContain("session is quiet or stale");
    expect(html).toContain(">5 frames</dd>");
    expect(html).toContain("sdk:auth");
    expect(html).not.toContain("runtime process exited</dd>");
  });

  test("issue inspector links runner sessions by persisted session directory", () => {
    const issue = JSON.parse(JSON.stringify(acceptanceProject.active_issues[0])) as typeof acceptanceProject.active_issues[number];
    issue.runner_sessions[0].worktree_path = "/runtime/stale/worktree";
    issue.runner_sessions[0].activity!.sessions[0].directory = "/actual/runner/session";
    const html = render(<IssueInspector issue={issue} />);

    expect(html).toContain("https://runner.vestalink.net/L2FjdHVhbC9ydW5uZXIvc2Vzc2lvbg/session/oc-sym-97");
    expect(html).not.toContain("L3J1bnRpbWUvc3RhbGUvd29ya3RyZWU");
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
    expect(normal).toContain("Jun 19, 2026");
    expect(normal).not.toContain("2026-06-19");
    expect(normal).not.toContain("provider-quota --localhost --plain");
    expect(unavailable).not.toContain("provider-quota --plain");
  });

  test("defects surface renders deduped defect table and empty state", () => {
    const populated = render(<DefectsSurface defects={defectRoutesFromFixtures()} />);
    const empty = render(<DefectsSurface defects={[]} />);

    expect(populated).toContain("runner-timeout:sym-91");
    expect(populated).toContain("repair managed defect");
    expect(empty).toContain("No Symphony self/runtime defects");
  });
});

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
