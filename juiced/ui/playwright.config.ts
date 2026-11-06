import { defineConfig } from '@playwright/test';
export default defineConfig({
  testDir: './e2e', timeout: 30_000, fullyParallel: true,
  testMatch: process.env.JUICED_LIVE_URL ? '**/native.spec.ts' : ['**/operator.spec.ts', '**/compact.spec.ts', '**/discovery.spec.ts'],
  use: { baseURL: process.env.JUICED_LIVE_URL || 'http://127.0.0.1:5187', trace: 'retain-on-failure', screenshot: 'only-on-failure',
    launchOptions: process.env.JUICED_TEST_CHROMIUM ? { executablePath: process.env.JUICED_TEST_CHROMIUM } : {} },
  reporter: [['list'], ['html', { open: 'never' }]],
  webServer: process.env.JUICED_LIVE_URL ? undefined : { command: 'bun run dev --port 5187 --strictPort', url: 'http://127.0.0.1:5187', reuseExistingServer: false },
});
