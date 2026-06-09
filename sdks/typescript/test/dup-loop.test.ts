// Regression: the duplicate-explosion feedback loop.
//
// A non-persisted node recreates its engine every reload and reconciles from
// disk. If it reconciles the disk BEFORE adopting the peer's canonical state,
// it re-imports the fold's OWN disambiguated names (`a (1).md`) as brand-new
// files; on merge they collide and re-disambiguate, DOUBLING every reload. The
// fix (sync-controller: adopt-before-reconcile) keeps the file set bounded.
//
// This guards the invariant the plugin's syncOnce now relies on: a node that
// adopts peer state before reconciling its disk does not multiply files.
import { expect, test } from 'bun:test';
import { Vault } from '../src/index.ts';

const enc = (s: string) => new TextEncoder().encode(s);
const allRows = (v: any) => v.eng.rows_after(JSON.stringify({}));
function exchange(x: any, y: any) {
  const rx = allRows(x);
  const ry = allRows(y);
  y.eng.integrate(rx);
  x.eng.integrate(ry);
}

// Two devices independently create the same path — a legitimate collision the
// fold resolves with one ` (1)` suffix. This must NOT grow on repeated reloads.
function setup() {
  const A = new Vault(new Uint8Array(32).fill(1), '');
  A.writeFile('a.md', enc('A'));
  const B = new Vault(new Uint8Array(32).fill(2), '');
  B.writeFile('a.md', enc('B'));
  exchange(A, B);
  return { A, B };
}

test('BUG SHAPE: reconcile-before-adopt doubles files every reload', () => {
  const { A, B } = setup();
  let disk = A.files();
  const counts: number[] = [];
  for (let i = 0; i < 3; i++) {
    const fresh = new Vault(new Uint8Array(32).fill(1), '');
    fresh.writeFiles(disk); // reconcile FIRST (the bug)
    exchange(fresh, B); // then adopt peer
    disk = fresh.files();
    counts.push(Object.keys(disk).length);
  }
  // Documents the runaway: strictly increasing (2→4→8…).
  expect(counts[1]).toBeGreaterThan(counts[0]);
  expect(counts[2]).toBeGreaterThan(counts[1]);
});

test('FIX: a fresh node adopts peer ids before reconciling same-path files', () => {
  // The peer (hub) already holds the files; a fresh client's DISK has the same
  // paths (e.g. it was synced before but lost its engine state). Reconciling
  // directly would mint NEW ids that collide with the peer's → duplication.
  // A pristine hub holds the canonical rows. (Use a fresh hub per scenario —
  // `exchange` MUTATES the hub by integrating the client's rows, so a polluted
  // hub would no longer be a clean peer for the next scenario.)
  const mkHub = () => {
    const h = new Vault(new Uint8Array(32).fill(9), '');
    h.writeFiles({ 'a.md': enc('A'), 'b.md': enc('B') });
    return h;
  };
  const disk = mkHub().files(); // client disk == hub content/paths

  // BUG: reconcile-first on a fresh engine, then exchange → collisions multiply.
  const bad = new Vault(new Uint8Array(32).fill(8), '');
  bad.writeFiles(disk); // fresh ids
  exchange(bad, mkHub());
  expect(Object.keys(bad.files()).length).toBeGreaterThan(2); // dups appeared

  // FIX: adopt the peer's rows (ids) FIRST, then reconcile the disk — matches by
  // path, reuses the peer's ids, no new files.
  const good = new Vault(new Uint8Array(32).fill(7), '');
  good.eng.integrate(mkHub().eng.rows_after(JSON.stringify({}))); // adopt-first (pull)
  good.writeFiles(disk); // reconcile — matches existing ids
  expect(Object.keys(good.files()).length).toBe(2); // a.md + b.md, no dups
});

test('FIX: restoring persisted engine state before reconcile keeps it bounded', () => {
  const { A, B } = setup();
  const baseline = Object.keys(A.files()).length; // 2: "a.md" + "a (1).md"
  let disk = A.files();
  let state = A.dump(); // the engine state a thin client persists across reloads
  for (let i = 0; i < 5; i++) {
    const fresh = new Vault(new Uint8Array(32).fill(1), '');
    fresh.load(state); // RESTORE persisted state (the fix) — engine knows file ids
    fresh.writeFiles(disk); // THEN reconcile disk — matches by path, no new files
    exchange(fresh, B);
    disk = fresh.files();
    state = fresh.dump();
    expect(Object.keys(disk).length).toBe(baseline); // never grows
  }
});
