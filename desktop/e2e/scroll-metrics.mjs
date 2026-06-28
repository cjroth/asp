// Real-browser measurement for the "spurious scrollbars" bug. Boots the real Vite
// dev server (real wasm + OPFS, WEB build — the env the user reported), creates a
// fresh WEB vault, and measures scrollHeight vs clientHeight for BOTH scroll
// containers that use `.asp-scroll`:
//   1) the file-tree scroll container (inside <aside>)
//   2) the editor content scroll container (the .asp-scroll wrapping <LiveEditor>)
// with SHORT content (a fresh, near-empty note). If scrollHeight > clientHeight
// when content clearly fits, that's a spurious overflow / layout bug.
//
// WebKit/MiniBrowser is slow, so waits are generous. Run: node e2e/scroll-metrics.mjs
import { spawn } from 'node:child_process';
import { setTimeout as sleep } from 'node:timers/promises';
import { Builder, By, until } from 'selenium-webdriver';

const PORT = Number(process.env.WEBAPP_PORT || 1438);
const WKWD_PORT = Number(process.env.WKWD_PORT || 4481);
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

// Returns {clientHeight, scrollHeight, overflow, clientWidth, scrollWidth} for a
// node found by a small in-page resolver function (string of JS returning a node).
const METRICS = `
  return (function(node){
    if(!node) return null;
    return {
      clientHeight: node.clientHeight, scrollHeight: node.scrollHeight,
      clientWidth: node.clientWidth, scrollWidth: node.scrollWidth,
      vOverflow: node.scrollHeight - node.clientHeight,
      hOverflow: node.scrollWidth - node.clientWidth,
    };
  })(NODEEXPR);`;

async function metrics(driver, nodeExpr) {
  return driver.executeScript(METRICS.replace('NODEEXPR', nodeExpr));
}

async function main() {
  const vite = spawn('bunx', ['vite', '--port', String(PORT), '--strictPort'], { cwd: '/home/chris/asp/desktop', stdio: ['ignore', 'inherit', 'inherit'] });
  const wkwd = spawn('WebKitWebDriver', ['--port=' + WKWD_PORT], { stdio: ['ignore', 'inherit', 'inherit'] });
  let driver;
  const out = {};
  try {
    if (!(await waitForServer(URL, 30000))) { fail('vite dev server did not start'); return; }
    await sleep(1000);
    driver = await new Builder().usingServer(`http://127.0.0.1:${WKWD_PORT}/`).withCapabilities({ browserName: 'MiniBrowser' }).build();
    // Force a deterministic viewport so the numbers are reproducible.
    await driver.manage().window().setRect({ width: 1200, height: 800 });

    await driver.get(URL);
    await driver.wait(until.elementLocated(By.xpath("//*[contains(text(),'Your vaults')]")), 30000);
    ok('connect screen loaded');

    await driver.findElement(By.xpath("//button[contains(.,'New Vault')]")).click();
    const name = await driver.wait(until.elementLocated(By.css('input[placeholder="My vault"]')), 10000);
    await driver.executeScript("const e=arguments[0];const s=Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype,'value').set;s.call(e,'Scroll Vault');e.dispatchEvent(new Event('input',{bubbles:true}));", name);
    await driver.findElement(By.xpath("//button[contains(.,'Create vault')]")).click();

    const opened = await driver.wait(async () => (await driver.findElements(By.css('[data-testid="live-editor"]'))).length > 0, 20000).then(() => true).catch(() => false);
    if (!opened) { fail('editor did not open after Create vault'); return; }
    ok('vault created and editor opened (seeded README)');

    // Create a fresh, near-empty note so the EDITOR holds SHORT content that
    // clearly fits — the case where a scrollbar must NOT appear.
    await driver.findElement(By.css('button[title="New note"]')).click();
    await (await driver.wait(until.elementLocated(By.xpath("//*[contains(text(),'New file')]")), 5000)).click();
    await driver.wait(until.elementLocated(By.xpath("//*[contains(text(),'untitled.md')]")), 8000);
    await sleep(800); // let layout settle

    // Resolver expressions for the two .asp-scroll containers.
    const treeExpr = "(function(){var a=document.querySelector('aside');return a?a.querySelector('.asp-scroll'):null;})()";
    const editorExpr = "(function(){var le=document.querySelector('[data-testid=\"live-editor\"]');var n=le;while(n){if(n.classList&&n.classList.contains('asp-scroll'))return n;n=n.parentElement;}return null;})()";

    out.tree_shortContent = await metrics(driver, treeExpr);
    out.editor_shortContent = await metrics(driver, editorExpr);

    // Also report how many files are in the tree (should be tiny → must not scroll).
    out.treeRowCount = await driver.executeScript("return document.querySelectorAll('.asp-hover-row').length;");

    // For contrast: select the seeded README (longer) to confirm the editor DOES
    // scroll for genuinely tall content.
    const readme = await driver.findElements(By.xpath("//*[contains(@class,'asp-hover-row')][contains(.,'README')]"));
    if (readme.length) {
      await readme[0].click();
      await sleep(800);
      out.editor_longContent = await metrics(driver, editorExpr);
    }

    console.log('\n=== SCROLL METRICS (viewport 1200x800) ===');
    console.log(JSON.stringify(out, null, 2));

    const t = out.tree_shortContent, e = out.editor_shortContent;
    if (t && t.vOverflow > 0) fail(`TREE spurious vertical overflow with ${out.treeRowCount} files: scrollHeight ${t.scrollHeight} > clientHeight ${t.clientHeight} (overflow ${t.vOverflow}px)`);
    else if (t) ok(`tree: no spurious vertical overflow (${t.scrollHeight} vs ${t.clientHeight})`);
    if (e && e.vOverflow > 0) fail(`EDITOR spurious vertical overflow with short content: scrollHeight ${e.scrollHeight} > clientHeight ${e.clientHeight} (overflow ${e.vOverflow}px)`);
    else if (e) ok(`editor: no spurious vertical overflow (${e.scrollHeight} vs ${e.clientHeight})`);
    if (e && e.hOverflow > 0) fail(`EDITOR horizontal overflow: scrollWidth ${e.scrollWidth} > clientWidth ${e.clientWidth}`);
  } catch (e) {
    fail('exception: ' + String(e?.stack || e).slice(0, 600));
  } finally {
    if (driver) await driver.quit().catch(() => {});
    wkwd.kill('SIGKILL');
    vite.kill('SIGKILL');
  }
  console.log(process.exitCode ? '\n=== SCROLL METRICS: FAIL (spurious overflow found) ===' : '\n=== SCROLL METRICS: PASS ===');
}

main();
