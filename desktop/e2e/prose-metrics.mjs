// Real-browser repro for the PROSE caret / selection metrics bug (run with `bun`).
//
// jsdom paints nothing, so the geometry these symptoms are about is invisible to
// unit tests. This boots a tiny static server that renders the ACTUAL
// `renderLiveHtml` / `renderCodeHtml` output inside contentEditable surfaces
// styled exactly like `LiveEditor`'s prose (15.5px / line-height 1.8) and code
// (13px / 1.7) branches, then drives MiniBrowser (WebKitWebDriver) to MEASURE:
//
//   * lineBox   — the line div's painted height (the visual line spacing).
//   * glyph     — the height of a 1-char Range of the first VISIBLE text. This
//                 equals the PAINTED CARET height (verified by pixel-scanning a
//                 coloured caret: a 15.5px prose paragraph caret measures 18px,
//                 exactly the 1-char rect), so it is a reliable caret proxy.
//   * markW     — the painted width of a hidden `.cm-mark` (must stay ~0).
//   * rtOK      — readLive-style textContent still equals the source (round-trip).
//
// FINDINGS (WebKitGTK / MiniBrowser):
//   prose paragraph: lineBox≈27  glyph/caret≈18  → caret fills only ~67% of the
//                    line, which reads as a "short caret". This reproduces on a
//                    PLAIN, mark-free paragraph, so the hidden `.cm-mark` spans
//                    are NOT the cause — the caret is the font's content box and
//                    the 1.8 line-height adds half-leading the caret never fills.
//   heading:         lineBox≈33  glyph/caret≈30  (~91%) — looks fine because its
//                    line-height is 1.3.
//   code:            lineBox≈22  glyph/caret≈17  (~77%) — looks fine because the
//                    monospace content box is close to the 1.7 line box.
//   Selection paints the full line box and is glyph-aligned in WebKitGTK for
//   plain, mark-heavy and heading lines (no misalignment observed here).
//
// `MARK_FIX=1` swaps `.cm-mark { display:none }` for the in-flow invisibility the
// fix ships (`font-size:0;color:transparent`). The table proves it keeps markW≈0
// and rtOK=true while leaving the line/caret geometry of the surrounding text
// unchanged — i.e. it is a safe structural change (marks stay inside the line box
// instead of being removed from it, which is what disturbs caret/selection
// geometry in Chromium-based engines such as Tauri's WebView2 that this sandbox
// cannot run).
//
// Usage: xvfb-run -a bun e2e/prose-metrics.mjs   (add MARK_FIX=1 for the A side)
import { createServer } from 'node:http';
import { readFileSync } from 'node:fs';
import { setTimeout as sleep } from 'node:timers/promises';
import { spawn } from 'node:child_process';
import { Builder } from 'selenium-webdriver';
import { renderLiveHtml, renderCodeHtml } from '../src/vault/markdown.ts';

const PORT = Number(process.env.PROSE_PORT || 1459);
const WKWD_PORT = Number(process.env.WKWD_PORT || 4481);
const css = readFileSync(new URL('../src/styles.css', import.meta.url), 'utf8');

const PROSE = [
  'Plain paragraph with no markdown syntax at all here now.',
  'Marks: **bold**, *italic*, `code`, and a [link](https://example.com) too.',
  '# Heading with some words',
].join('\n');
const CODE = ['function plain(line, of, code) {', '  return line + of + code;', '}'].join('\n');

const MARK_CSS = process.env.MARK_FIX ? '.cm-mark{display:inline!important;font-size:0!important;color:transparent!important;}' : '';

const page = `<!doctype html><html><head><meta charset="utf-8"><style>${css}${MARK_CSS}
  .ed{width:760px;max-width:100%;outline:none;background:#fff;white-space:pre-wrap;word-break:break-word;font-family:Georgia,serif;font-size:15.5px;line-height:1.8;color:#111;padding:20px 40px;--accent:#3d63dd;}
  .code{font-family:'JetBrains Mono',ui-monospace,monospace;font-size:13px;line-height:1.7;}</style></head>
<body style="margin:0;background:#fff"><div id="ed" class="ed" contenteditable="true" spellcheck="false">${renderLiveHtml(PROSE)}</div>
<div id="code" class="ed code" contenteditable="true" spellcheck="false">${renderCodeHtml(CODE, 'js')}</div></body></html>`;

const server = createServer((_req, res) => { res.writeHead(200, { 'content-type': 'text/html' }); res.end(page); });

async function main() {
  await new Promise((r) => server.listen(PORT, r));
  const wkwd = spawn('WebKitWebDriver', ['--port=' + WKWD_PORT], { stdio: ['ignore', 'inherit', 'inherit'] });
  await sleep(1200);
  let driver;
  try {
    driver = await new Builder().usingServer(`http://127.0.0.1:${WKWD_PORT}/`).withCapabilities({ browserName: 'MiniBrowser' }).build();
    await driver.get(`http://localhost:${PORT}/`);
    await sleep(700);

    const measure = (id, src) => driver.executeScript(`
      const ed = document.getElementById(arguments[0]);
      const fix = (n) => +(+n).toFixed(1);
      const firstVisibleText = (root) => {
        const w = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
        let n; while ((n = w.nextNode())) {
          if (!(n.nodeValue||'').length) continue;
          const r = document.createRange(); r.selectNodeContents(n);
          if (r.getBoundingClientRect().height > 1) return n;
        }
        return null;
      };
      const res = [];
      ed.childNodes.forEach((div, i) => {
        const lineH = div.getBoundingClientRect().height;
        const t = firstVisibleText(div);
        let glyph = null;
        if (t && (t.nodeValue||'').length) {
          const r = document.createRange(); r.setStart(t, 0); r.setEnd(t, 1);
          glyph = r.getBoundingClientRect().height;
        }
        const mark = div.querySelector('.cm-mark');
        const markW = mark ? mark.getBoundingClientRect().width : 0;
        res.push({ i, lineH: fix(lineH), glyph: glyph==null?null:fix(glyph), markW: fix(markW), text: (div.textContent||'').slice(0,40) });
      });
      getSelection().removeAllRanges();
      return { rows: res, rt: ed.id==='ed' ? null : null, text: [...ed.childNodes].map(d => d.nodeName==='DIV' ? (d.textContent||'') : '').join('\\n') };
    `, id).then((out) => ({ out, src }));

    const prose = await measure('ed', PROSE);
    const code = await measure('code', CODE);

    console.log('\n=== PROSE / CODE METRICS  (MARK_FIX=' + (process.env.MARK_FIX ? 'ON' : 'off') + ') ===');
    console.log('cols: lineBox | caret≈glyph | caret%line | markW(must~0)');
    for (const r of prose.out.rows) {
      const pct = r.glyph ? Math.round((r.glyph / r.lineH) * 100) : 0;
      console.log(`prose ${r.i}: lineBox=${r.lineH}  caret=${r.glyph}  ${pct}%  markW=${r.markW}  "${r.text}"`);
    }
    for (const r of code.out.rows) {
      const pct = r.glyph ? Math.round((r.glyph / r.lineH) * 100) : 0;
      console.log(`code  ${r.i}: lineBox=${r.lineH}  caret=${r.glyph}  ${pct}%  markW=${r.markW}  "${r.text}"`);
    }
    // Round-trip: the joined textContent of the prose line divs must equal source.
    const rtOK = prose.out.text === PROSE;
    console.log('round-trip (readLive textContent === source):', rtOK ? 'OK' : 'FAIL');
    if (!rtOK) { console.log('  got:', JSON.stringify(prose.out.text)); process.exitCode = 1; }
    console.log('=== end ===\n');
  } catch (e) {
    console.error('exception:', String(e?.stack || e).slice(0, 800));
    process.exitCode = 1;
  } finally {
    if (driver) await driver.quit().catch(() => {});
    wkwd.kill('SIGKILL');
    server.close();
  }
}
main();
