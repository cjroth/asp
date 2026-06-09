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

test('FIX: adopt-before-reconcile keeps the file set bounded across reloads', () => {
  const { A, B } = setup();
  const baseline = Object.keys(A.files()).length; // 2: "a.md" + "a (1).md"
  let disk = A.files();
  for (let i = 0; i < 5; i++) {
    const fresh = new Vault(new Uint8Array(32).fill(1), '');
    exchange(fresh, B); // ADOPT peer state first
    fresh.writeFiles(disk); // THEN reconcile disk — names match existing ids
    disk = fresh.files();
    expect(Object.keys(disk).length).toBe(baseline); // never grows
  }
});
