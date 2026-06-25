import { describe, expect, test } from "bun:test";

import { parseQuotaJson } from "@/src/quota";

describe("quota parsing", () => {
  test("parses common quota JSON into typed quota data", () => {
    const result = parseQuotaJson(
      JSON.stringify({
        quota: {
          used: "12",
          limit: 100,
          remaining: 88,
          resets_at: "2026-06-20T00:00:00Z",
          model: "local",
        },
      }),
      "ocu --localhost --plain",
    );

    expect(result.status).toBe("available");
    expect(result.command).toBe("ocu --localhost --plain");
    expect(result.fetchedAt).toBeTruthy();
    expect(result.parsedAt).toBeTruthy();
    expect(result.source).toBe("ocu");
    expect(result.parseHealth).toBe("ok");
    expect(result.sourceHealth).toBe("ok");
    if (result.status === "available") {
      expect(result.quota.used).toBe(12);
      expect(result.quota.limit).toBe(100);
      expect(result.quota.remaining).toBe(88);
      expect(result.quota.resetsAt).toBe("2026-06-20T00:00:00Z");
      expect(result.quota.model).toBe("local");
    }
  });

  test("parses real ocu --localhost --plain bucket windows into general and model quota data", () => {
    const result = parseQuotaJson(
      JSON.stringify({
        buckets: [
          {
            title: "Main Codex bucket",
            windows: [
              { label: "5h", reset_at: 1782421834, used_percent: 2 },
              { label: "weekly", reset_at: 1782726776, used_percent: 10 },
            ],
          },
          {
            title: "Codex 5.3 Spark",
            windows: [
              { label: "5h", reset_at: 1782424264, used_percent: 0 },
              { label: "weekly", reset_at: 1783011064, used_percent: 0 },
            ],
          },
        ],
      }),
      "ocu --localhost --plain",
    );

    expect(result.status).toBe("available");
    if (result.status === "available") {
      expect(result.command).toBe("ocu --localhost --plain");
      expect(result.source).toBe("ocu");
      expect(result.parseHealth).toBe("ok");
      expect(result.sourceHealth).toBe("ok");
      expect(result.quota.buckets).toEqual([
        {
          title: "Main Codex bucket",
          scope: "general",
          windows: [
            {
              label: "5h",
              resetAt: "2026-06-25T21:10:34.000Z",
              resetAtEpoch: 1782421834,
              usedPercent: 2,
              remainingPercent: 98,
            },
            {
              label: "weekly",
              resetAt: "2026-06-29T09:52:56.000Z",
              resetAtEpoch: 1782726776,
              usedPercent: 10,
              remainingPercent: 90,
            },
          ],
        },
        {
          title: "Codex 5.3 Spark",
          scope: "model",
          windows: [
            {
              label: "5h",
              resetAt: "2026-06-25T21:51:04.000Z",
              resetAtEpoch: 1782424264,
              usedPercent: 0,
              remainingPercent: 100,
            },
            {
              label: "weekly",
              resetAt: "2026-07-02T16:51:04.000Z",
              resetAtEpoch: 1783011064,
              usedPercent: 0,
              remainingPercent: 100,
            },
          ],
        },
      ]);
    }
  });

  test("returns unavailable state on malformed JSON", () => {
    const result = parseQuotaJson("not json", "ocu --localhost --plain");

    expect(result.status).toBe("unavailable");
    if (result.status === "unavailable") {
      expect(result.reason).toBe("malformed_json");
      expect(result.quota).toBeNull();
    }
  });
});
