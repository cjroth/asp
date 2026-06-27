// Real-browser repro for the ```mermaid render bug: boots the actual Vite dev
// server (real mermaid bundle, no Tauri/mock), creates a WEB vault, and inspects
// the seeded README's `.md-diagram` to prove it renders an <svg> — not the raw
// `<pre>` code fallback. It captures the browser console (errors, rejections,
// console.error/warn) so the REAL mermaid failure is visible, and dumps the
// `.md-diagram` innerHTML so you can SEE whether it's an SVG, the fallback, or
// empty. WebKit/MiniBrowser is slow, so waits are generous.
import { spawn } from 'node:child_process';
import { setTimeout as sleep } from 'node:timers/promises';
import { Builder, By, until } from 'selenium-webdriver';

const PORT = Number(process.env.WEBAPP_PORT || 1437);
const WKWD_PORT = Number(process.env.WKWD_PORT || 4479);
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
    if (!(await waitForServer(URL, 30000))) { fail('vite dev server did not start'); return; }
    await sleep(1000);
    driver = await new Builder().usingServer(`http://127.0.0.1:${WKWD_PORT}/`).withCapabilities({ browserName: 'MiniBrowser' }).build();

    await driver.get(URL);
    await driver.wait(until.elementLocated(By.xpath("//*[contains(text(),'Your vaults')]")), 30000);
    ok('connect screen loaded (real wasm + mermaid bundle)');

    // Capture console from here on.
    await driver.executeScript("window.__errs=[];const oe=console.error;console.error=function(){window.__errs.push('ERROR:'+Array.from(arguments).map(String).join(' '));oe.apply(console,arguments)};const ow=console.warn;console.warn=function(){window.__errs.push('WARN:'+Array.from(arguments).map(String).join(' '));ow.apply(console,arguments)};window.addEventListener('error',e=>window.__errs.push('ERR:'+(e.message||e.error)));window.addEventListener('unhandledrejection',e=>window.__errs.push('REJ:'+e.reason));");

    await driver.findElement(By.xpath("//button[contains(.,'New Vault')]")).click();
    const name = await driver.wait(until.elementLocated(By.css('input[placeholder="My vault"]')), 10000);
    await driver.executeScript("const e=arguments[0];const s=Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype,'value').set;s.call(e,'Diagram Vault');e.dispatchEvent(new Event('input',{bubbles:true}));", name);
    await driver.findElement(By.xpath("//button[contains(.,'Create vault')]")).click();

    const opened = await driver.wait(async () => (await driver.findElements(By.css('[data-testid="live-editor"]'))).length > 0, 20000).then(() => true).catch(() => false);
    if (!opened) { fail('editor did not open after Create vault'); return; }
    ok('vault created and editor opened');

    // The seeded README contains a ```mermaid flowchart. Wait (generously) for the
    // async mermaid render to fill the `.md-diagram` placeholder with an <svg>.
    const placeholder = await driver.wait(async () => (await driver.findElements(By.css('.md-diagram'))).length > 0, 15000).then(() => true).catch(() => false);
    if (!placeholder) { fail('no .md-diagram placeholder appeared (fence not detected)'); }
    else ok('.md-diagram placeholder present (fence detected)');

    const hasSvg = await driver.wait(async () => (await driver.findElements(By.css('.md-diagram svg'))).length > 0, 20000).then(() => true).catch(() => false);

    const innerHTML = placeholder ? await driver.executeScript("const n=document.querySelector('.md-diagram');return n?n.innerHTML:'(none)';") : '(no placeholder)';
    console.log('\n--- .md-diagram innerHTML (first 600 chars) ---');
    console.log(String(innerHTML).slice(0, 600));
    console.log('--- end innerHTML ---\n');

    if (hasSvg) ok('SEEDED diagram rendered as SVG (.md-diagram svg present)');
    else fail('SEEDED diagram did NOT render — still showing fallback/empty');

    const errs = await driver.executeScript('return window.__errs');
    console.log('\n--- browser console (errors/warns) ---');
    console.log(errs.length ? JSON.stringify(errs, null, 2) : '(clean)');
    console.log('--- end console ---\n');
  } catch (e) {
    fail('exception: ' + String(e?.stack || e).slice(0, 600));
  } finally {
    if (driver) await driver.quit().catch(() => {});
    wkwd.kill('SIGKILL');
    vite.kill('SIGKILL');
  }
  console.log(process.exitCode ? '\n=== DIAGRAM CHECK: FAIL ===' : '\n=== DIAGRAM CHECK: PASS ===');
}

main();
