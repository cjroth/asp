// Drive the real built frontend in a real WebKit browser (MiniBrowser via
// WebKitWebDriver) at scale, measuring real rendering/interaction cost. Spawns
// WebKitWebDriver itself; expects the static server (serve.mjs) already running.
import { spawn } from 'node:child_process';
import { setTimeout as sleep } from 'node:timers/promises';
import { Builder, By, until, Key } from 'selenium-webdriver';

const URL = process.env.URL || 'http://127.0.0.1:5599/?n=1000';
const WKWD_PORT = Number(process.env.WKWD_PORT || 4450);
const report = { url: URL, steps: [], ok: true };
const ok = (name, d) => { report.steps.push({ name, ...d }); console.log(`• ${name}: ${JSON.stringify(d)}`); };
const bad = (name, d) => { report.ok = false; report.steps.push({ name, fail: true, ...d }); console.log(`✗ ${name}: ${JSON.stringify(d)}`); };
const xtext = (s) => By.xpath(`//*[contains(text(), ${JSON.stringify(s)})]`);
const rowCount = (driver) => driver.findElements(By.className('asp-hover-row')).then((e) => e.length);

async function main() {
  const wkwd = spawn('WebKitWebDriver', ['--port=' + WKWD_PORT], { stdio: ['ignore', 'inherit', 'inherit'] });
  await sleep(1500);
  let driver;
  try {
    driver = await new Builder()
      .usingServer(`http://127.0.0.1:${WKWD_PORT}/`)
      .withCapabilities({ browserName: 'MiniBrowser' })
      .build();
  } catch (e) {
    bad('session', { error: String(e).slice(0, 300) });
    wkwd.kill('SIGKILL');
    return finish();
  }

  try {
    let t = Date.now();
    await driver.get(URL);
    await driver.wait(until.elementLocated(xtext('Your vaults')), 20000);
    ok('load-connect', { ms: Date.now() - t });

    // Open the (massive) vault → time to the editor.
    t = Date.now();
    await (await driver.wait(until.elementLocated(xtext('massive')), 20000)).click();
    await driver.wait(until.elementLocated(xtext('Files')), 20000);
    const openMs = Date.now() - t;
    ok('open-vault', { ms: openMs });
    if (openMs > 5000) bad('open-too-slow', { ms: openMs });

    await sleep(500);
    const rows = await rowCount(driver);
    ok('virtualized-rows', { count: rows });
    if (rows >= 200) bad('not-virtualized', { count: rows });

    // The bottom bar starts collapsed in the new design — expand the History tab
    // so the time-travel track renders.
    await (await driver.wait(until.elementLocated(By.xpath("//button[contains(.,'History')]")), 5000)).click();

    // History track must cap its rendered tick DOM nodes regardless of event count.
    await sleep(900); // history loads on a ~700ms debounce
    const ticks = await driver.executeScript("const tr=document.querySelector('[data-testid=\"history-track\"]');return tr?tr.querySelectorAll('div[title]').length:-1;");
    ok('history-tick-cap', { ticks });
    if (ticks > 260) bad('history-ticks-uncapped', { ticks });

    // Scroll the tree hard and measure responsiveness (real layout/paint).
    const scroller = await driver.findElement(By.className('asp-scroll'));
    t = Date.now();
    for (let i = 0; i < 10; i++) {
      await driver.executeScript('arguments[0].scrollTop += 600;', scroller);
      await sleep(16);
    }
    ok('scroll-10x', { ms: Date.now() - t });

    // Select a visible file → content appears.
    t = Date.now();
    const fileRow = await driver.findElement(By.xpath("//*[contains(@class,'asp-hover-row')][contains(.,'note-')]"));
    await fileRow.click();
    await driver.wait(async () => {
      const e = await driver.findElements(By.css('[data-testid="live-editor"]'));
      return e.length && (await e[0].getText()).trim().length > 0;
    }, 8000);
    ok('select-file', { ms: Date.now() - t });

    // Typing latency in the auto-selected README (large when ?big= set): focus,
    // type a burst, measure. Then measure the re-highlight settle after a pause —
    // this must stay small (line-level re-highlight) even on a big file.
    const editor = await driver.findElement(By.css('[data-testid="live-editor"]'));
    await editor.click();
    t = Date.now();
    await editor.sendKeys(' the quick brown fox jumps over the lazy dog');
    const typeMs = Date.now() - t;
    ok('type-43-chars', { ms: typeMs, msPerKey: Math.round(typeMs / 43) });
    if (typeMs / 43 > 120) bad('typing-laggy', { msPerKey: Math.round(typeMs / 43) });

    await sleep(450); // let any pending re-highlight finish
    t = Date.now();
    await editor.sendKeys('Z');
    await sleep(360); // re-highlight debounce (~320ms) fires within this
    const settleMs = Date.now() - t - 360;
    ok('rehighlight-settle', { ms: settleMs });
    if (settleMs > 250) bad('rehighlight-slow', { ms: settleMs });

    // Create a file via the "+" menu → New file. It appears (breadcrumb +
    // scrolled-to tree row) AND the editor shows its template content.
    const newFile = async () => {
      await driver.findElement(By.css('button[title="New note"]')).click();
      await (await driver.wait(until.elementLocated(xtext('New file')), 5000)).click();
    };
    t = Date.now();
    await newFile();
    const created = await driver.wait(until.elementLocated(xtext('untitled.md')), 8000).then(() => true).catch(() => false);
    const hasTemplate = await driver
      .wait(async () => (await driver.findElement(By.css('[data-testid="live-editor"]')).getText()).includes('untitled'), 8000)
      .then(() => true)
      .catch(() => false);
    ok('create-file', { ms: Date.now() - t, created, editorShowsContent: hasTemplate });
    if (!created) bad('create-missing', {});
    if (!hasTemplate) bad('create-editor-empty', {});

    // Create a second quickly → distinct name (no collision).
    await newFile();
    const created2 = await driver.wait(until.elementLocated(xtext('untitled-1.md')), 8000).then(() => true).catch(() => false);
    ok('create-file-2', { created2 });
    if (!created2) bad('create-2-missing', {});

    // Delete via context menu → leaves the tree.
    t = Date.now();
    const target = await driver.findElement(By.xpath("//*[contains(@class,'asp-hover-row')][contains(.,'untitled-1.md')]"));
    await driver.actions({ async: true }).contextClick(target).perform();
    await (await driver.wait(until.elementLocated(xtext('Delete')), 5000)).click();
    const gone = await driver
      .wait(async () => (await driver.findElements(By.xpath("//*[contains(@class,'asp-hover-row')][contains(.,'untitled-1.md')]"))).length === 0, 8000)
      .then(() => true)
      .catch(() => false);
    ok('delete-file', { ms: Date.now() - t, removed: gone });
    if (!gone) bad('delete-stuck', {});

    // Rapid multi-delete: delete 4 rendered note-* rows back-to-back (the 60ms
    // backend overlaps the ops) — the race that left files stuck in the tree.
    const names = await driver.executeScript(
      "return Array.from(document.querySelectorAll('.asp-hover-row')).map(r=>(r.textContent.match(/note-\\d+\\.md/)||[])[0]).filter(Boolean).slice(0,4);",
    );
    t = Date.now();
    for (const nm of names) {
      const row = await driver.findElement(By.xpath(`//*[contains(@class,'asp-hover-row')][contains(.,'${nm}')]`));
      await driver.actions({ async: true }).contextClick(row).perform();
      await (await driver.wait(until.elementLocated(xtext('Delete')), 5000)).click();
    }
    const allGone = await driver
      .wait(async () => {
        const present = await driver.executeScript(
          'return arguments[0].some(nm => Array.from(document.querySelectorAll(".asp-hover-row")).some(r => r.textContent.includes(nm)));',
          names,
        );
        return !present;
      }, 10000)
      .then(() => true)
      .catch(() => false);
    ok('rapid-multi-delete', { ms: Date.now() - t, count: names.length, allRemoved: allGone });
    if (!allGone) bad('multi-delete-race', { names });

    await sleep(400); // let the tree settle after the deletes

    // Re-find a row fresh right before acting to avoid stale references (the
    // virtualized list recreates row nodes on every render).
    const rowByName = async (nm) => driver.findElement(By.xpath(`//*[contains(@class,'asp-hover-row')][contains(.,'${nm}')]`));

    // Rename a rendered note file via context menu.
    try {
      t = Date.now();
      const rnName = await driver.executeScript("return Array.from(document.querySelectorAll('.asp-hover-row')).map(r=>(r.textContent.match(/note-\\d+\\.md/)||[])[0]).filter(Boolean)[0] || null;");
      if (rnName) {
        await driver.actions({ async: true }).contextClick(await rowByName(rnName)).perform();
        await (await driver.wait(until.elementLocated(xtext('Rename')), 5000)).click();
        const input = await driver.wait(until.elementLocated(By.css('.asp-hover-row input')), 5000);
        // Drive React's controlled input via the native setter + a real Enter.
        await driver.executeScript(
          "const el=arguments[0];const set=Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype,'value').set;set.call(el,'aaa-renamed.md');el.dispatchEvent(new Event('input',{bubbles:true}));el.dispatchEvent(new KeyboardEvent('keydown',{key:'Enter',bubbles:true}));",
          input,
        );
        const renamed = await driver.wait(until.elementLocated(xtext('aaa-renamed.md')), 8000).then(() => true).catch(() => false);
        const oldGone = (await driver.findElements(By.xpath(`//*[contains(@class,'asp-hover-row')][contains(.,'${rnName}')]`))).length === 0;
        ok('rename-file', { ms: Date.now() - t, renamed, oldGone });
        if (!renamed || !oldGone) bad('rename-broken', { rnName, renamed, oldGone });
      }
    } catch (e) {
      bad('rename-file', { error: String(e).slice(0, 200) });
    }

    // Switch to the other vault (the "opening another vault froze" path).
    try {
      t = Date.now();
      await driver.findElement(By.css('[data-testid="vault-switcher"]')).click();
      await (await driver.wait(until.elementLocated(xtext('second')), 5000)).click();
      const switched = await driver
        .wait(async () => {
          const e = await driver.findElements(By.css('[data-testid="live-editor"]'));
          return e.length && (await e[0].getText()).includes('Second');
        }, 10000)
        .then(() => true)
        .catch(() => false);
      ok('switch-vault', { ms: Date.now() - t, switched });
      if (!switched) bad('switch-vault-broken', {});
    } catch (e) {
      bad('switch-vault', { error: String(e).slice(0, 200) });
    }

    // History scrub: click on the track → time-travel (read-only) → back to Now.
    t = Date.now();
    const track = await driver.findElement(By.css('[data-testid="history-track"]'));
    // A no-move pointerdown→pointerup sets the playhead (a move would pan). Dispatch
    // directly so selenium's between-event motion can't turn the click into a pan.
    await driver.executeScript(
      "const t=arguments[0];const r=t.getBoundingClientRect();const o={bubbles:true,cancelable:true,clientX:r.x+r.width*0.3,clientY:r.y+r.height/2,pointerId:1,button:0};t.dispatchEvent(new PointerEvent('pointerdown',o));document.dispatchEvent(new PointerEvent('pointerup',o));",
      track,
    );
    // "read-only" lives in a non-first text node, so match on innerText, not XPath text().
    const hasRO = () => driver.executeScript("return document.body.innerText.includes('read-only')");
    const tt = await driver.wait(async () => await hasRO(), 6000).then(() => true).catch(() => false);
    if (tt) await (await driver.findElement(By.xpath("//button[contains(.,'Now')]"))).click();
    const backToNow = await driver.wait(async () => !(await hasRO()), 6000).then(() => true).catch(() => false);
    ok('history-scrub', { ms: Date.now() - t, enteredTimeTravel: tt, returnedToNow: backToNow });
  } catch (e) {
    bad('exception', { error: String(e?.stack || e).slice(0, 600) });
  } finally {
    if (driver) await driver.quit().catch(() => {});
    wkwd.kill('SIGKILL');
    finish();
  }
}

function finish() {
  console.log('\n=== REPORT ===\n' + JSON.stringify(report, null, 2));
  process.exit(report.ok ? 0 : 1);
}

main();
