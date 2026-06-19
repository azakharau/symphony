import { describe, expect, test } from "bun:test";

import {
  normalizeDashboardEventStream,
  normalizeDashboardPayload,
  removeCostFields,
} from "@/src/dashboard-contract";

describe("dashboard contract normalization", () => {
  test("removes cost fields recursively without removing token telemetry", () => {
    const normalized = removeCostFields({
      totals: {
        running_tokens: 10,
        running_cost_micros: 99,
      },
      projects: [
        {
          recorded_cost_micros: 12,
          running_issues: [{ cost_micros: 7, token_count: 42 }],
        },
      ],
    });

    expect(JSON.stringify(normalized)).not.toContain("cost");
    expect(normalized).toEqual({
      totals: { running_tokens: 10 },
      projects: [{ running_issues: [{ token_count: 42 }] }],
    });
  });

  test("rewrites Rust UI metadata to BFF fallback endpoints", () => {
    const normalized = normalizeDashboardPayload({
      metadata: {
        polling_fallback_endpoint: "/api/dashboard/ui",
        live_events_endpoint: "/api/dashboard/events",
      },
      totals: { running_cost_micros: 1 },
    });

    expect(normalized).toEqual({
      metadata: {
        polling_fallback_endpoint: "/api/dashboard",
        live_events_endpoint: "/api/dashboard/events",
      },
      totals: {},
    });
  });

  test("normalizes JSON data lines in dashboard event streams", () => {
    const normalized = normalizeDashboardEventStream(
      'event: dashboard.snapshot\ndata: {"snapshot":{"totals":{"recorded_cost_micros":8,"running_tokens":3}}}\n\n',
    );

    expect(normalized).toContain("event: dashboard.snapshot");
    expect(normalized).toContain('"running_tokens":3');
    expect(normalized).not.toContain("cost");
  });
});
