// Regression: files deleted while the app was closed must not resurrect.
//
// Deletes are only captured live (Obsidian's `delete` event). A file removed
// while the plugin wasn't running authors no delete row anywhere; the old
// write-only startup reconcile then left the engine's copy in place, and
// materializeToHost saw "engine has it, disk doesn't" → wrote the folder right
// back. The fix: on a WARM engine (restored state / already synced), reconcile
// diffs disk against the engine tree and authors deletes for the missing paths
// (`captureDeletes`). On a COLD engine it must stay off — there, "missing from
// disk" just means "not materialized yet".
import { expect, test } from 'bun:test';
import { Vault } from '../../../sdks/typescript/src/index.ts';
import { Bridge } from '../src/bridge.ts';
import { PathFilter } from '../src/path-filter.ts';
import { FakeVault } from './mocks/fake-vault.ts';

const enc = (s: string) => new TextEncoder().encode(s);
const allRows = (v: Vault) => (v as any).eng.rows_after(JSON.stringify({}));
const integrate = (v: Vault, rows: string) => (v as any).eng.integrate(rows);

/** A device that synced { a.md, dir/b.md, dir/c.md } and persisted its state. */
function syncedDevice() {
  const engine = new Vault(new Uint8Array(32).fill(1), 'v');
  const host = new FakeVault();
  for (const [p, c] of [
    ['a.md', 'alpha\n'],
    ['dir/b.md', 'beta\n'],
    ['dir/c.md', 'gamma\n'],
  ]) {
    host.setText(p, c);
    engine.writeFile(p, enc(c));
  }
  return { engine, host, state: engine.dumpState() };
}

test('warm reconcile captures offline deletions — the folder stays gone and propagates', async () => {
  const dev = syncedDevice();
  // The hub holds the same rows (the device had synced before it was closed).
  const hub = new Vault(new Uint8Array(32).fill(9), 'v');
  hub.loadState(dev.state);

  // While the app is closed: the user deletes the whole `dir/` folder.
  await dev.host.remove('dir/b.md');
  await dev.host.remove('dir/c.md');

  // Next launch: fresh engine, warm-restored from the persisted snapshot.
  const fresh = new Vault(new Uint8Array(32).fill(1), '');
  fresh.loadState(dev.state);
  const bridge = new Bridge(fresh as never, dev.host as never, new PathFilter());
  await bridge.reconcileFromHost({ captureDeletes: true });

  // The engine now agrees with the disk…
  expect(Object.keys(fresh.files()).sort()).toEqual(['a.md']);

  // …materialize does NOT resurrect the folder…
  await bridge.materializeToHost();
  expect(await dev.host.list()).toEqual(['a.md']);

  // …and the delete rows propagate, so the hub drops the folder too.
  integrate(hub, allRows(fresh));
  expect(Object.keys(hub.files()).sort()).toEqual(['a.md']);
});

test('cold reconcile (captureDeletes off) authors no deletes', async () => {
  const dev = syncedDevice();
  // Cold start: the engine was adopted from the hub (it holds all three
  // files), but the DISK is missing one — on a cold engine that must not be
  // read as a deletion (default behavior, no opts).
  const cold = new Vault(new Uint8Array(32).fill(2), '');
  cold.loadState(dev.state); // stands in for the adopt-first pull
  await dev.host.remove('dir/b.md');

  const bridge = new Bridge(cold as never, dev.host as never, new PathFilter());
  await bridge.reconcileFromHost();
  expect(Object.keys(cold.files()).sort()).toEqual(['a.md', 'dir/b.md', 'dir/c.md']);
});

test('locally-ignored paths are exempt — ignoring a file must not delete it vault-wide', async () => {
  const dev = syncedDevice();
  // This device ignores `dir/` (e.g. via .aspignore) — so dir/* is neither on
  // its disk nor staged, but other devices still sync it.
  await dev.host.remove('dir/b.md');
  await dev.host.remove('dir/c.md');

  const fresh = new Vault(new Uint8Array(32).fill(1), '');
  fresh.loadState(dev.state);
  const bridge = new Bridge(fresh as never, dev.host as never, new PathFilter('dir/'));
  await bridge.reconcileFromHost({ captureDeletes: true });

  // The ignored paths survive in the engine (no delete rows authored).
  expect(Object.keys(fresh.files()).sort()).toEqual(['a.md', 'dir/b.md', 'dir/c.md']);
});

test('full launch cycle: delete while closed, relaunch, sync — no resurrection on any device', async () => {
  // Device A and the hub in sync; A persists state; app closes; user deletes
  // a folder via the OS; A relaunches (warm restore → reconcile → "sync" →
  // materialize, the plugin's startup order); hub must converge to the delete
  // and A's disk must not see the folder come back.
  const dev = syncedDevice();
  const hub = new Vault(new Uint8Array(32).fill(9), 'v');
  hub.loadState(dev.state);

  await dev.host.remove('dir/b.md');
  await dev.host.remove('dir/c.md');

  const relaunched = new Vault(new Uint8Array(32).fill(1), '');
  relaunched.loadState(dev.state);
  const bridge = new Bridge(relaunched as never, dev.host as never, new PathFilter());

  // The warm startup order (sync-controller.syncOnce with reconcile+deletes):
  await bridge.reconcileFromHost({ captureDeletes: true });
  integrate(hub, allRows(relaunched)); // push
  integrate(relaunched, allRows(hub)); // pull
  await bridge.materializeToHost();

  expect(await dev.host.list()).toEqual(['a.md']);
  expect(Object.keys(hub.files()).sort()).toEqual(['a.md']);

  // And the NEXT launch stays stable (no oscillation): restore the new state,
  // reconcile again — nothing changes.
  const next = new Vault(new Uint8Array(32).fill(1), '');
  next.loadState(relaunched.dumpState());
  const bridge2 = new Bridge(next as never, dev.host as never, new PathFilter());
  await bridge2.reconcileFromHost({ captureDeletes: true });
  await bridge2.materializeToHost();
  expect(await dev.host.list()).toEqual(['a.md']);
  expect(Object.keys(next.files()).sort()).toEqual(['a.md']);
});
