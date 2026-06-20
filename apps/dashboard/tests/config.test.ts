import { describe, expect, test } from "bun:test";

import { readDashboardConfig } from "@/src/config";

describe("dashboard config", () => {
  test("polls dashboard data every 1000ms by default", () => {
    expect(readDashboardConfig({ ...process.env }).refreshMs).toBe(1000);
  });

  test("allows an explicit positive refresh override", () => {
    expect(readDashboardConfig({ ...process.env, NEXT_PUBLIC_DASHBOARD_REFRESH_MS: "750" }).refreshMs).toBe(750);
  });
});
