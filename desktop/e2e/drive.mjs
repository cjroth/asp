// Real end-to-end harness: drives the ACTUAL Tauri app (real WebView + real Rust
// backend + real IPC) via tauri-driver + WebKitWebDriver, headless under Xvfb.
// It launches with a pre-seeded massive vault (via HOME/desktop_folders.json),
// clicks around, and measures/asserts responsiveness + correctness.
//
// Env: APP_BIN (path to context-desktop), TEST_HOME (seeded home), VAULT_NAME,
//      NATIVE_DRIVER (WebKitWebDriver path), TD_PORT.
import { spawn } from 'node:child_process';
import { setTimeout as sleep } from 'node:timers/promises';
import { Builder, By, until, Key } from 'selenium-webdriver';

const APP_BIN = process.env.APP_BIN;
const TEST_HOME = process.env.TEST_HOME;
const VAULT_NAME = process.env.VAULT_NAME || 'vault';
const NATIVE_DRIVER = process.env.NATIVE_DRIVER || '/usr/bin/WebKitWebDriver';
const TD_PORT = Number(process.env.TD_PORT || 4445);

const report = { steps: [], ok: true };
const note = (name, detail) => { report.steps.push({ name, ...detail }); console.log(`• ${name}: ${JSON.stringify(detail)}`); };
const fail = (name, detail) => { report.ok = false; report.steps.push({ name, fail: true, ...detail }); console.log(`✗ ${name}: ${JSON.stringify(detail)}`); };

const xtext = (s) => By.xpath(`//*[contains(text(), ${JSON.stringify(s)})]`);

async function main() {
  // 1. Start tauri-driver (inherits our HOME + DISPLAY → app loads the seeded vault).
  const td = spawn('tauri-driver', ['--port', String(TD_PORT), '--native-driver', NATIVE_DRIVER], {
    env: { ...process.env, HOME: TEST_HOME },
    stdio: ['ignore', 'inherit', 'inherit'],
  });
  td.on('exit', (c) => c && c !== 0 && console.log(`tauri-driver exited ${c}`));
  await sleep(1500);

  let driver;
  try {
    driver = await new Builder()
      .usingServer(`http://127.0.0.1:${TD_PORT}/`)
      .withCapabilities({ browserName: 'wry', 'tauri:options': { application: APP_BIN } })
      .build();
  } catch (e) {
    fail('session', { error: String(e).slice(0, 300) });
    td.kill('SIGKILL');
    return finish();
  }

  try {
    // 2. Connect screen → the seeded vault appears (app captured it on launch).
    let t = Date.now();
    await driver.wait(until.elementLocated(xtext('Your vaults')), 40000);
    note('connect-screen', { ms: Date.now() - t });

    const vaultRow = await driver.wait(until.elementLocated(xtext(VAULT_NAME)), 40000);

    // 3. Open the vault → time to the editor (Files label) appearing.
    t = Date.now();
    await vaultRow.click();
    await driver.wait(until.elementLocated(xtext('Files')), 40000);
    const openMs = Date.now() - t;
    note('open-vault', { ms: openMs, slow: openMs > 4000 });
    if (openMs > 8000) fail('open-vault-too-slow', { ms: openMs });

    // 4. Virtualization: only a bounded number of row DOM nodes despite many files.
    await sleep(400);
    const rows = await driver.findElements(By.className('asp-hover-row'));
    note('rendered-rows', { count: rows.length, virtualized: rows.length < 80 });
    if (rows.length >= 200) fail('not-virtualized', { count: rows.length });

    // 5. Select a file → content shows.
    t = Date.now();
    const fileRow = await driver.wait(until.elementLocated(By.xpath("//*[contains(@class,'asp-hover-row')][.//*[contains(text(),'.md')] or contains(.,'.md')]")), 10000).catch(() => null);
    if (fileRow) {
      await fileRow.click();
      // editor is the contenteditable; wait for it to have text
      await driver.wait(async () => {
        const els = await driver.findElements(By.css('[data-testid="live-editor"]'));
        if (!els.length) return false;
        const txt = await els[0].getText();
        return txt && txt.trim().length > 0;
      }, 10000).catch(() => {});
      note('select-file', { ms: Date.now() - t });
    } else {
      note('select-file', { skipped: 'no file row found in viewport' });
    }

    // 6. Create a file (the + button) → it must appear + be selected.
    const beforeCreate = (await driver.findElements(By.className('asp-hover-row'))).length;
    t = Date.now();
    const plusBtn = await driver.findElement(By.css('button[title="New note"]'));
    await plusBtn.click();
    // The new "untitled.md" should become the breadcrumb/selected file.
    const created = await driver.wait(until.elementLocated(xtext('untitled.md')), 10000).then(() => true).catch(() => false);
    note('create-file', { ms: Date.now() - t, created });
    if (!created) fail('create-file-missing', { beforeRows: beforeCreate });

    // 7. Create a second file quickly (was "every other time" buggy).
    await driver.findElement(By.css('button[title="New note"]')).click();
    const created2 = await driver.wait(until.elementLocated(xtext('untitled-1.md')), 10000).then(() => true).catch(() => false);
    note('create-file-2', { created2 });
    if (!created2) fail('create-file-2-missing', {});

    // 8. Delete the selected file via context menu → it must leave the tree.
    t = Date.now();
    const target = await driver.wait(until.elementLocated(By.xpath("//*[contains(@class,'asp-hover-row')][contains(.,'untitled-1.md')]")), 8000).catch(() => null);
    if (target) {
      await driver.actions({ async: true }).contextClick(target).perform();
      const del = await driver.wait(until.elementLocated(xtext('Delete')), 5000);
      await del.click();
      const gone = await driver.wait(async () => {
        const els = await driver.findElements(By.xpath("//*[contains(@class,'asp-hover-row')][contains(.,'untitled-1.md')]"));
        return els.length === 0;
      }, 10000).then(() => true).catch(() => false);
      note('delete-file', { ms: Date.now() - t, removed: gone });
      if (!gone) fail('delete-file-stuck', {});
    } else {
      fail('delete-file', { error: 'target row not found' });
    }
  } catch (e) {
    fail('exception', { error: String(e?.stack || e).slice(0, 500) });
  } finally {
    if (driver) await driver.quit().catch(() => {});
    td.kill('SIGKILL');
    finish();
  }
}

function finish() {
  console.log('\n=== REPORT ===');
  console.log(JSON.stringify(report, null, 2));
  process.exit(report.ok ? 0 : 1);
}

main();
