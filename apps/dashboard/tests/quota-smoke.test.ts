import { describe, expect, test } from "bun:test";

import { readDashboardConfig } from "@/src/config";
import { readQuota } from "@/src/quota";

// Reads real host quota through the local `ocu` binary; opt in with SYMPHONY_LIVE_OCU_QUOTA=1.
const liveQuotaSmoke = process.env.SYMPHONY_LIVE_OCU_QUOTA === "1";

describe.if(liveQuotaSmoke)("quota command smoke", () => {
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
