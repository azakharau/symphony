import { describe, expect, test } from "bun:test";

import { readDashboardConfig } from "@/src/config";
import { readQuota } from "@/src/quota";

describe("quota command smoke", () => {
  test("reads non-colocated local quota command", async () => {
    const result = await readQuota(
      readDashboardConfig({ ...process.env, OCU_COMMAND: "ocu --localhost --plain" }),
    );

    expect(result.command).toBe("ocu --localhost --plain");
    expect(result.status).toBe("available");
    if (result.status === "available") {
      expect(result.quota.buckets.length).toBeGreaterThan(0);
    }
  });
});
