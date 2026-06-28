// Real-browser proof that @dnd-kit/sortable reordering of the tab strip COMMITS
// the new order on an actual POINTER drag — the path jsdom can't exercise (jsdom
// has no layout, so the unit tests drive reordering through the KeyboardSensor,
// and tabDnd.test.ts pins the pure index mapping). Here we mount the REAL
// `TabBar` in WebKitGTK (MiniBrowser, the same engine family as the Tauri macOS
// webview), drag the first tab past the third with a real pointer gesture, and
// assert the rendered tab order (and the onReorder(from,to) App receives) is the
// reordered one.
//
// Usage: xvfb-run -a bun e2e/tab-reorder-drag.mjs
import { createServer } from 'node:http';
import { writeFileSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { setTimeout as sleep } from 'node:timers/promises';
import { spawn } from 'node:child_process';
import { Builder, By } from 'selenium-webdriver';

const PORT = Number(process.env.TAB_PORT || 1461);
const WKWD_PORT = Number(process.env.WKWD_PORT || 4483);
const here = new URL('.', import.meta.url).pathname;

// A tiny app that mounts the real TabBar and keeps the canonical order in state,
// updating it from the SAME onReorder(from,to) contract App uses. We mirror the
// (from,to) → array move so the DOM order reflects exactly what App would persist,
// and stash the last onReorder args on window for the assertion.
const entry = `
import React, { useState } from 'react';
import { createRoot } from 'react-dom/client';
import TabBar from '../src/vault/TabBar.tsx';

function move(arr, from, to) {
  const copy = arr.slice();
  const [it] = copy.splice(from, 1);
  copy.splice(to, 0, it);
  return copy;
}

function App() {
  const [tabs, setTabs] = useState(['one.md', 'two.md', 'three.md']);
  const [active, setActive] = useState('one.md');
  return React.createElement('div', { style: { display: 'flex', width: 600, height: 44, border: '1px solid #ccc' } },
    React.createElement(TabBar, {
      tabs, active, prettyNames: false, accent: '#3d63dd', accentSoft: '#3d63dd22',
      onSelect: setActive, onClose: () => {},
      onReorder: (from, to) => { window.__reorder = [from, to]; setTabs((t) => move(t, from, to)); },
    }),
  );
}
createRoot(document.getElementById('root')).render(React.createElement(App));
`;

async function main() {
  // The entry must live inside the project so its `react` / `@dnd-kit` imports
  // resolve against desktop/node_modules. Written next to this script, bundled,
  // then removed.
  const entryPath = join(here, '_tab_entry.tmp.tsx');
  writeFileSync(entryPath, entry);

  let js;
  try {
    const built = await Bun.build({ entrypoints: [entryPath], target: 'browser', minify: false });
    if (!built.success) {
      console.error('✗ bundle failed:', built.logs.map(String).join('\n'));
      process.exitCode = 1;
      return;
    }
    js = await built.outputs[0].text();
  } finally {
    rmSync(entryPath, { force: true });
  }
  const page = `<!doctype html><html><head><meta charset="utf-8"><style>
    *{box-sizing:border-box} body{margin:0;font-family:system-ui}
    [data-testid=tab]{height:44px}
  </style></head><body><div id="root"></div><script type="module">${js}</script></body></html>`;

  const server = createServer((_req, res) => { res.writeHead(200, { 'content-type': 'text/html' }); res.end(page); });
  await new Promise((r) => server.listen(PORT, r));
  const wkwd = spawn('WebKitWebDriver', ['--port=' + WKWD_PORT], { stdio: ['ignore', 'inherit', 'inherit'] });
  await sleep(1200);

  let driver;
  try {
    driver = await new Builder().usingServer(`http://127.0.0.1:${WKWD_PORT}/`).withCapabilities({ browserName: 'MiniBrowser' }).build();
    await driver.get(`http://localhost:${PORT}/`);
    await sleep(700);

    const order = () => driver.executeScript(
      "return [...document.querySelectorAll('[data-testid=tab]')].map((t) => t.getAttribute('data-path'));",
    );
    const before = await order();
    console.log('order before:', before.join(' , '));
    if (before.join(',') !== 'one.md,two.md,three.md') {
      console.error('✗ unexpected initial order'); process.exitCode = 1; return;
    }

    const tabs = await driver.findElements(By.css('[data-testid=tab]'));
    const first = tabs[0];
    const last = tabs[2];
    // Real pointer gesture: press on tab 1, nudge >4px to clear the activation
    // constraint, glide over tab 3, then release. dnd-kit commits on pointer up.
    const actions = driver.actions({ async: true });
    await actions
      .move({ origin: first })
      .press()
      .move({ origin: first, x: 10, y: 0 })
      .move({ origin: last })
      .move({ origin: last, x: 20, y: 0 })
      .pause(120)
      .release()
      .perform();
    await sleep(500);

    const after = await order();
    const reorder = await driver.executeScript('return window.__reorder || null;');
    console.log('order after: ', after.join(' , '), ' onReorder=', JSON.stringify(reorder));

    // one.md must have moved off the front (it was dragged to the end region).
    if (after[0] === 'one.md') {
      console.error('✗ tab did not move — pointer drag did not commit a reorder');
      process.exitCode = 1;
    } else if (after.slice().sort().join(',') !== 'one.md,three.md,two.md') {
      console.error('✗ tabs were lost/duplicated by the drag:', after.join(','));
      process.exitCode = 1;
    } else if (!reorder) {
      console.error('✗ onReorder(from,to) was never called');
      process.exitCode = 1;
    } else {
      console.log('✓ pointer drag committed a reorder via onReorder(from,to)');
    }
  } catch (e) {
    console.error('exception:', String(e?.stack || e).slice(0, 1000));
    process.exitCode = 1;
  } finally {
    if (driver) await driver.quit().catch(() => {});
    wkwd.kill('SIGKILL');
    server.close();
  }
}
main();
