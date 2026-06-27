import { defineConfig } from '@playwright/test';

// Playwright config for the desktop web-target e2e. Serves the built `dist/`
// (vite build) on a port and drives the real editor UI in headless Chromium
// against the real wasm engine (iroh-in-wasm) + OPFS. The native sync path is
// covered by the Rust e2e suite; here we prove the browser surface end-to-end.
export default defineConfig({
  testDir: './e2e',
  timeout: 90_000,
  expect: { timeout: 15_000 },
  fullyParallel: false,
  retries: 0,
  workers: 1,
  reporter: 'list',
  use: {
    headless: true,
    viewport: { width: 1280, height: 900 },
    baseURL: 'http://127.0.0.1:4173',
    trace: 'retain-on-failure',
    launchOptions: {
      args: ['--no-sandbox', '--enable-features=SharedArrayBuffer'],
    },
  },
  webServer: {
    command: 'bun run build:web && bunx vite preview --port 4173 --strictPort --host 127.0.0.1',
    port: 4173,
    reuseExistingServer: false,
    timeout: 120_000,
  },
});
