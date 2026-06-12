// Engine state persistence — the compact binary snapshot thin clients save
// across launches (dumpState/loadState), plus the legacy JSON dump it replaces.
//
// Why the format matters: the legacy dump was the wire catch-up (`rows_after`)
// as JSON — every content byte inflated to ~4 chars AND each row bundled its
// base+result blobs, so an edit history duplicated content. On a large mobile
// vault, serializing that one giant string OOM'd the worker → the save failed
// → every launch cold-started (a ~90s full pull) → deletions made while the
// app was closed resurrected. The binary snapshot stores each blob once,
// msgpack-encoded, so persistence survives large vaults.

import { expect, test } from 'bun:test';
import {
  EngineWorkerHost,
  type FromWorker,
  type ToWorker,
  Vault,
  WorkerVault,
  linkedPorts,
} from '../src/index.ts';

const enc = (s: string) => new TextEncoder().encode(s);
const dec = new TextDecoder();

function vaultWithHistory(): Vault {
  const v = new Vault(new Uint8Array(32).fill(1), 'vault-1');
  v.writeFile('a.md', enc('alpha\n'));
  v.writeFile('dir/b.md', enc('beta\n'));
  v.writeFile('a.md', enc('alpha edited\n'));
  v.renameFile('dir/b.md', 'dir/c.md');
  v.writeFile('gone.md', enc('bye\n'));
  v.deleteFile('gone.md');
  return v;
}

test('binary state snapshot round-trips the full engine state', () => {
  const a = vaultWithHistory();
  const snap = a.dumpState();
  expect(snap).toBeInstanceOf(Uint8Array);

  const b = new Vault(new Uint8Array(32).fill(2), '');
  const added = b.loadState(snap);
  expect(added).toBe(a.rowCount());
  expect(b.files()).toEqual(a.files());
  expect(b.vaultId()).toBe('vault-1'); // adopted from the snapshot
  expect(b.loadState(snap)).toBe(0); // idempotent
});

test('binary snapshot stores each blob once — far smaller than the legacy JSON dump', () => {
  const v = new Vault(new Uint8Array(32).fill(1), '');
  const bigA = new Uint8Array(100_000).fill(97);
  const bigB = new Uint8Array(100_000).fill(98);
  // 5 rows over the same two contents: the wire/JSON dump carries base+result
  // blobs per row (~8 copies, ~4 chars per byte); the snapshot carries 2.
  v.writeFile('big.bin', bigA);
  v.writeFile('big.bin', bigB);
  v.writeFile('big.bin', bigA);
  v.writeFile('big.bin', bigB);
  v.writeFile('big.bin', bigA);

  const legacy = v.dump().length;
  const compact = v.dumpState().length;
  expect(compact).toBeLessThan(3 * 100_000); // two blobs + rows + framing
  expect(compact * 10).toBeLessThan(legacy); // an order of magnitude smaller

  const r = new Vault(new Uint8Array(32).fill(2), '');
  r.loadState(v.dumpState());
  expect(r.files()['big.bin']).toEqual(bigA);
});

test('corrupt or truncated snapshots fail loudly and leave the engine untouched', () => {
  const a = vaultWithHistory();
  const snap = a.dumpState();
  const b = new Vault(new Uint8Array(32).fill(2), '');
  expect(() => b.loadState(snap.slice(0, snap.length / 2))).toThrow();
  expect(() => b.loadState(enc('not a snapshot'))).toThrow();
  expect(b.rowCount()).toBe(0);
});

test('legacy JSON dump still loads (the one-time migration path)', () => {
  const a = vaultWithHistory();
  const json = a.dump();
  const b = new Vault(new Uint8Array(32).fill(2), '');
  b.load(json);
  expect(b.files()).toEqual(a.files());
  // …and re-saving in the compact format round-trips identically.
  const c = new Vault(new Uint8Array(32).fill(3), '');
  c.loadState(b.dumpState());
  expect(c.files()).toEqual(a.files());
});

test('restoring the BINARY snapshot before reconcile keeps the file set bounded', () => {
  // The dup-loop invariant (see dup-loop.test.ts), on the persistence path the
  // plugin actually uses now: restore → reconcile must never multiply files.
  const allRows = (v: Vault) => (v as any).eng.rows_after(JSON.stringify({}));
  const exchange = (x: Vault, y: Vault) => {
    const rx = allRows(x);
    const ry = allRows(y);
    (y as any).eng.integrate(rx);
    (x as any).eng.integrate(ry);
  };
  const A = new Vault(new Uint8Array(32).fill(1), '');
  A.writeFile('a.md', enc('A'));
  const B = new Vault(new Uint8Array(32).fill(2), '');
  B.writeFile('a.md', enc('B'));
  exchange(A, B);

  const baseline = Object.keys(A.files()).length; // 2: "a.md" + "a (1).md"
  let disk = A.files();
  let state = A.dumpState();
  for (let i = 0; i < 5; i++) {
    const fresh = new Vault(new Uint8Array(32).fill(1), '');
    fresh.loadState(state); // restore persisted state — engine knows file ids
    fresh.writeFiles(disk); // then reconcile disk — matches by path
    exchange(fresh, B);
    disk = fresh.files();
    state = fresh.dumpState();
    expect(Object.keys(disk).length).toBe(baseline); // never grows
  }
});

test('deleteFiles authors batch deletes that propagate', () => {
  const a = new Vault(new Uint8Array(32).fill(1), 'v');
  a.writeFile('keep.md', enc('k'));
  a.writeFile('x.md', enc('x'));
  a.writeFile('dir/y.md', enc('y'));
  const b = new Vault(new Uint8Array(32).fill(2), 'v');
  b.loadState(a.dumpState());

  a.deleteFiles(['x.md', 'dir/y.md', 'never-existed.md']);
  expect(Object.keys(a.files()).sort()).toEqual(['keep.md']);

  (b as any).eng.integrate((a as any).eng.rows_after(JSON.stringify({})));
  expect(Object.keys(b.files()).sort()).toEqual(['keep.md']); // deletes propagate
});

test('dumpState/loadState/deleteFiles cross the worker boundary', async () => {
  const [mainA, hostA] = linkedPorts<ToWorker, FromWorker>();
  new EngineWorkerHost(hostA);
  const a = new WorkerVault(mainA);
  await a.init({ seed: new Uint8Array(32).fill(7), vaultId: 'v', wasmBytes: new Uint8Array() });
  await a.writeFile('note.md', enc('hello\n'));
  await a.writeFile('drop.md', enc('bye\n'));
  await a.deleteFiles(['drop.md']);
  const snap = await a.dumpState();
  expect(snap).toBeInstanceOf(Uint8Array);

  const [mainB, hostB] = linkedPorts<ToWorker, FromWorker>();
  new EngineWorkerHost(hostB);
  const b = new WorkerVault(mainB);
  await b.init({ seed: new Uint8Array(32).fill(8), vaultId: '', wasmBytes: new Uint8Array() });
  const added = await b.loadState(snap);
  expect(added).toBeGreaterThan(0);
  const files = await b.files();
  expect(Object.keys(files).sort()).toEqual(['note.md']);
  expect(dec.decode(files['note.md'])).toBe('hello\n');

  await a.free();
  await b.free();
});
