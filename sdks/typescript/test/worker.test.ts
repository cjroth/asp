// The engine Web Worker path, end to end, against the REAL `asp` daemon. The
// worker boundary is simulated by `linkedPorts()` (same request/reply protocol
// as a real Worker, just in-process), so the whole WorkerVault ⇄
// EngineWorkerHost ⇄ wasm-engine ⇄ WebSocket stack runs under `bun test`. Proves
// the off-thread surface converges byte-identically with the native node —
// nothing about moving the engine to a worker changes the protocol.

import { expect, test } from 'bun:test';
import { spawnSync } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { EngineWorkerHost, type FromWorker, type ToWorker, WorkerVault, linkedPorts } from '../src/index.ts';

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, '..', '..', '..');
const ASP = join(repoRoot, 'target', 'debug', 'asp');

function asp(args: string[], home: string) {
  const r = spawnSync(ASP, args, { env: { ...process.env, ASP_HOME: home, ASP_LOG: 'warn' } });
  if (r.status !== 0) throw new Error(`asp ${args.join(' ')} failed: ${r.stderr}`);
}
async function readMatch(stream: ReadableStream<Uint8Array>, re: RegExp, ms = 20000): Promise<string> {
  const reader = stream.getReader();
  const td = new TextDecoder();
  let buf = '';
  const deadline = Date.now() + ms;
  while (Date.now() < deadline) {
    const { value, done } = await reader.read();
    if (done) break;
    buf += td.decode(value);
    const m = buf.match(re);
    if (m) {
      reader.releaseLock();
      return m[1];
    }
  }
  throw new Error(`process did not emit ${re}`);
}
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
const dec = new TextDecoder();
const enc = new TextEncoder();

test('WorkerVault converges with the real asp daemon (off-thread engine path)', async () => {
  if (!existsSync(ASP)) throw new Error(`build the binary first: cargo build -p asp`);
  const root = mkdtempSync(join(tmpdir(), 'asp-worker-'));
  const dir = join(root, 'A');
  const home = join(root, 'home-a');

  const relayPort = 20000 + Math.floor(Math.random() * 20000);
  const relayUrl = `http://127.0.0.1:${relayPort}`;
  const relay = Bun.spawn([ASP, 'relay', '--listen-addr', `127.0.0.1:${relayPort}`], {
    env: { ...process.env, ASP_LOG: 'warn' },
    stdout: 'pipe',
    stderr: 'ignore',
  });
  await readMatch(relay.stdout as ReadableStream<Uint8Array>, /relay listening on (http:\/\/\S+)/);

  asp(['--dir', dir, 'init'], home);
  writeFileSync(join(dir, 'cli-note.md'), '# from the CLI\n');
  asp(['--dir', dir, 'commit'], home);
  const hub = Bun.spawn(
    [ASP, '--dir', dir, '--relay-url', relayUrl, 'watch', '--listen', '--auth-key', 'S'],
    { env: { ...process.env, ASP_HOME: home, ASP_LOG: 'warn' }, stdout: 'pipe', stderr: 'ignore' },
  );
  try {
    const ticket = await readMatch(hub.stdout as ReadableStream<Uint8Array>, /^ticket: (\S+)/m);

    // The worker boundary: `linkedPorts()` is exactly the request/reply channel
    // a real Worker uses, wired in-process so the test is deterministic.
    const [mainSide, workerSide] = linkedPorts<ToWorker, FromWorker>();
    new EngineWorkerHost(workerSide);
    const vault = new WorkerVault(mainSide);

    const id = await vault.init({ seed: new Uint8Array(32).fill(7), vaultId: '', wasmBytes: new Uint8Array() });
    expect(id.nodeSsh).toContain('ssh-ed25519');
    expect(vault.nodeSsh()).toBe(id.nodeSsh); // cached, synchronous

    // Author "in Obsidian", then sync — the engine work runs in the host half.
    await vault.writeFile('obsidian-note.md', enc.encode('# typed in Obsidian\n'));
    const integrated = await vault.sync(ticket, { authKey: 'S', relayUrl });
    expect(integrated).toBeGreaterThan(0); // pulled the CLI note

    const files = await vault.files();
    expect(dec.decode(files['cli-note.md'])).toBe('# from the CLI\n');

    // The CLI received our note.
    const cliCopy = join(dir, 'obsidian-note.md');
    const deadline = Date.now() + 8000;
    while (!existsSync(cliCopy) && Date.now() < deadline) await sleep(100);
    expect(existsSync(cliCopy)).toBe(true);
    expect(readFileSync(cliCopy, 'utf8')).toBe('# typed in Obsidian\n');

    // A later peer-side change reaches us on a subsequent sync (live pull).
    writeFileSync(join(dir, 'peer-added.md'), '# added on the peer\n');
    let pulled = false;
    const dl2 = Date.now() + 8000;
    while (!pulled && Date.now() < dl2) {
      await vault.sync(ticket, { authKey: 'S', relayUrl });
      const f = await vault.files();
      if (f['peer-added.md'] && dec.decode(f['peer-added.md']) === '# added on the peer\n') pulled = true;
      else await sleep(300);
    }
    expect(pulled).toBe(true);

    await vault.free();
  } finally {
    hub.kill();
    relay.kill();
  }
}, 60000);
