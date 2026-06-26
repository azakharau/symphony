import { expect, test } from "@playwright/test";

const fixtureState = process.env.DASHBOARD_FIXTURE_STATE ?? "acceptance";

const pages = [
  { slug: "overview", path: "/", expected: "Running now" },
  { slug: "projects", path: "/projects", expected: "Projects" },
  { slug: "project", path: "/projects/symphony", expected: "Symphony current execution" },
  { slug: "issue", path: "/projects/symphony/issues/sym-97", expected: "Current runner status" },
  { slug: "quota", path: "/quota", expected: "Quota windows" },
  { slug: "defects", path: "/defects", expected: "Deduped defects" },
];

test.describe("SYM-126 dashboard responsive smoke", () => {
  test.skip(fixtureState !== "acceptance", "full surface smoke uses acceptance fixture data");

  for (const pageCase of pages) {
    test(`${pageCase.slug} renders`, async ({ page }, testInfo) => {
      await page.goto(pageCase.path);
      await expect(page.getByText(pageCase.expected).first()).toBeVisible();
      await expect(page.getByText(`${"co"}st`).first()).toHaveCount(0);
      await page.screenshot({
        path: `../../artifacts/screenshots/sym-126/${testInfo.project.name}-${pageCase.slug}.png`,
        fullPage: true,
      });
    });
  }

  test("projects hides raw last-event fields", async ({ page }, testInfo) => {
    await page.goto("/projects");
    await expect(page.getByRole("heading", { name: "Projects" })).toBeVisible();
    await expect(page.locator(".project-table")).toBeVisible();
    await expect(page.getByText(/last event/i)).toHaveCount(0);

    const bodyText = (await page.locator("body").textContent()) ?? "";
    expect(bodyText).not.toMatch(/linear terminal reconciled/i);
    expect(bodyText).not.toMatch(/linear_terminal_reconciled/i);
    expect(bodyText).not.toMatch(/omp_jsonl_updated/i);

    const mobileLabels = await page.locator(".project-table td").evaluateAll((cells) =>
      cells.map((cell) => getComputedStyle(cell, "::before").content.replace(/^"|"$/g, "")),
    );
    expect(mobileLabels).not.toContain("Last event");

    await page.screenshot({
      path: `/tmp/symphony-dashboard-acceptance-20260626/projects-${testInfo.project.name}.png`,
      fullPage: true,
    });
  });

  test("mobile routes render list-first responsive tables and favicon", async ({ page, request }, testInfo) => {
    test.skip(testInfo.project.name !== "mobile", "responsive table smoke only runs on the mobile project");

    const favicon = await request.get("/favicon.svg");
    expect(favicon.status()).toBe(200);
    await page.goto("/");
    await expect(page.getByRole("heading", { name: "Running now" })).toBeVisible();
    await expect(page.locator(".running-table tr").first()).toHaveCSS("display", "block");
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);

    await page.goto("/projects/symphony");
    await expect(page.getByRole("heading", { name: "Symphony current execution" })).toBeVisible();
    await expect(page.locator(".issue-table tr").first()).toHaveCSS("display", "block");
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);

    await page.goto("/projects/symphony/issues/sym-97");
    await expect(page.getByRole("heading", { name: "Current runner status" })).toBeVisible();
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);

    await page.goto("/defects");
    await expect(page.getByRole("heading", { name: "Deduped defects" })).toBeVisible();
    await expect(page.locator(".defect-table tr").first()).toHaveCSS("display", "block");
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
  });

  test("overview shows running and blocked operations", async ({ page }, testInfo) => {
    await page.goto("/");
    await expect(page.getByRole("heading", { name: "Running now" })).toBeVisible();
    await expect(page.getByRole("link", { name: "SYM-97" })).toBeVisible();
    await expect(page.getByText("Build dashboard surfaces")).toBeVisible();
    await expect(page.getByRole("link", { name: /5h quota 76% remaining/ })).toHaveAttribute("href", "/quota");
    await expect(page.getByRole("heading", { name: "Project health and capacity" })).toBeVisible();
    await expect(page.getByRole("heading", { name: "Blockers and idle reasons" })).toBeVisible();
    await expect(page.getByRole("link", { name: "Atlas" }).last()).toBeVisible();
    await expect(page.getByText("waiting for quota reset").last()).toBeVisible();
    await page.screenshot({
      path: `../../artifacts/screenshots/sym-126/${testInfo.project.name}-overview-operations.png`,
      fullPage: true,
    });
  });

  test("overview drills into active and blocked projects", async ({ page }, testInfo) => {
    await page.goto("/");
    await page.getByRole("link", { name: "Symphony" }).first().click();
    await expect(page.getByRole("heading", { name: "Symphony current execution" })).toBeVisible();
    await expect(page.getByRole("link", { name: "SYM-97" }).first()).toBeVisible();
    await expect(page.getByText("next eligible").first()).toBeVisible();
    await expect(page.getByText("provider quota exhausted").first()).toBeVisible();
    await expect(page.getByText("Showing newest 5")).toHaveCount(0);
    await page.screenshot({
      path: `../../artifacts/screenshots/sym-126/${testInfo.project.name}-project-active.png`,
      fullPage: true,
    });

    await page.goto("/");
    await page.getByRole("link", { name: "Atlas" }).first().click();
    await expect(page.getByRole("heading", { name: "Atlas current execution" })).toBeVisible();
    await expect(page.getByText("No live execution is currently reported for this project")).toBeVisible();
    await expect(page.getByText("runtime process exit").first()).toBeVisible();
    await expect(page.getByText("restart supervised runner").first()).toBeVisible();
    await page.screenshot({
      path: `../../artifacts/screenshots/sym-126/${testInfo.project.name}-project-blocked-idle.png`,
      fullPage: true,
    });
  });

  test("overview project issue link opens running issue execution drilldown", async ({ page }, testInfo) => {
    await page.goto("/");
    await page.getByRole("link", { name: "Symphony" }).first().click();
    await expect(page.getByRole("heading", { name: "Symphony current execution" })).toBeVisible();
    await page.getByRole("link", { name: "SYM-97" }).first().click();
    await expect(page).toHaveURL(/\/projects\/symphony\/issues\/sym-97$/);
    await expect(page.getByRole("heading", { level: 2, name: "Build dashboard surfaces" })).toBeVisible();
    await expect(page.getByRole("heading", { name: "Current runner status" })).toBeVisible();
    await expect(page.getByRole("heading", { name: "Lifecycle timeline" })).toBeVisible();
    await expect(page.getByText("stage review").first()).toBeVisible();
    await expect(page.getByText("Desktop and mobile route coverage in progress.").first()).toBeVisible();
    await expect(page.getByRole("heading", { name: "OMP workers" })).toBeVisible();
    await expect(page.getByRole("heading", { name: "Tool activity" })).toBeVisible();
    await expect(page.getByRole("heading", { name: "Eval state" })).toBeVisible();
    await expect(page.getByRole("heading", { name: "Git and worktree" })).toBeVisible();
    await expect(page.getByText("Last event:")).toHaveCount(0);
    await expect(page.getByText("Raw issue JSON")).toBeVisible();
    await page.screenshot({
      path: `../../artifacts/screenshots/sym-130/${testInfo.project.name}-issue-running.png`,
      fullPage: true,
    });
  });
});

test.describe("SYM-126 empty overview", () => {
  test.skip(fixtureState !== "empty", "empty state requires DASHBOARD_FIXTURE_STATE=empty");

  test("overview explains no running work", async ({ page }, testInfo) => {
    await page.goto("/");
    await expect(page.getByRole("heading", { name: "Running now" })).toBeVisible();
    await expect(page.getByText("No runner sessions are running")).toBeVisible();
    await expect(page.getByText("waiting for eligible issues")).toBeVisible();
    await expect(page.getByRole("link", { name: "5h quota unavailable" })).toHaveAttribute("href", "/quota");
    await expect(page.getByText(`${"co"}st`).first()).toHaveCount(0);
    await page.screenshot({
      path: `../../artifacts/screenshots/sym-126/${testInfo.project.name}-overview-empty.png`,
      fullPage: true,
    });
  });
});
