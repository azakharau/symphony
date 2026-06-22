import { describe, expect, test } from "bun:test";

import { readDashboardConfig } from "@/src/config";
import { readQuota } from "@/src/quota";

describe("quota command smoke", () => {
  const command = `printf '{"buckets":[{"title":"Provider","windows":[{"label":"5h","used_percent":20}]}]}'`;
  test("reads non-colocated local quota command", async () => {
    const result = await readQuota(
      readDashboardConfig({ ...process.env, SYMPHONY_QUOTA_COMMAND: command }),
    );

    expect(result.command).toBe(command);
    expect(result.status).toBe("available");
    if (result.status === "available") {
      expect(result.quota.buckets.length).toBeGreaterThan(0);
    }
  });
});
