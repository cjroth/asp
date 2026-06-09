// SDK ⇄ real-`asp` parity (§Testing): a wasm/TS node and the native `asp` binary
// converge bidirectionally. Spawns the real listener, runs the wasm node's
// handshake + version-vector catch-up over a WebSocket, and asserts both sides
// end up byte-identical — the cross-surface gate against the actual binary.

import { expect, test } from 'bun:test';
import { spawnSync } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { Vault } from '../src/index.ts';

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, '..', '..', '..');
const ASP = join(repoRoot, 'target', 'debug', 'asp');

function asp(args: string[], home: string) {
  const r = spawnSync(ASP, args, { env: { ...process.env, ASP_HOME: home, ASP_LOG: 'warn' } });
  if (r.status !== 0) throw new Error(`asp ${args.join(' ')} failed: ${r.stderr}`);
}

async function readPort(stream: ReadableStream<Uint8Array>): Promise<number> {
  const reader = stream.getReader();
  const dec = new TextDecoder();
  let buf = '';
  const deadline = Date.now() + 15000;
  while (Date.now() < deadline) {
    const { value, done } = await reader.read();
    if (done) break;
    buf += dec.decode(value);
    const m = buf.match(/:\/\/0\.0\.0\.0:(\d+)/);
    if (m) {
      reader.releaseLock();
      return Number.parseInt(m[1], 10);
    }
  }
  throw new Error('asp listener did not announce a port');
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

test('wasm SDK and native asp converge bidirectionally through a relay', async () => {
  if (!existsSync(ASP)) throw new Error(`build the binary first: cargo build -p asp (looked at ${ASP})`);
  const root = mkdtempSync(join(tmpdir(), 'asp-parity-'));
  const dir = join(root, 'A');
  const home = join(root, 'home-a');

  // Native side: init + author a file, then listen (relay).
  asp(['--dir', dir, 'init'], home);
  writeFileSync(join(dir, 'from-asp.md'), 'hello from asp\n');
  asp(['--dir', dir, 'commit'], home);

  const hub = Bun.spawn(
    [ASP, '--dir', dir, 'watch', '--listen', '--no-tls', '--port', '0', '--auth-key', 'S'],
    { env: { ...process.env, ASP_HOME: home, ASP_LOG: 'warn' }, stdout: 'pipe', stderr: 'ignore' },
  );
  try {
    const port = await readPort(hub.stdout as ReadableStream<Uint8Array>);
    const url = `ws://127.0.0.1:${port}`;

    // wasm/TS thin node: author its own file, then sync (adopts the vault, pulls
    // asp's rows, pushes its own).
    const seed = new Uint8Array(32).fill(9);
    const v = new Vault(seed, '');
    v.writeFile('from-wasm.md', 'hello from wasm\n');
    await v.sync(url, { authKey: 'S', idleMs: 500 });

    // The wasm node pulled the native file.
    expect(v.readTextFile('from-asp.md')).toBe('hello from asp\n');
    expect(v.vaultId().length).toBeGreaterThan(0); // adopted the native vault id

    // The native node received + materialized the wasm node's file.
    const aspFile = join(dir, 'from-wasm.md');
    const deadline = Date.now() + 8000;
    while (!existsSync(aspFile) && Date.now() < deadline) await sleep(100);
    expect(existsSync(aspFile)).toBe(true);
    expect(readFileSync(aspFile, 'utf8')).toBe('hello from wasm\n');
  } finally {
    hub.kill();
  }
});

test('wasm node and native asp converge a concurrent edit (3-way merge across surfaces)', async () => {
  if (!existsSync(ASP)) throw new Error('build the binary first');
  const root = mkdtempSync(join(tmpdir(), 'asp-parity2-'));
  const dir = join(root, 'A');
  const home = join(root, 'home-a');

  asp(['--dir', dir, 'init'], home);
  writeFileSync(join(dir, 'doc.md'), 'l1\nl2\nl3\n');
  asp(['--dir', dir, 'commit'], home);

  const hub = Bun.spawn(
    [ASP, '--dir', dir, 'watch', '--listen', '--no-tls', '--port', '0', '--auth-key', 'S'],
    { env: { ...process.env, ASP_HOME: home, ASP_LOG: 'warn' }, stdout: 'pipe', stderr: 'ignore' },
  );
  try {
    const port = await readPort(hub.stdout as ReadableStream<Uint8Array>);
    const url = `ws://127.0.0.1:${port}`;

    // wasm node clones (pulls doc.md), edits a different line concurrently, syncs.
    const v = new Vault(new Uint8Array(32).fill(5), '');
    await v.sync(url, { authKey: 'S', idleMs: 500 });
    expect(v.readTextFile('doc.md')).toBe('l1\nl2\nl3\n');

    // Native edits line 1; wasm edits line 3 — disjoint, both survive.
    writeFileSync(join(dir, 'doc.md'), 'L1\nl2\nl3\n');
    asp(['--dir', dir, 'commit'], home);
    v.writeFile('doc.md', 'l1\nl2\nL3\n');
    await v.sync(url, { authKey: 'S', idleMs: 500 });
    await v.sync(url, { authKey: 'S', idleMs: 500 }); // second round to pull native's edit back

    expect(v.readTextFile('doc.md')).toBe('L1\nl2\nL3\n');
    expect(readFileSync(join(dir, 'doc.md'), 'utf8')).toBe('L1\nl2\nL3\n');
  } finally {
    hub.kill();
  }
});
