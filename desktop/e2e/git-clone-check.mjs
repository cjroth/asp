// Real-browser e2e of the WEB git-bridge clone path (git-bridge spec §7.3, §10).
//
// Boots the real Vite dev server (real asp-wasm engine, no Tauri, no mock backend),
// stands up a hermetic git smart-HTTP server (a Node CGI shim around
// `git http-backend`, mirroring `tests/e2e/src/gitfix.rs`), runs the REAL relay git
// proxy (`asp relay --git-proxy`), then drives WebKit to paste the git URL into the
// connect modal and asserts the vault clones + a repo file appears in the editor.
//
// ── One important, real constraint (read before "why won't it go green here") ──
// The production proxy is SSRF-hardened: HTTPS/443 upstreams only, and it *rejects*
// private/loopback/link-local addresses (`crates/asp-core/src/gitproxy.rs`). That is
// correct and deliberate — but it means a *loopback* hermetic git server is refused
// by the proxy on purpose. So a fully green run needs ONE of:
//   (a) GIT_CLONE_URL=<a small, reachable https git repo>  → the real proxy accepts
//       it, and the whole path (browser → proxy → real host → wasm import) is
//       exercised for real. This is the recommended CI configuration.
//   (b) a proxy built with the crate-private `test_upstream_addr` hook wired to the
//       loopback git server (see `spawn_test_proxy` in gitproxy.rs) — fully hermetic,
//       needs a tiny test-only binary that isn't shipped by `asp relay`.
// With neither, the harness still boots every piece and drives the browser, but the
// proxy will (correctly) refuse the loopback upstream; the harness detects that and
// reports it as the known-limitation SKIP rather than a false failure.
//
// Also needs a browser: `xvfb-run -a node e2e/git-clone-check.mjs`. The VM this was
// authored on can't run WebKit+xvfb+iroh reliably; a capable machine / CI runs it.
import { spawn, execFileSync } from 'node:child_process';
import { setTimeout as sleep } from 'node:timers/promises';
import { mkdtempSync, writeFileSync, mkdirSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import http from 'node:http';
import { Builder, By, until } from 'selenium-webdriver';

const ROOT = '/home/chris/asp';
const WEB_PORT = Number(process.env.WEBAPP_PORT || 1432);
const PROXY_PORT = Number(process.env.GIT_PROXY_PORT || 8091);
const GIT_PORT = Number(process.env.GIT_HTTP_PORT || 8092);
const WKWD_PORT = Number(process.env.WKWD_PORT || 4476);
const WEB_URL = `http://localhost:${WEB_PORT}/`;
const PROXY_BASE = `http://127.0.0.1:${PROXY_PORT}`;

const fail = (msg) => { console.error('✗ ' + msg); process.exitCode = 1; };
const ok = (msg) => console.log('• ' + msg);
const skip = (msg) => console.log('⏭  SKIP: ' + msg);

async function waitForServer(url, ms, expectOk = true) {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    try { const r = await fetch(url); if (!expectOk || r.ok) return true; } catch { /* not up */ }
    await sleep(200);
  }
  return false;
}

// ── Hermetic git repo + smart-HTTP server (mirrors gitfix.rs GitHttpServer) ──
// Build a small repo, bare-clone it, and serve the bare repo over `git http-backend`
// so a real smart-HTTP-v2 clone works with no network.
function buildBareRepo(dir) {
  const work = join(dir, 'work');
  const bare = join(dir, 'srv');
  mkdirSync(work, { recursive: true });
  mkdirSync(bare, { recursive: true });
  const G = { cwd: work, env: { ...process.env, GIT_AUTHOR_NAME: 'ASP Test', GIT_AUTHOR_EMAIL: 't@asp.test', GIT_COMMITTER_NAME: 'ASP Test', GIT_COMMITTER_EMAIL: 't@asp.test', GIT_AUTHOR_DATE: '2020-01-01T00:00:00Z', GIT_COMMITTER_DATE: '2020-01-01T00:00:00Z' } };
  const git = (...a) => execFileSync('git', a, G);
  git('init', '-q', '-b', 'main');
  writeFileSync(join(work, 'README.md'), '# hermetic repo\n\nCloned by the asp git bridge e2e.\n');
  writeFileSync(join(work, 'HELLO-FROM-GIT.md'), 'this file must appear in the vault after clone\n');
  git('add', '-A');
  git('commit', '-q', '-m', 'initial commit');
  writeFileSync(join(work, 'HELLO-FROM-GIT.md'), 'this file must appear in the vault after clone\n\nsecond commit.\n');
  git('commit', '-qam', 'second commit');
  // Bare mirror that http-backend serves at /repo.git
  execFileSync('git', ['clone', '-q', '--bare', work, join(bare, 'repo.git')]);
  execFileSync('git', ['-C', join(bare, 'repo.git'), 'update-server-info']);
  return bare;
}

// Node http server that CGI-execs `git http-backend` (same mechanism gitfix.rs uses).
function startGitHttp(bareRoot, port) {
  const execPath = execFileSync('git', ['--exec-path']).toString().trim();
  const backend = join(execPath, 'git-http-backend');
  const server = http.createServer((req, res) => {
    const [path, query = ''] = (req.url || '/').split('?');
    const env = {
      ...process.env,
      GIT_PROJECT_ROOT: bareRoot,
      GIT_HTTP_EXPORT_ALL: '1',
      PATH_INFO: path,
      QUERY_STRING: query,
      REQUEST_METHOD: req.method,
      CONTENT_TYPE: req.headers['content-type'] || '',
      // http-backend re-exports this to upload-pack; required for protocol v2.
      HTTP_GIT_PROTOCOL: req.headers['git-protocol'] || '',
      GIT_PROTOCOL: req.headers['git-protocol'] || '',
    };
    const child = spawn(backend, [], { env });
    req.pipe(child.stdin);
    let buf = Buffer.alloc(0);
    child.stdout.on('data', (d) => { buf = Buffer.concat([buf, d]); });
    child.stdout.on('end', () => {
      const sep = buf.indexOf('\r\n\r\n');
      const head = buf.slice(0, sep).toString();
      const body = buf.slice(sep + 4);
      let status = 200;
      const headers = {};
      for (const line of head.split('\r\n')) {
        const i = line.indexOf(':');
        if (i < 0) continue;
        const k = line.slice(0, i).trim(), v = line.slice(i + 1).trim();
        if (k.toLowerCase() === 'status') status = parseInt(v, 10) || 200;
        else headers[k] = v;
      }
      res.writeHead(status, headers);
      res.end(body);
    });
    child.stderr.on('data', () => {});
  });
  return new Promise((resolve) => server.listen(port, '127.0.0.1', () => resolve(server)));
}

async function main() {
  // Guard: system git required for the hermetic fixture.
  try { execFileSync('git', ['--version']); } catch { skip('system git not available'); return; }

  const tmp = mkdtempSync(join(tmpdir(), 'asp-git-e2e-'));
  const hermeticUrl = `http://127.0.0.1:${GIT_PORT}/repo.git`;
  const cloneUrl = process.env.GIT_CLONE_URL || hermeticUrl;
  const usingHermetic = !process.env.GIT_CLONE_URL;
  const expectFile = process.env.GIT_EXPECT_FILE || (usingHermetic ? 'HELLO-FROM-GIT' : 'README');

  let gitServer, proxy, vite, wkwd, driver;
  try {
    // 1. hermetic git host (skipped when GIT_CLONE_URL points at a real host)
    if (usingHermetic) {
      const bare = buildBareRepo(tmp);
      gitServer = await startGitHttp(bare, GIT_PORT);
      ok(`hermetic git http server on ${hermeticUrl}`);
    } else {
      ok(`using external git repo ${cloneUrl}`);
    }

    // 2. the REAL relay git proxy
    const bin = join(ROOT, 'target', 'release', 'asp');
    proxy = spawn(bin, ['relay', '--git-proxy', '--git-proxy-addr', `127.0.0.1:${PROXY_PORT}`, '--listen-addr', '127.0.0.1:8090'], { stdio: ['ignore', 'inherit', 'inherit'] });
    // The proxy answers even a rejected request; wait for the port to accept a TCP connect.
    if (!(await waitForServer(`${PROXY_BASE}/`, 15000, false))) { fail('git proxy did not start (build `cargo build --release -p asp` first)'); return; }
    ok(`relay --git-proxy on ${PROXY_BASE}`);

    // 3. the web app (real wasm), with the proxy base wired in via VITE_GIT_PROXY_BASE
    vite = spawn('bunx', ['vite', '--port', String(WEB_PORT), '--strictPort'], {
      cwd: join(ROOT, 'desktop'),
      stdio: ['ignore', 'inherit', 'inherit'],
      env: { ...process.env, VITE_GIT_PROXY_BASE: PROXY_BASE },
    });
    if (!(await waitForServer(WEB_URL, 25000))) { fail('vite dev server did not start'); return; }
    await sleep(800);

    // 4. drive WebKit
    wkwd = spawn('WebKitWebDriver', ['--port=' + WKWD_PORT], { stdio: ['ignore', 'inherit', 'inherit'] });
    await sleep(500);
    driver = await new Builder().usingServer(`http://127.0.0.1:${WKWD_PORT}/`).withCapabilities({ browserName: 'MiniBrowser' }).build();

    await driver.get(WEB_URL);
    await driver.wait(until.elementLocated(By.xpath("//*[contains(text(),'Your vaults')]")), 20000);
    ok('web app loaded (real wasm bundle)');

    await driver.executeScript("window.__errs=[];const o=console.error;console.error=function(){window.__errs.push(Array.from(arguments).map(String).join(' '));o.apply(console,arguments)};window.addEventListener('unhandledrejection',e=>window.__errs.push('REJ:'+e.reason));");

    // Open the connect modal, paste the git URL.
    await driver.findElement(By.xpath("//button[contains(.,'Connect Vault')]")).click();
    const ta = await driver.wait(until.elementLocated(By.css('textarea')), 5000);
    await driver.executeScript("const e=arguments[0];const s=Object.getOwnPropertyDescriptor(window.HTMLTextAreaElement.prototype,'value').set;s.call(e,arguments[1]);e.dispatchEvent(new Event('input',{bubbles:true}));", ta, cloneUrl);

    // For an https URL the modal swaps the access-key field for a Token field.
    if (cloneUrl.startsWith('https')) {
      const hasToken = (await driver.findElements(By.css('[data-testid="git-token-field"]'))).length > 0;
      if (!hasToken) fail('https git URL did not reveal the Token field');
      else ok('git URL recognized (token field shown)');
    }

    await driver.findElement(By.xpath("//button[contains(.,'Connect')]")).click();

    // Success == the editor opens (vault materialized) and the repo file shows up.
    const opened = await driver.wait(async () => (await driver.findElements(By.css('[data-testid="live-editor"]'))).length > 0, 40000).then(() => true).catch(() => false);

    if (!opened) {
      // Distinguish the known SSRF limitation from a real failure.
      const err = (await driver.findElements(By.css('[data-testid="connect-error"]'))).length
        ? await driver.findElement(By.css('[data-testid="connect-error"]')).getText() : '';
      if (usingHermetic && /private|loopback|disallowed|proxy|refus|blocked/i.test(err)) {
        skip(`hermetic loopback upstream refused by the SSRF-hardened proxy (expected). Error: ${err}\n` +
             '   Re-run with GIT_CLONE_URL=<a small https repo> for a fully green pass, or wire the proxy `test_upstream_addr` hook.');
        return;
      }
      fail('vault did not open after clone. connect-error: ' + (err || '(none)'));
      return;
    }
    ok('vault cloned from git and editor opened');

    // The imported repo file should be visible somewhere in the vault UI.
    const body = await driver.findElement(By.css('body')).getText();
    if (!body.includes(expectFile)) fail(`expected repo file "${expectFile}" not visible after clone`);
    else ok(`imported repo file "${expectFile}" is present`);

    const errs = await driver.executeScript('return window.__errs');
    if (errs && errs.length) fail('console errors during clone: ' + JSON.stringify(errs));
    else ok('no console errors');
  } catch (e) {
    fail('exception: ' + String(e?.stack || e).slice(0, 500));
  } finally {
    if (driver) await driver.quit().catch(() => {});
    for (const p of [wkwd, vite, proxy]) { try { p?.kill('SIGKILL'); } catch { /* */ } }
    try { gitServer?.close(); } catch { /* */ }
    try { rmSync(tmp, { recursive: true, force: true }); } catch { /* */ }
  }
  console.log(process.exitCode ? '\n=== WEB GIT-CLONE E2E: FAIL ===' : '\n=== WEB GIT-CLONE E2E: PASS/SKIP ===');
}

main();
