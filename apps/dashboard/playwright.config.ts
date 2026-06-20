import { tmpdir } from "node:os";
import { join } from "node:path";

import { defineConfig, devices } from "@playwright/test";

export default defineConfig({
  testDir: "./tests/e2e",
  testMatch: "**/*.pw.ts",
  timeout: 30_000,
  expect: { timeout: 10_000 },
  outputDir: process.env.PLAYWRIGHT_OUTPUT_DIR ?? join(tmpdir(), "symphony-dashboard-playwright", "sym-97"),
  use: {
    baseURL: "http://127.0.0.1:32097",
    trace: "retain-on-failure",
  },
  projects: [
    {
      name: "desktop",
      use: { ...devices["Desktop Chrome"], viewport: { width: 1440, height: 1000 } },
    },
    {
      name: "mobile",
      use: { ...devices["Pixel 5"], viewport: { width: 390, height: 844 } },
    },
  ],
  webServer: {
    command: 'DASHBOARD_FIXTURE_STATE=acceptance OCU_COMMAND="ocu --localhost --plain" bun run start --hostname 127.0.0.1 --port 32097',
    url: "http://127.0.0.1:32097",
    reuseExistingServer: !process.env.CI,
    timeout: 120_000,
  },
});
