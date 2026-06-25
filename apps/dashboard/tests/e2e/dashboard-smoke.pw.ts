import { expect, test } from "@playwright/test";

const fixtureState = process.env.DASHBOARD_FIXTURE_STATE ?? "acceptance";

const pages = [
  { slug: "overview", path: "/", expected: "Running now" },
  { slug: "projects", path: "/projects", expected: "Projects" },
  { slug: "project", path: "/projects/symphony", expected: "Symphony current execution" },
  { slug: "issue", path: "/projects/symphony/issues/sym-97", expected: "runner session inspector" },
  { slug: "quota", path: "/quota", expected: "Quota windows" },
  { slug: "defects", path: "/defects", expected: "Deduped defects" },
];

test.describe("SYM-122 dashboard smoke", () => {
  test.skip(fixtureState !== "acceptance", "full surface smoke uses acceptance fixture data");

  for (const pageCase of pages) {
    test(`${pageCase.slug} renders`, async ({ page }, testInfo) => {
      await page.goto(pageCase.path);
      await expect(page.getByText(pageCase.expected).first()).toBeVisible();
      await expect(page.getByText(`${"co"}st`).first()).toHaveCount(0);
      await page.screenshot({
        path: `../../artifacts/screenshots/sym-122/${testInfo.project.name}-${pageCase.slug}.png`,
        fullPage: true,
      });
    });
  }

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
      path: `../../artifacts/screenshots/sym-122/${testInfo.project.name}-overview-operations.png`,
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
      path: `../../artifacts/screenshots/sym-123/${testInfo.project.name}-project-active.png`,
      fullPage: true,
    });

    await page.goto("/");
    await page.getByRole("link", { name: "Atlas" }).first().click();
    await expect(page.getByRole("heading", { name: "Atlas current execution" })).toBeVisible();
    await expect(page.getByText("No live execution is currently reported for this project")).toBeVisible();
    await expect(page.getByText("runtime process exit").first()).toBeVisible();
    await expect(page.getByText("restart supervised runner").first()).toBeVisible();
    await page.screenshot({
      path: `../../artifacts/screenshots/sym-123/${testInfo.project.name}-project-blocked-idle.png`,
      fullPage: true,
    });
  });
});

test.describe("SYM-122 empty overview", () => {
  test.skip(fixtureState !== "empty", "empty state requires DASHBOARD_FIXTURE_STATE=empty");

  test("overview explains no running work", async ({ page }, testInfo) => {
    await page.goto("/");
    await expect(page.getByRole("heading", { name: "Running now" })).toBeVisible();
    await expect(page.getByText("No runner sessions are running")).toBeVisible();
    await expect(page.getByText("waiting for eligible issues")).toBeVisible();
    await expect(page.getByRole("link", { name: "5h quota unavailable" })).toHaveAttribute("href", "/quota");
    await expect(page.getByText(`${"co"}st`).first()).toHaveCount(0);
    await page.screenshot({
      path: `../../artifacts/screenshots/sym-122/${testInfo.project.name}-overview-empty.png`,
      fullPage: true,
    });
  });
});
