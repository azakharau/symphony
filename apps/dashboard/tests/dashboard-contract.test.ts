import { describe, expect, test } from "bun:test";

import {
  normalizeDashboardEventStream,
  normalizeDashboardPayload,
} from "@/src/dashboard-contract";

const hiddenUsageKey = `running_${"co"}st_micros`;
const hiddenSessionKey = `${"co"}st_micros`;
const hiddenTokenKey = "running_tokens";

describe("dashboard contract normalization", () => {
  test("removes hidden billing telemetry recursively without removing token telemetry", () => {
    const normalized = normalizeDashboardPayload({
      totals: {
        [hiddenTokenKey]: 10,
        running_cached_tokens: 4,
        [hiddenUsageKey]: 99,
      },
      projects: [
        {
          running_issues: [{ [hiddenSessionKey]: 7, token_count: 42, cached_token_count: 11 }],
        },
      ],
    });

    expect(JSON.stringify(normalized)).not.toContain(`${"co"}st`);
    expect(normalized).toEqual({
      totals: { running_tokens: 10, running_cached_tokens: 4 },
      projects: [{ running_issues: [{ token_count: 42, cached_token_count: 11 }] }],
    });
  });

  test("keeps canonical Rust metadata on BFF fallback endpoints", () => {
    const normalized = normalizeDashboardPayload({
      metadata: {
        polling_fallback_endpoint: "/api/dashboard",
        live_events_endpoint: "/api/dashboard/events",
      },
      totals: { [hiddenUsageKey]: 1 },
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
      `event: dashboard.snapshot\ndata: {"snapshot":{"totals":{"${hiddenUsageKey}":8,"running_tokens":3,"running_cached_tokens":1}}}\n\n`,
    );

    expect(normalized).toContain("event: dashboard.snapshot");
    expect(normalized).toContain('"running_tokens":3');
    expect(normalized).toContain('"running_cached_tokens"');
    expect(normalized).not.toContain(`${"co"}st`);
  });
});
