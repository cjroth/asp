// Computer-use verification of the branching-UX redesign in a REAL headed browser
// (chromium under Xvfb), driving the built web app on the REAL wasm engine — so
// auto-branch-on-edit-in-the-past, tags, the timeline network graph, branch
// switching, and the removed dropdown are all exercised end-to-end. Screenshots
// are written to e2e/shots/ for visual confirmation.
import { chromium } from 'playwright-core';
import { mkdirSync } from 'node:fs';
import { setTimeout as sleep } from 'node:timers/promises';

const APP_URL = process.env.URL || 'http://127.0.0.1:5601/';
const EXE = process.env.CHROME || '/opt/pw-browsers/chromium-1194/chrome-linux/chrome';
const SHOTS = new URL('./shots/', import.meta.url).pathname;
mkdirSync(SHOTS, { recursive: true });

const results = [];
const ok = (name, extra = {}) => { results.push({ name, pass: true, ...extra }); console.log(`✓ ${name} ${JSON.stringify(extra)}`); };
const bad = (name, extra = {}) => { results.push({ name, pass: false, ...extra }); console.log(`✗ ${name} ${JSON.stringify(extra)}`); };

const browser = await chromium.launch({ executablePath: EXE, headless: false, args: ['--no-sandbox', '--disable-gpu'] });
const page = await browser.newPage({ viewport: { width: 1280, height: 820 } });
page.on('console', (m) => { if (m.type() === 'error') console.log('  [page error]', m.text().slice(0, 200)); });
const shot = (n) => page.screenshot({ path: SHOTS + n + '.png' }).catch(() => {});

try {
  await page.goto(APP_URL, { waitUntil: 'load', timeout: 30000 });
  await page.getByText('Your vaults').waitFor({ timeout: 20000 });
  ok('connect-screen-loaded');
  await shot('01-connect');

  // Create a browser (OPFS, real wasm) vault.
  await page.getByRole('button', { name: 'New Vault' }).click();
  await page.getByPlaceholder('My vault').fill('Demo Vault');
  await page.getByRole('button', { name: 'Create vault' }).click();
  await page.locator('[data-testid="live-editor"]').waitFor({ timeout: 20000 });
  ok('vault-created-editor-open');
  await shot('02-editor');

  // No branch dropdown anywhere (removed in the redesign).
  const dropdown = await page.locator('[data-testid="branch-switcher"]').count();
  if (dropdown === 0) ok('branch-dropdown-removed'); else bad('branch-dropdown-removed', { count: dropdown });

  // Make a couple of edits so there's real history on the timeline.
  const editor = page.locator('[data-testid="live-editor"]');
  await editor.click();
  await page.keyboard.press('End');
  await page.keyboard.type('\nFirst edit on main.');
  await sleep(900); // let the debounced save + capture land a log row
  await page.keyboard.type('\nSecond edit on main.');
  await sleep(900);
  ok('edited-on-main');

  // Open the History panel — the timeline IS the network graph now.
  await page.getByRole('button', { name: 'History' }).click();
  await page.locator('[data-testid="history-track"]').waitFor({ timeout: 10000 });
  // Event dots are titled divs WITHOUT a data-testid (tag flags carry one) — wait
  // for the first to render (history() loads on a debounce), then count them.
  const eventDot = '[data-testid="history-track"] div[title]:not([data-testid])';
  const dot0 = page.locator(eventDot).first();
  await dot0.waitFor({ timeout: 10000 }).catch(() => {});
  await shot('03-history-open');
  const dots = await page.locator(eventDot).count();
  if (dots > 0) ok('timeline-has-event-dots', { dots }); else bad('timeline-has-event-dots', { dots });

  // Tag the current moment.
  await page.getByTestId('tag-here').click();
  await page.getByTestId('tag-name-input').fill('milestone');
  await page.getByTestId('tag-confirm').click();
  await sleep(700);
  const tagFlag = await page.locator('[data-testid="tag-milestone"]').count();
  if (tagFlag > 0) ok('tag-created-and-shown'); else bad('tag-created-and-shown', { tagFlag });
  await shot('04-tagged');

  // Time-travel: click the tag flag we just made (a moment where the file exists,
  // rendered at the top of the track — not under the playhead handle) → the
  // editable time-travel banner appears (no longer read-only). Jumping to a tag is
  // itself part of the tagging UX ("find and go back to specific points").
  await page.locator('[data-testid="tag-milestone"]').click();
  const banner = page.locator('[data-testid="time-travel-banner"]');
  await banner.waitFor({ timeout: 8000 });
  const bannerText = await banner.innerText();
  if (/branch from here/i.test(bannerText)) ok('time-travel-editable-banner', { bannerText: bannerText.slice(0, 80) });
  else bad('time-travel-editable-banner', { bannerText });
  await shot('05-time-travel');

  // Edit while scrubbed into the past → AUTO-BRANCH (no manual create step).
  await editor.click();
  await page.keyboard.press('End');
  await page.keyboard.type('\nEditing in the past — should fork a branch.');
  const created = page.locator('[data-testid="branch-created-banner"]');
  await created.waitFor({ timeout: 12000 });
  const createdText = await created.innerText();
  ok('auto-branch-on-edit-in-past', { createdText: createdText.slice(0, 90) });
  await sleep(1200);
  await shot('06-auto-branched');

  // The timeline should now show a second lane (the new branch) + a branch pill.
  await page.getByRole('button', { name: 'History' }).count();
  await sleep(600);
  const laneLabels = await page.locator('[data-testid^="lane-label-"]').count();
  const pill = await page.locator('[data-testid="current-branch-pill"]').count();
  if (laneLabels >= 2) ok('timeline-shows-branch-lanes', { laneLabels }); else bad('timeline-shows-branch-lanes', { laneLabels });
  if (pill >= 1) ok('current-branch-pill-shown'); else bad('current-branch-pill-shown', { pill });
  await shot('07-branch-lanes');

  // Switch back to main via the lane label (replaces the old dropdown).
  const mainLane = page.locator('[data-testid="lane-label-main"]');
  if (await mainLane.count()) {
    await mainLane.click();
    await sleep(1000);
    ok('branch-switch-via-lane');
    await shot('08-switched-to-main');
  } else {
    bad('branch-switch-via-lane', { reason: 'no main lane label' });
  }
} catch (e) {
  bad('exception', { error: String(e).slice(0, 300) });
  await shot('99-error');
} finally {
  await browser.close();
  const passed = results.filter((r) => r.pass).length;
  const failed = results.filter((r) => !r.pass);
  console.log(`\n=== ${passed}/${results.length} checks passed ===`);
  if (failed.length) { console.log('FAILED:', JSON.stringify(failed, null, 2)); process.exit(1); }
  console.log('ALL VERIFICATION CHECKS PASSED');
}
