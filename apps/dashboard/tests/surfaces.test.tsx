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
    expect(html).toContain("SYM-97");
    expect(html).toContain("component tests passed");
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
    expect(html).toContain("Capture smoke screenshots");
  });

  test("quota surface renders unavailable and normal windows", () => {
    const unavailable = render(<QuotaSurface quota={quotaUnavailable} />);
    const normal = render(<QuotaSurface quota={quotaNormal} />);

    expect(unavailable).toContain("Quota unavailable");
    expect(normal).toContain("5h window");
    expect(normal).toContain("weekly window");
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
