import { defineConfig } from "@playwright/test";

export default defineConfig({
  testDir: "./e2e",
  fullyParallel: false,
  workers: 1,
  timeout: 30000,
  use: {
    baseURL: "http://127.0.0.1:13307",
    trace: "retain-on-failure",
    launchOptions: { executablePath: process.env.JUICED_TEST_CHROMIUM },
  },
  projects: [
    { name: "desktop", use: { viewport: { width: 1280, height: 800 } } },
    { name: "tablet", use: { viewport: { width: 820, height: 900 } } },
    { name: "phone", use: { viewport: { width: 390, height: 844 }, isMobile: true, hasTouch: true } },
  ],
  webServer: {
    command: "bun scripts/fixture.ts",
    url: "http://127.0.0.1:13307/v1/health",
    env: { JUICED_PREVIEW_PORT: "13307", JUICED_TEST_ORPHAN_FIXTURE: "1" },
    reuseExistingServer: false,
    timeout: 60000,
    gracefulShutdown: { signal: "SIGTERM", timeout: 5000 },
  },
});
