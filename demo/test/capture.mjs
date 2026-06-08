// Capture a screenshot of a live 3-node mesh for the README / handoff.
import { spawn } from 'node:child_process';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright';

const here = dirname(fileURLToPath(import.meta.url));
const PORT = 5312;
const URL = `http://127.0.0.1:${PORT}`;
const out = process.argv[2] || '/tmp/asp-demo.png';
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const server = spawn('node', [resolve(here, '..', 'serve.mjs')], { env: { ...process.env, PORT: String(PORT) }, stdio: 'ignore' });
await sleep(700);
const browser = await chromium.launch({ headless: true });
const page = await browser.newPage({ viewport: { width: 1480, height: 920 }, deviceScaleFactor: 2 });
try {
  await page.goto(URL);
  await page.evaluate(async () => { try { const d = await navigator.storage.getDirectory(); await d.removeEntry('asp-demo-state.json'); } catch {} });
  await page.reload();
  const panel = (name) => page.locator('.node-panel', { has: page.locator('.np-name .nm-txt', { hasText: new RegExp(`^${name}$`) }) });

  await page.getByRole('button', { name: 'Add a new node' }).click();
  await page.getByRole('button', { name: 'Create node' }).click();
  await panel('laptop').waitFor();
  for (let i = 0; i < 2; i++) {
    await page.getByRole('button', { name: 'Add node' }).click();
    await page.getByRole('button', { name: 'Clone node' }).click();
    await sleep(900);
  }
  await panel('studio').waitFor();
  await sleep(800);
  // a live edit to show propagation + a packet on the map
  await panel('laptop').locator('.ed-area textarea').fill('# Vault\n\nlive across three nodes — no commit, no push\n');
  await sleep(500);
  await page.screenshot({ path: out });
  console.log('screenshot →', out);
} finally {
  await browser.close();
  server.kill();
}
