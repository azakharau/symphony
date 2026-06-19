import { describe, expect, test } from "bun:test";
import type React from "react";
import { renderToStaticMarkup } from "react-dom/server";

import { DefectsSurface, OverviewSurface, ProjectSurface, ProjectsSurface, QuotaSurface } from "@/src/components";
import { IssueInspector } from "@/src/issue-inspector";
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

    expect(html).toContain("No OpenCode sessions are running");
    expect(html).toContain("waiting for eligible issues");
    expect(html).toContain("unavailable");
  });

  test("overview renders running operations first", () => {
    const html = render(<OverviewSurface dashboard={acceptanceDashboard} quota={quotaNormal} />);

    expect(html).toContain("Running now");
    expect(html).toContain("Sessions");
    expect(html).toContain("4 slots available");
    expect(html).not.toContain(">Capacity</p>");
    expect(html).toContain("SYM-97");
    expect(html).toContain("component tests passed");
    expect(sectionText(html, "Running now", "Blockers and idle reasons")).not.toContain(">tools<");
    expect(sectionText(html, "Running now", "Blockers and idle reasons")).not.toContain("running /");
    expect(sectionText(html, "Blockers and idle reasons", "Project health")).toContain("No blockers reported");
    expect(sectionText(html, "Blockers and idle reasons", "Project health")).not.toContain("two OpenCode sessions are executing");
    expect(sectionText(html, "Blockers and idle reasons", "Project health")).not.toContain("running/slots");
    expect(sectionText(html, "Blockers and idle reasons", "Project health")).not.toContain(">active<");
    expect(sectionText(html, "Blockers and idle reasons", "Project health")).not.toContain(">blocked<");
    expect(html).not.toContain(`${"co"}st`);
  });

  test("projects surface renders table-first comparison", () => {
    const html = render(<ProjectsSurface dashboard={acceptanceDashboard} />);

    expect(html).toContain("running/slots");
    expect(html).toContain("provider quota exhausted");
  });

  test("project detail renders blocked and runtime-defect states", () => {
    const blocked = render(<ProjectSurface project={acceptanceProject} />);
    const failed = render(<ProjectSurface project={failedProject} />);

    expect(blocked).toContain("provider quota exhausted");
    expect(failed).toContain("runtime_process_exit");
    expect(failed).toContain("restart supervised runner");
  });

  test("issue inspector renders bounded operational tabs", () => {
    const html = render(<IssueInspector issue={acceptanceProject.active_issues[0]} />);

    expect(html).toContain("OpenCode session inspector");
    expect(html).toContain("Todos");
    expect(html).toContain("Timeline");
    expect(html).toContain("Evidence");
    expect(countOccurrences(html, ">running</span>")).toBe(1);
    expect(html).not.toContain(">review</span>");
    expect(html).toContain("Capture smoke screenshots");
    expect(html).toContain("line-through");
    expect(html).toContain("animate-spin");
    expect(html).toContain("aria-label=\"pending\"");
    expect(html).not.toContain("updated 178");
    expect(html).not.toContain(">medium<");
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
    expect(normal).not.toContain("ocu --plain --localhost");
    expect(unavailable).not.toContain("ocu --plain");
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
