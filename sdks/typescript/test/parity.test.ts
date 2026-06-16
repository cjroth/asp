// SDK ⇄ real-`asp` parity (§Testing): a wasm/TS node and the native `asp` binary
// converge bidirectionally **over iroh**. A browser/wasm node can't do UDP, so
// iroh relays its QUIC over a WebSocket — the test stands up a local `asp relay`,
// points the native listener and the wasm node at it, and asserts both sides end
// up byte-identical. The cross-surface gate against the actual binary.

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

/** Read a process's stdout until a line matching `re`; return the first capture. */
async function readMatch(stream: ReadableStream<Uint8Array>, re: RegExp, ms = 20000): Promise<string> {
  const reader = stream.getReader();
  const dec = new TextDecoder();
  let buf = '';
  const deadline = Date.now() + ms;
  while (Date.now() < deadline) {
    const { value, done } = await reader.read();
    if (done) break;
    buf += dec.decode(value);
    const m = buf.match(re);
    if (m) {
      reader.releaseLock();
      return m[1];
    }
  }
  throw new Error(`process did not emit ${re}`);
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** A local iroh relay so browser/wasm nodes (no UDP) can reach the native peer. */
async function startRelay(addr: string) {
  const relay = Bun.spawn([ASP, 'relay', '--listen-addr', addr], {
    env: { ...process.env, ASP_LOG: 'warn' },
    stdout: 'pipe',
    stderr: 'ignore',
  });
  await readMatch(relay.stdout as ReadableStream<Uint8Array>, /relay listening on (http:\/\/\S+)/);
  return relay;
}

test('wasm SDK and native asp converge bidirectionally through a relay', async () => {
  if (!existsSync(ASP)) throw new Error(`build the binary first: cargo build -p asp (looked at ${ASP})`);
  const root = mkdtempSync(join(tmpdir(), 'asp-parity-'));
  const dir = join(root, 'A');
  const home = join(root, 'home-a');
  const relayPort = 20000 + Math.floor(Math.random() * 20000);
  const relayAddr = `127.0.0.1:${relayPort}`;
  const relayUrl = `http://${relayAddr}`;

  const relay = await startRelay(relayAddr);
  // Native side: init + author a file, then listen (relayed via the local relay).
  asp(['--dir', dir, 'init'], home);
  writeFileSync(join(dir, 'from-asp.md'), 'hello from asp\n');
  asp(['--dir', dir, 'commit'], home);

  const hub = Bun.spawn(
    [ASP, '--dir', dir, '--relay-url', relayUrl, 'watch', '--listen', '--auth-key', 'S'],
    { env: { ...process.env, ASP_HOME: home, ASP_LOG: 'warn' }, stdout: 'pipe', stderr: 'ignore' },
  );
  try {
    const ticket = await readMatch(hub.stdout as ReadableStream<Uint8Array>, /^ticket: (\S+)/m);

    // wasm/TS thin node: author its own file, then sync over iroh (adopts the
    // vault, pulls asp's rows, pushes its own) — relayed through the local relay.
    const seed = new Uint8Array(32).fill(9);
    const v = new Vault(seed, '');
    v.writeFile('from-wasm.md', 'hello from wasm\n');
    await v.sync(ticket, { authKey: 'S', relayUrl });

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
    relay.kill();
  }
}, 60000);
