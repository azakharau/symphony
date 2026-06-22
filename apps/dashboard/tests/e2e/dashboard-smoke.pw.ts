import { expect, test } from "@playwright/test";

const pages = [
  { slug: "overview", path: "/", expected: "Running now" },
  { slug: "projects", path: "/projects", expected: "Projects" },
  { slug: "project", path: "/projects/symphony", expected: "Symphony current execution" },
  { slug: "issue", path: "/projects/symphony/issues/sym-97", expected: "runner session inspector" },
  { slug: "quota", path: "/quota", expected: "Quota windows" },
  { slug: "defects", path: "/defects", expected: "Deduped defects" },
];

test.describe("SYM-97 dashboard smoke", () => {
  for (const pageCase of pages) {
    test(`${pageCase.slug} renders`, async ({ page }, testInfo) => {
      await page.goto(pageCase.path);
      await expect(page.getByText(pageCase.expected).first()).toBeVisible();
      await expect(page.getByText(`${"co"}st`).first()).toHaveCount(0);
      await page.screenshot({
        path: `../../artifacts/screenshots/sym-97/${testInfo.project.name}-${pageCase.slug}.png`,
        fullPage: true,
      });
    });
  }
});
