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
      "ocu --plain --localhost",
    );

    expect(result).toEqual({
      status: "available",
      command: "ocu --plain --localhost",
      quota: {
        raw: {
          quota: {
            used: "12",
            limit: 100,
            remaining: 88,
            resets_at: "2026-06-20T00:00:00Z",
            model: "local",
          },
        },
        buckets: [],
        used: 12,
        limit: 100,
        remaining: 88,
        resetsAt: "2026-06-20T00:00:00Z",
        model: "local",
      },
    });
  });

  test("parses real ocu --plain bucket windows into first-class subscription data", () => {
    const result = parseQuotaJson(
      JSON.stringify({
        buckets: [
          {
            title: "Main Codex bucket",
            windows: [
              { label: "5h", reset_at: 1781886160, used_percent: 20 },
              { label: "weekly", reset_at: 1782335893, used_percent: 20 },
            ],
          },
        ],
      }),
      "ocu --plain",
    );

    expect(result).toEqual({
      status: "available",
      command: "ocu --plain",
      quota: {
        raw: {
          buckets: [
            {
              title: "Main Codex bucket",
              windows: [
                { label: "5h", reset_at: 1781886160, used_percent: 20 },
                { label: "weekly", reset_at: 1782335893, used_percent: 20 },
              ],
            },
          ],
        },
        buckets: [
          {
            title: "Main Codex bucket",
            windows: [
              {
                label: "5h",
                resetAt: "2026-06-19T16:22:40.000Z",
                resetAtEpoch: 1781886160,
                usedPercent: 20,
                remainingPercent: 80,
              },
              {
                label: "weekly",
                resetAt: "2026-06-24T21:18:13.000Z",
                resetAtEpoch: 1782335893,
                usedPercent: 20,
                remainingPercent: 80,
              },
            ],
          },
        ],
        used: undefined,
        limit: undefined,
        remaining: undefined,
        resetsAt: undefined,
        model: undefined,
      },
    });
  });

  test("returns unavailable state on malformed JSON", () => {
    const result = parseQuotaJson("not json", "ocu --plain --localhost");

    expect(result.status).toBe("unavailable");
    if (result.status === "unavailable") {
      expect(result.reason).toBe("malformed_json");
      expect(result.quota).toBeNull();
    }
  });
});
