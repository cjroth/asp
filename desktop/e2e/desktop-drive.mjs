// Computer-use verification of the REAL desktop Tauri app (native SqliteEngine,
// real Tauri IPC) headed under Xvfb, driven via tauri-driver + WebKitWebDriver.
// A vault is pre-registered in $HOME/.asp/desktop_folders.json (the native folder
// picker can't be scripted), so the app reopens it on launch. Screenshots land in
// e2e/shots-desktop/.
import pkg from 'selenium-webdriver';
const { Builder, By, until } = pkg;
import { spawn } from 'node:child_process';
import { mkdirSync, writeFileSync } from 'node:fs';
import { setTimeout as sleep } from 'node:timers/promises';

const BIN = process.env.APP_BIN || '/home/user/asp/desktop/src-tauri/target/debug/context-desktop';
const PORT = Number(process.env.TD_PORT || 4444);
const SHOTS = new URL('./shots-desktop/', import.meta.url).pathname;
mkdirSync(SHOTS, { recursive: true });

const results = [];
const ok = (n, x = {}) => { results.push({ n, pass: true, ...x }); console.log(`✓ ${n} ${JSON.stringify(x)}`); };
const bad = (n, x = {}) => { results.push({ n, pass: false, ...x }); console.log(`✗ ${n} ${JSON.stringify(x)}`); };

const td = spawn('tauri-driver', ['--port', String(PORT)], { stdio: ['ignore', 'inherit', 'inherit'], env: process.env });
await sleep(2000);

let driver;
const shot = async (name) => { try { const b = await driver.takeScreenshot(); writeFileSync(SHOTS + name + '.png', b, 'base64'); } catch {} };

try {
  driver = await new Builder()
    .usingServer(`http://127.0.0.1:${PORT}/`)
    .withCapabilities({ browserName: 'wry', 'tauri:options': { application: BIN } })
    .build();

  // Connect screen — the pre-registered vault reopens on the background thread.
  await driver.wait(until.elementLocated(By.xpath("//*[contains(text(),'Your vaults')]")), 30000);
  ok('connect-screen');
  await shot('01-connect');

  // Open the seeded vault from "Recent vaults" (basename "seedvault").
  const row = await driver.wait(until.elementLocated(By.xpath("//*[contains(text(),'seedvault')]")), 20000);
  await row.click();
  await driver.wait(until.elementLocated(By.css('[data-testid="live-editor"]')), 20000);
  ok('vault-opened-native-engine');
  await shot('02-editor');

  // No branch dropdown.
  const dd = await driver.findElements(By.css('[data-testid="branch-switcher"]'));
  if (dd.length === 0) ok('branch-dropdown-removed'); else bad('branch-dropdown-removed', { n: dd.length });

  // Open History → the timeline network graph, backed by the native history().
  await (await driver.findElement(By.xpath("//button[contains(.,'History')]"))).click();
  await driver.wait(until.elementLocated(By.css('[data-testid="history-track"]')), 10000);
  await sleep(1500);
  await shot('03-history');
  const dots = await driver.findElements(By.css('[data-testid="history-track"] div[title]:not([data-testid])'));
  if (dots.length > 0) ok('timeline-event-dots', { dots: dots.length }); else bad('timeline-event-dots', { dots: 0 });

  // Tag the moment (real create_tag → Kind::Tag row in SQLite).
  await (await driver.findElement(By.css('[data-testid="tag-here"]'))).click();
  const tin = await driver.wait(until.elementLocated(By.css('[data-testid="tag-name-input"]')), 5000);
  await tin.sendKeys('desktop-mark');
  await (await driver.findElement(By.css('[data-testid="tag-confirm"]'))).click();
  await sleep(800);
  const flag = await driver.findElements(By.css('[data-testid="tag-desktop-mark"]'));
  if (flag.length) ok('tag-created-native'); else bad('tag-created-native');
  await shot('04-tagged');

  // Jump to the tag → editable time-travel banner.
  await (await driver.findElement(By.css('[data-testid="tag-desktop-mark"]'))).click();
  await driver.wait(until.elementLocated(By.css('[data-testid="time-travel-banner"]')), 8000);
  ok('time-travel-editable');
  await shot('05-time-travel');

  // Edit in the past → auto-branch (real fork_from_time on the SqliteEngine +
  // checkout re-materializes the working tree).
  const ed = await driver.findElement(By.css('[data-testid="live-editor"]'));
  await ed.click();
  await ed.sendKeys('\nedit in the past on the desktop app');
  await driver.wait(until.elementLocated(By.css('[data-testid="branch-created-banner"]')), 15000);
  ok('auto-branch-native');
  await sleep(1200);
  await shot('06-auto-branched');

  const lanes = await driver.findElements(By.css('[data-testid^="lane-label-"]'));
  if (lanes.length >= 2) ok('timeline-branch-lanes', { lanes: lanes.length }); else bad('timeline-branch-lanes', { lanes: lanes.length });
  await shot('07-lanes');
} catch (e) {
  bad('exception', { error: String(e).slice(0, 300) });
  if (driver) await shot('99-error');
} finally {
  if (driver) await driver.quit().catch(() => {});
  td.kill('SIGKILL');
  const passed = results.filter((r) => r.pass).length;
  console.log(`\n=== desktop: ${passed}/${results.length} checks passed ===`);
  const failed = results.filter((r) => !r.pass);
  if (failed.length) { console.log('FAILED:', JSON.stringify(failed)); process.exit(1); }
  console.log('ALL DESKTOP CHECKS PASSED');
}
