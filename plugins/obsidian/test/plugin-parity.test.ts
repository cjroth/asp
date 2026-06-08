// The reference-client surface, end-to-end: the plugin's bridge + sync-controller
// (driving the @asp/sdk thin node) converge byte-identically with the real `asp`
// full node — a vault edited "in Obsidian" (the FakeVault) and a vault edited via
// the CLI reach the same state. Proves the host glue + SDK integration against the
// actual binary (no protocol logic in the plugin).

import { expect, test } from 'bun:test';
import { spawnSync } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { Vault } from '../../../sdks/typescript/src/index.ts';
import { Bridge } from '../src/bridge.ts';
import { SyncController } from '../src/sync-controller.ts';
import { FakeVault } from './mocks/fake-vault.ts';

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
  throw new Error('no port');
}
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

test('Obsidian plugin (bridge + controller) converges with the real asp CLI', async () => {
  if (!existsSync(ASP)) throw new Error(`build the binary first: cargo build -p asp`);
  const root = mkdtempSync(join(tmpdir(), 'asp-obsidian-'));
  const dir = join(root, 'A');
  const home = join(root, 'home-a');

  // The CLI side authors a note and listens.
  asp(['--dir', dir, 'init'], home);
  writeFileSync(join(dir, 'cli-note.md'), '# from the CLI\n');
  asp(['--dir', dir, 'commit'], home);
  const hub = Bun.spawn(
    [ASP, '--dir', dir, 'watch', '--listen', '--no-tls', '--port', '0', '--auth-key', 'S'],
    { env: { ...process.env, ASP_HOME: home, ASP_LOG: 'warn' }, stdout: 'pipe', stderr: 'ignore' },
  );
  try {
    const port = await readPort(hub.stdout as ReadableStream<Uint8Array>);
    const url = `ws://127.0.0.1:${port}`;

    // The "Obsidian" side: a FakeVault with a locally-edited note, wired through
    // the plugin's bridge + controller over an in-memory SDK thin node.
    const host = new FakeVault();
    host.setText('obsidian-note.md', '# typed in Obsidian\n');
    const sdk = new Vault(new Uint8Array(32).fill(7), '');
    const bridge = new Bridge(sdk, host);
    const controller = new SyncController(sdk, bridge);

    // Initial sync captures the whole host tree (the plugin does this once at
    // startup / on manual sync); the hot path skips reconcile.
    await controller.syncOnce({ peerUrl: url, authKey: 'S' }, { reconcile: true });

    // The plugin pulled the CLI note into the Obsidian vault...
    expect(host.getText('cli-note.md')).toBe('# from the CLI\n');
    expect(controller.state).toBe('connected');

    // ...and the CLI received the Obsidian note (materialized to disk).
    const cliCopy = join(dir, 'obsidian-note.md');
    const deadline = Date.now() + 8000;
    while (!existsSync(cliCopy) && Date.now() < deadline) await sleep(100);
    expect(existsSync(cliCopy)).toBe(true);
    expect(readFileSync(cliCopy, 'utf8')).toBe('# typed in Obsidian\n');

    // A PEER-side change must reach the plugin on a LATER sync — not only at
    // first connect. (This is the "renamed on another device, never shows up in
    // Obsidian" bug: the thin node is one-shot, so without a re-sync the change
    // never arrives.) The hot-path sync uses reconcile:false, exactly like the
    // plugin's periodic poll.
    writeFileSync(join(dir, 'peer-added.md'), '# added on the peer\n');
    let pulled = false;
    const dl2 = Date.now() + 8000;
    while (!pulled && Date.now() < dl2) {
      await controller.syncOnce({ peerUrl: url, authKey: 'S' }, { reconcile: false });
      if (host.getText('peer-added.md') === '# added on the peer\n') pulled = true;
      else await sleep(300);
    }
    expect(pulled).toBe(true);
  } finally {
    hub.kill();
  }
});
