import { defineConfig } from "@playwright/test";

const port = process.env.JUICED_E2E_PORT ?? "13307";

export default defineConfig({
  testDir: "./e2e",
  fullyParallel: false,
  workers: 1,
  timeout: 30000,
  use: {
    baseURL: `http://127.0.0.1:${port}`,
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
    url: `http://127.0.0.1:${port}/v1/health`,
    env: { JUICED_PREVIEW_PORT: port, JUICED_SOURCE_PORT: process.env.JUICED_SOURCE_PORT ?? "13308", JUICED_TEST_ORPHAN_FIXTURE: "1" },
    reuseExistingServer: false,
    timeout: 60000,
    gracefulShutdown: { signal: "SIGTERM", timeout: 5000 },
  },
});
