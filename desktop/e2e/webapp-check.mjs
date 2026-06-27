// Real-browser smoke test of the WEB stack (the gap that let the wasm/OPFS bugs
// through): boots the actual Vite dev server — no Tauri, no mock backend — and
// drives it in a real WebKit browser, creating a vault end-to-end through the
// asp-wasm engine. Asserts the editor opens and the console stays clean. Catches
// wasm-loading (MIME / file://) and web-backend regressions the jsdom unit tests
// (which mock the backend) and the desktop perf harness (which mocks Tauri) can't.
import { spawn } from 'node:child_process';
import { setTimeout as sleep } from 'node:timers/promises';
import { Builder, By, until } from 'selenium-webdriver';

const PORT = Number(process.env.WEBAPP_PORT || 1431);
const WKWD_PORT = Number(process.env.WKWD_PORT || 4475);
const URL = `http://localhost:${PORT}/`;

const fail = (msg) => { console.error('✗ ' + msg); process.exitCode = 1; };
const ok = (msg) => console.log('• ' + msg);

async function waitForServer(url, ms) {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    try { if ((await fetch(url)).ok) return true; } catch { /* not up yet */ }
    await sleep(200);
  }
  return false;
}

async function main() {
  const vite = spawn('bunx', ['vite', '--port', String(PORT), '--strictPort'], { cwd: '/home/chris/asp/desktop', stdio: ['ignore', 'inherit', 'inherit'] });
  const wkwd = spawn('WebKitWebDriver', ['--port=' + WKWD_PORT], { stdio: ['ignore', 'inherit', 'inherit'] });
  let driver;
  try {
    if (!(await waitForServer(URL, 20000))) { fail('vite dev server did not start'); return; }
    await sleep(800);
    driver = await new Builder().usingServer(`http://127.0.0.1:${WKWD_PORT}/`).withCapabilities({ browserName: 'MiniBrowser' }).build();

    await driver.get(URL);
    await driver.wait(until.elementLocated(By.xpath("//*[contains(text(),'Your vaults')]")), 20000);
    ok('connect screen loaded (real wasm bundle)');

    // It must NOT prompt for a folder on the web.
    if ((await driver.findElements(By.xpath("//*[contains(text(),'On this computer')]"))).length) fail('web app showed desktop "On this computer"');
    ok('platform detected as web');

    // Capture console errors from here on.
    await driver.executeScript("window.__errs=[];const o=console.error;console.error=function(){window.__errs.push(Array.from(arguments).map(String).join(' '));o.apply(console,arguments)};window.addEventListener('error',e=>window.__errs.push('ERR:'+(e.message||e.error)));window.addEventListener('unhandledrejection',e=>window.__errs.push('REJ:'+e.reason));");

    await driver.findElement(By.xpath("//button[contains(.,'New Vault')]")).click();
    if ((await driver.findElements(By.xpath("//*[contains(text(),'Choose…')]"))).length) fail('web "New Vault" offered a folder chooser');
    const name = await driver.wait(until.elementLocated(By.css('input[placeholder="My vault"]')), 5000);
    await driver.executeScript("const e=arguments[0];const s=Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype,'value').set;s.call(e,'Smoke Vault');e.dispatchEvent(new Event('input',{bubbles:true}));", name);
    await driver.findElement(By.xpath("//button[contains(.,'Create vault')]")).click();

    // The editor opens only if the wasm engine created + materialized the vault.
    const opened = await driver.wait(async () => (await driver.findElements(By.css('[data-testid="live-editor"]'))).length > 0, 15000).then(() => true).catch(() => false);
    if (!opened) fail('editor did not open after Create vault');
    else ok('vault created via asp-wasm and editor opened');

    const editorText = opened ? (await driver.findElement(By.css('[data-testid="live-editor"]')).getText()) : '';
    if (opened && !editorText.includes('New vault')) fail('seeded README did not render (editor empty)');
    else if (opened) ok('seeded note rendered');

    const errs = await driver.executeScript('return window.__errs');
    if (errs.length) fail('console errors during create: ' + JSON.stringify(errs));
    else ok('no console errors');
  } catch (e) {
    fail('exception: ' + String(e?.stack || e).slice(0, 400));
  } finally {
    if (driver) await driver.quit().catch(() => {});
    wkwd.kill('SIGKILL');
    vite.kill('SIGKILL');
  }
  console.log(process.exitCode ? '\n=== WEBAPP SMOKE: FAIL ===' : '\n=== WEBAPP SMOKE: PASS ===');
}

main();
