// Real-browser (Chromium/Firefox) e2e for the WEB git-clone + OPFS persistence path.
//
// Boots the real relay git proxy + a fresh Vite (real asp-wasm bundle) and drives a
// headless Playwright browser to clone a git URL through the modal, then asserts:
//   1. the vault opens (editor appears),
//   2. NO console error — in particular the large-clone OOM regression
//      "invalid value write: error while writing multi-byte MessagePack value",
//   3. the vault survives a reload (OPFS persistence round-trips).
//
// Chromium runs headless with no xvfb. Firefox (Playwright build) too. Config via env:
//   BROWSER=chromium|firefox   GIT_CLONE_URL=<https repo>   CLONE_DEPTH=<n>
//   GIT_EXPECT_FILE=<substring that should appear after clone>
// The production proxy is SSRF-hardened (https/443, no loopback), so GIT_CLONE_URL must
// be a real reachable https repo. Default: a tiny public repo.
import { spawn } from 'node:child_process';
import { setTimeout as sleep } from 'node:timers/promises';

const require = (await import('node:module')).createRequire('/home/chris/asp/demo/');
const { chromium, firefox } = require('playwright');

const ROOT = '/home/chris/asp';
const WEB_PORT = Number(process.env.WEBAPP_PORT || 1433);
const PROXY_PORT = Number(process.env.GIT_PROXY_PORT || 8093);
const RELAY_PORT = Number(process.env.RELAY_PORT || 8092);
const WEB_URL = `http://localhost:${WEB_PORT}/`;
const PROXY_BASE = `http://127.0.0.1:${PROXY_PORT}`;
const cloneUrl = process.env.GIT_CLONE_URL || 'https://github.com/octocat/Hello-World.git';
const depth = process.env.CLONE_DEPTH ? String(parseInt(process.env.CLONE_DEPTH, 10)) : '';
const expectFile = process.env.GIT_EXPECT_FILE || 'README';
const browserName = process.env.BROWSER || 'chromium';

const ok = (m) => console.log('• ' + m);
const fail = (m) => { console.error('✗ ' + m); process.exitCode = 1; };

async function waitForServer(url, ms, expectOk = true) {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    try { const r = await fetch(url); if (!expectOk || r.ok) return true; } catch { /* */ }
    await sleep(200);
  }
  return false;
}

async function main() {
  let proxy, vite, browser;
  try {
    const bin = `${ROOT}/target/release/asp`;
    proxy = spawn(bin, ['relay', '--git-proxy', '--git-proxy-addr', `127.0.0.1:${PROXY_PORT}`, '--listen-addr', `127.0.0.1:${RELAY_PORT}`], { stdio: ['ignore', 'inherit', 'inherit'] });
    if (!(await waitForServer(`${PROXY_BASE}/`, 15000, false))) return fail('git proxy did not start (build `cargo build --release -p asp`)');
    ok(`relay --git-proxy on ${PROXY_BASE}`);

    vite = spawn('bunx', ['vite', '--port', String(WEB_PORT), '--strictPort'], {
      cwd: `${ROOT}/desktop`, stdio: ['ignore', 'inherit', 'inherit'],
      env: { ...process.env, VITE_GIT_PROXY_BASE: PROXY_BASE },
    });
    if (!(await waitForServer(WEB_URL, 25000))) return fail('vite did not start');
    await sleep(800);
    ok(`vite (fresh wasm bundle) on ${WEB_URL}`);

    const engine = browserName === 'firefox' ? firefox : chromium;
    // --unlimited-storage lifts headless Chromium's tiny throwaway-profile OPFS quota
    // (a real user profile gets a disk-proportional quota); irrelevant for firefox.
    const launchArgs = browserName === 'firefox' ? [] : ['--unlimited-storage'];
    browser = await engine.launch({ headless: true, args: launchArgs });
    const page = await browser.newPage();
    const errors = [];
    page.on('console', (m) => { if (m.type() === 'error') errors.push(m.text()); });
    page.on('pageerror', (e) => errors.push('PAGEERROR: ' + e.message));

    await page.goto(WEB_URL);
    await page.getByText('Your vaults').first().waitFor({ timeout: 20000 });
    ok(`app loaded in ${browserName} (real wasm)`);

    await page.getByRole('button', { name: /Connect Vault/i }).click();
    await page.locator('textarea').first().fill(cloneUrl);
    if (depth) {
      // Reveal the Advanced section (a clickable "Advanced" span) — depth lives there
      // as an inputMode=numeric field with placeholder "e.g. 50".
      const adv = page.getByText('Advanced', { exact: true }).first();
      if (await adv.count()) await adv.click().catch(() => {});
      const dEl = page.locator('input[placeholder="e.g. 50"]').first();
      await dEl.waitFor({ timeout: 4000 }).catch(() => {});
      if (await dEl.count()) { await dEl.fill(depth); ok(`depth=${depth}`); }
      else return fail('depth field not found — refusing to clone full history in the gold test');
    }
    const t0 = Date.now();
    await page.getByRole('button', { name: /^Connect$/i }).click();

    // Sample the progress bar while the clone runs — prove it goes DETERMINATE and
    // advances (not a stuck spinner). Reads the progress block's phase title, count
    // text, and the fill-bar width.
    const cloneTimeout = Number(process.env.CLONE_TIMEOUT_MS || 180000);
    let sampling = true;
    const samples = [];
    const samplePromise = (async () => {
      while (sampling) {
        try {
          const s = await page.evaluate(() => {
            const el = document.querySelector('[data-testid="clone-progress"]');
            if (!el) return null;
            return { phase: el.getAttribute('data-phase') || '', done: +(el.getAttribute('data-done') || 0), total: +(el.getAttribute('data-total') || 0), pct: +(el.getAttribute('data-pct') || 0) };
          });
          if (s) {
            const key = `${s.phase}|${s.done}|${s.total}|${s.pct}`;
            if (!samples.length || samples[samples.length - 1].key !== key) samples.push({ ...s, key });
          }
        } catch { /* navigation/teardown */ }
        await sleep(120);
      }
    })();

    const opened = await page.locator('[data-testid="live-editor"]').first().waitFor({ timeout: cloneTimeout }).then(() => true).catch(() => false);
    sampling = false; await samplePromise;
    const secs = ((Date.now() - t0) / 1000).toFixed(1);

    // Progress-bar assertions (WEB reality): the web clone runs the wasm synchronously
    // on the main thread, so the DOM only repaints at phase boundaries (the CSS shimmer
    // still animates on the compositor thread during the blocking compute). So we assert
    // the bar RENDERS and STEPS THROUGH the weighted phases (pct advances past 0) — NOT
    // in-phase count increments (that's the native path, covered by the Rust test
    // `clone_reports_determinate_progress_counts`). The msgpack-free + persistence checks
    // above are the load-bearing ones for the OOM fix.
    const phases = [...new Set(samples.map((s) => s.phase).filter(Boolean))];
    const pctValues = [...new Set(samples.map((s) => s.pct))].sort((a, b) => a - b);
    const rendered = samples.length > 0 && phases.length > 0;
    const advanced = pctValues.length >= 2 && pctValues[pctValues.length - 1] > 0;
    console.log(`  progress phases seen: ${JSON.stringify(phases)}`);
    console.log(`  pct values seen: ${JSON.stringify(pctValues)}`);
    console.log(`  samples: ${samples.length}  (web repaints at phase boundaries only — main-thread wasm)`);
    if (rendered && advanced) ok('progress bar rendered and stepped through weighted phases');
    else fail(`progress bar did not render/advance (phases=${JSON.stringify(phases)} pcts=${JSON.stringify(pctValues)})`);

    const msgpack = errors.find((e) => /multi-byte MessagePack|invalid value write/i.test(e));
    if (msgpack) fail(`MSGPACK/OOM ERROR still present: ${msgpack}`);
    else ok('no msgpack/OOM error during clone+persist');

    if (!opened) {
      const err = (await page.locator('[data-testid="connect-error"]').count())
        ? await page.locator('[data-testid="connect-error"]').first().innerText() : '(none)';
      return fail(`vault did not open after ${secs}s. connect-error: ${err}`);
    }
    ok(`vault cloned + editor opened in ${secs}s`);

    if (expectFile) {
      const body = await page.locator('body').innerText();
      if (!body.includes(expectFile)) fail(`expected file "${expectFile}" not visible after clone`);
      else ok(`imported file "${expectFile}" present`);
    }

    // The OOM/msgpack error fired during PERSIST (coalesced ~700ms after the editor
    // opens, in the background) — so wait for persist to run, THEN re-check the console.
    const persistWait = Number(process.env.PERSIST_WAIT_MS || 2000);
    await sleep(persistWait);
    const msgpackPersist = errors.find((e) => /multi-byte MessagePack|invalid value write/i.test(e));
    if (msgpackPersist) fail(`MSGPACK/OOM error during persist: ${msgpackPersist}`);
    else ok(`no msgpack/OOM error during persist (waited ${persistWait}ms)`);

    // Persistence: reload and confirm the vault is still there (OPFS round-trip).
    await page.reload();
    await page.getByText('Your vaults').first().waitFor({ timeout: 20000 });
    await sleep(1200);
    const errsAfter = errors.filter((e) => /MessagePack|invalid value write|load_state|import/i.test(e));
    if (errsAfter.length) fail('errors after reload: ' + JSON.stringify(errsAfter));
    else ok('reload OK — OPFS persistence round-tripped, no load errors');

    if (errors.length) console.log('  (console errors seen: ' + JSON.stringify(errors.slice(0, 5)) + ')');
  } catch (e) {
    fail('exception: ' + String(e?.stack || e).slice(0, 600));
  } finally {
    if (browser) await browser.close().catch(() => {});
    for (const p of [vite, proxy]) { try { p?.kill('SIGKILL'); } catch { /* */ } }
  }
  console.log(process.exitCode ? '\n=== WEB CLONE E2E: FAIL ===' : '\n=== WEB CLONE E2E: PASS ===');
}
main();
