// ws:// interop: a demo node bridges to a REAL `asp watch --listen` peer via the
// genuine Session (handshake + version-vector catch-up), exactly as `asp clone`
// would. Spawns the native binary as a relay; asserts the demo node pulls the
// native vault and pushes its own edit back. Mirrors sdks/typescript parity.
import { spawnSync } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createNetwork } from '../src/engine/network.ts';

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, '..', '..');
const ASP = join(repoRoot, 'target', 'debug', 'asp');
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
let fail = 0;
const check = (n, c) => { console.log(`${c ? '✓' : '✗ FAIL'}  ${n}`); if (!c) fail++; };

function asp(args, home) {
  const r = spawnSync(ASP, args, { env: { ...process.env, ASP_HOME: home, ASP_LOG: 'warn' } });
  if (r.status !== 0) throw new Error(`asp ${args.join(' ')} failed: ${r.stderr}`);
}
async function readPort(stream) {
  const reader = stream.getReader();
  const dec = new TextDecoder();
  let buf = '';
  const deadline = Date.now() + 15000;
  while (Date.now() < deadline) {
    const { value, done } = await reader.read();
    if (done) break;
    buf += dec.decode(value);
    const m = buf.match(/:\/\/0\.0\.0\.0:(\d+)/);
    if (m) { reader.releaseLock(); return Number.parseInt(m[1], 10); }
  }
  throw new Error('asp listener did not announce a port');
}
async function waitFor(pred, ms = 9000) {
  const deadline = Date.now() + ms;
  while (Date.now() < deadline) { if (pred()) return true; await sleep(80); }
  return false;
}

if (!existsSync(ASP)) { console.error(`build first: cargo build -p asp (looked at ${ASP})`); process.exit(1); }

const root = mkdtempSync(join(tmpdir(), 'asp-demo-ws-'));
const dir = join(root, 'hub');
const home = join(root, 'home');
asp(['--dir', dir, 'init'], home);
writeFileSync(join(dir, 'from-asp.md'), 'hello from the real asp node\n');
asp(['--dir', dir, 'commit'], home);

const hub = Bun.spawn(
  [ASP, '--dir', dir, 'watch', '--listen', '--no-tls', '--port', '0', '--auth-key', 'S'],
  { env: { ...process.env, ASP_HOME: home, ASP_LOG: 'warn' }, stdout: 'pipe', stderr: 'ignore' },
);

try {
  const port = await readPort(hub.stdout);
  const url = `ws://127.0.0.1:${port}`;
  const api = createNetwork({ latencyMs: 30, debounceMs: 10 });

  // A demo node that clones from the REAL peer (asp clone <url>).
  const id = api.addNode({ name: 'browser', externalUrl: url, authKey: 'S' });
  const filesOf = () => {
    const n = api.snapshot().nodes.find((x) => x.id === id);
    const out = {};
    for (const f of Object.values(n.files)) if (!f.deleted) out[f.path] = api.fileText(id, f.path);
    return out;
  };
  const snap = () => api.snapshot().nodes.find((x) => x.id === id);

  const pulled = await waitFor(() => filesOf()['from-asp.md'] === 'hello from the real asp node\n');
  check('demo node pulled the native vault over ws://', pulled);
  check('demo node adopted the native vault id', snap().site && snap().rowCount > 0);
  await waitFor(() => snap().syncing === false);
  check('connection is LIVE (persistent watch, not one-shot)', snap().live === true);

  // realtime SEND: author on the demo node → native receives with NO re-sync.
  api.createFile(id, '', 'from-browser.md');
  const f = Object.values(snap().files).find((x) => x.path === 'from-browser.md');
  api.stageEdit(id, f.file_id, 'hello from the wasm demo node\n');
  api.commitNow(id, f.file_id);
  const hubFile = join(dir, 'from-browser.md');
  const got = await waitFor(() => existsSync(hubFile) && readFileSync(hubFile, 'utf8') === 'hello from the wasm demo node\n');
  check('realtime push: native node received the demo edit live (no re-sync)', got);

  // realtime RECEIVE: change a file on the native side → demo node gets it live.
  writeFileSync(join(dir, 'from-asp.md'), 'edited on the real asp node\n');
  const recv = await waitFor(() => filesOf()['from-asp.md'] === 'edited on the real asp node\n', 12000);
  check('realtime receive: demo node got the native edit live (no re-sync)', recv);

  api.disconnectPeer(id);
  await sleep(100);
  check('disconnect stops the live link', snap().live === false);
} finally {
  hub.kill();
}

console.log(fail === 0 ? '\nWS INTEROP: ALL PASS' : `\nWS INTEROP: ${fail} FAILURE(S)`);
process.exit(fail === 0 ? 0 : 1);
