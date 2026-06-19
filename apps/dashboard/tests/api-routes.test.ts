import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";

import { GET as getDashboard } from "@/app/api/dashboard/route";
import { GET as getProject } from "@/app/api/projects/[projectId]/route";
import { GET as getIssue } from "@/app/api/projects/[projectId]/issues/[issueId]/route";

const originalFetch = globalThis.fetch;
const originalApiBase = process.env.SYMPHONY_API_BASE;

describe("dashboard BFF route proxies", () => {
  beforeEach(() => {
    process.env.SYMPHONY_API_BASE = "http://rust.test/";
  });

  afterEach(() => {
    globalThis.fetch = originalFetch;
    if (originalApiBase === undefined) {
      delete process.env.SYMPHONY_API_BASE;
    } else {
      process.env.SYMPHONY_API_BASE = originalApiBase;
    }
  });

  test("dashboard route targets Rust UI dashboard contract and keeps public fallback metadata", async () => {
    const fetchMock = installJsonFetch({
      metadata: {
        polling_fallback_endpoint: "/api/dashboard/ui",
        live_events_endpoint: "/api/dashboard/events",
      },
      totals: { running_cost_micros: 1 },
    });

    const response = await getDashboard();

    expectFetchTarget(fetchMock, "http://rust.test/api/dashboard/ui");
    expect(await response.json()).toEqual({
      metadata: {
        polling_fallback_endpoint: "/api/dashboard",
        live_events_endpoint: "/api/dashboard/events",
      },
      totals: {},
    });
  });

  test("project route targets the Rust project UI contract", async () => {
    const fetchMock = installJsonFetch({ project: { id: "project 1" } });

    await getProject(new Request("http://bff.test/api/projects/project%201"), {
      params: Promise.resolve({ projectId: "project 1" }),
    });

    expectFetchTarget(fetchMock, "http://rust.test/api/projects/project%201/ui");
  });

  test("issue route targets the Rust issue UI contract", async () => {
    const fetchMock = installJsonFetch({ issue: { id: "issue/42" } });

    await getIssue(new Request("http://bff.test/api/projects/project%201/issues/issue%2F42"), {
      params: Promise.resolve({ projectId: "project 1", issueId: "issue/42" }),
    });

    expectFetchTarget(fetchMock, "http://rust.test/api/projects/project%201/issues/issue%2F42/ui");
  });
});

function installJsonFetch(payload: unknown) {
  const fetchMock = mock(async (input: RequestInfo | URL, init?: RequestInit) => {
    void input;
    void init;
    return new Response(JSON.stringify(payload), {
      status: 200,
      headers: { "content-type": "application/json" },
    });
  });
  globalThis.fetch = fetchMock as unknown as typeof fetch;
  return fetchMock;
}

function expectFetchTarget(
  fetchMock: ReturnType<typeof installJsonFetch>,
  expectedUrl: string,
): void {
  const [input, init] = fetchMock.mock.calls[0] as [RequestInfo | URL, RequestInit | undefined];
  expect(String(input)).toBe(expectedUrl);
  expect(new Headers(init?.headers).get("accept")).toBe("application/json");
  expect(init?.cache).toBe("no-store");
}
