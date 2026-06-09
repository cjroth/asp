// Real-browser end-to-end (Playwright/chromium headless): drives the BUILT demo
// in a real DOM — add/clone nodes, live propagation, offline → reconnect
// catch-up, OPFS persistence across reload, and the wss:// connect UI wiring.
// Requires: `node build.mjs` first, and chromium installed (PLAYWRIGHT_BROWSERS_PATH).
import { spawn } from 'node:child_process';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright';

const here = dirname(fileURLToPath(import.meta.url));
const PORT = 5311;
const URL = `http://127.0.0.1:${PORT}`;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
let fail = 0;
const check = (n, c) => { console.log(`${c ? '✓' : '✗ FAIL'}  ${n}`); if (!c) fail++; };

const server = spawn('node', [resolve(here, '..', 'serve.mjs')], { env: { ...process.env, PORT: String(PORT) }, stdio: 'ignore' });
await sleep(700);

const browser = await chromium.launch({ headless: true });
const page = await browser.newPage();
page.on('pageerror', (e) => console.log('  [pageerror]', e.message));

// node panel by header name
const panel = (name) => page.locator('.node-panel', { has: page.locator('.np-name .nm-txt', { hasText: new RegExp(`^${name}$`) }) });
async function waitValue(locator, expected, ms = 6000) {
  const deadline = Date.now() + ms;
  while (Date.now() < deadline) {
    if ((await locator.inputValue().catch(() => '')) === expected) return true;
    await sleep(100);
  }
  return false;
}
async function waitValueNot(locator, notExpected, ms = 2500) {
  const deadline = Date.now() + ms;
  let last = '';
  while (Date.now() < deadline) { last = await locator.inputValue().catch(() => ''); if (last === notExpected) return false; await sleep(100); }
  return last !== notExpected;
}
// The UI is eventually-consistent: engine state updates ride a snapshot back
// from the worker and land on the next animation frame, so poll rather than
// reading visibility the instant an action's click resolves.
async function waitVisible(locator, ms = 4000) {
  const deadline = Date.now() + ms;
  while (Date.now() < deadline) { if (await locator.isVisible().catch(() => false)) return true; await sleep(100); }
  return false;
}

try {
  // clear any prior OPFS state for a deterministic run
  await page.goto(URL);
  await page.evaluate(async () => { try { const d = await navigator.storage.getDirectory(); await d.removeEntry('asp-demo-state.json'); } catch {} });
  await page.reload();

  // 1) empty state (wait for the worker to start + first paint after reload)
  check('empty-mesh state shown', await waitVisible(page.locator('text=An empty mesh')));

  // 2) create the first node
  await page.getByRole('button', { name: 'Add a new node' }).click();
  await page.getByRole('button', { name: 'Create node' }).click();
  await panel('laptop').waitFor({ timeout: 8000 });
  check('first node created (laptop)', await panel('laptop').isVisible());
  check('genesis vault shows 5 files', (await panel('laptop').locator('.tree-head .t').textContent())?.includes('· 5'));
  const laptopEd = panel('laptop').locator('.ed-area textarea');
  check('README open with real content', (await laptopEd.inputValue()).includes('Shared context'));

  // 3) clone a second node from laptop (in-page)
  await page.getByRole('button', { name: 'Add node' }).click();
  await page.getByRole('button', { name: 'Clone node' }).click();
  await panel('desktop').waitFor({ timeout: 8000 });
  const desktopEd = panel('desktop').locator('.ed-area textarea');
  check('desktop cloned + converged (README content)', await waitValue(desktopEd, await laptopEd.inputValue(), 8000));

  // 4) live propagation: edit laptop → desktop updates
  const edited = '# Vault\n\nedited live in the browser\n';
  await laptopEd.fill(edited);
  check('live edit propagated laptop → desktop', await waitValue(desktopEd, edited, 8000));
  check('both report In sync', await panel('laptop').locator('.status.insync').isVisible() && await panel('desktop').locator('.status.insync').isVisible());

  // 5) offline → edit → reconnect catch-up
  await panel('desktop').getByRole('button', { name: 'Go offline' }).click();
  check('desktop shows Offline', await waitVisible(panel('desktop').locator('.status.offline')));
  const offlineEdit = '# Vault\n\nauthored while desktop was offline\n';
  await laptopEd.fill(offlineEdit);
  check('desktop did NOT receive the edit while offline', await waitValueNot(desktopEd, offlineEdit, 2500));
  await panel('desktop').getByRole('button', { name: 'Reconnect' }).click();
  check('reconnect delivered the missed edit (catch-up)', await waitValue(desktopEd, offlineEdit, 8000));

  // 6) OPFS persistence across reload
  await sleep(700); // let the debounced autosave flush
  await page.reload();
  await panel('laptop').waitFor({ timeout: 8000 });
  check('persisted: both nodes restored after reload',
    await panel('laptop').isVisible() && await panel('desktop').isVisible());
  check('persisted: laptop content survived reload',
    (await panel('laptop').locator('.ed-area textarea').inputValue()) === offlineEdit);

  // 7) wss:// connect UI wiring (no live peer needed)
  await page.getByRole('button', { name: 'Add node' }).click();
  await page.getByRole('button', { name: 'real wss:// peer', exact: true }).click();
  check('Add-node dialog exposes the wss:// peer url field',
    await page.locator('input[placeholder="wss://127.0.0.1:9000"]').isVisible());
  await page.getByRole('button', { name: 'Cancel' }).click();
  await panel('laptop').getByRole('button', { name: 'connect to a real wss:// peer' }).click();
  check('per-node connect dialog opens (Bridge to a real peer)',
    await page.locator('text=Bridge').first().isVisible());
  await page.getByRole('button', { name: 'Cancel' }).click();
} catch (e) {
  console.log('✗ FAIL  exception:', e.message);
  fail++;
} finally {
  await browser.close();
  server.kill();
}

console.log(fail === 0 ? '\nE2E: ALL PASS' : `\nE2E: ${fail} FAILURE(S)`);
process.exit(fail === 0 ? 0 : 1);
