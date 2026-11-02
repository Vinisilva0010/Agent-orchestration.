import { defineConfig, devices } from "@playwright/test";

/**
 * End-to-end tests against the whole workspace started by
 * scripts/e2e-stack.sh (mock model provider, throwaway database). Run them
 * with scripts/e2e.sh, which also provides the browser.
 */
export default defineConfig({
  testDir: "./e2e",
  testMatch: /.*\.e2e\.ts$/,
  outputDir: process.env.E2E_OUTPUT_DIR ?? "test-results",
  fullyParallel: false,
  workers: 1,
  retries: 0,
  timeout: 60_000,
  expect: { timeout: 20_000 },
  reporter: [["list"]],
  use: {
    baseURL: process.env.E2E_BASE_URL ?? "http://localhost:3100",
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
});
