// Branches in the SDK (§2, §7): the wasm node has the same branch model as the
// native engine — scoped views over the shared log, with every branch (not just
// HEAD) syncing between peers. These run the REAL wasm engine, so a pass proves
// the TS SDK exposes branching that converges byte-identically to native.

import { expect, test } from 'bun:test';
import { MAIN_BRANCH, Vault } from '../src/index.ts';

const enc = (s: string) => new TextEncoder().encode(s);
const dec = new TextDecoder();
const text = (v: Vault, p: string) => {
  const b = v.readFile(p);
  return b ? dec.decode(b) : undefined;
};
// Ship every row x holds into y (and back) — the same exchange the persistence
// tests use; it carries content AND Kind::Branch records.
const allRows = (v: Vault) => (v as any).eng.rows_after(JSON.stringify({}));
function exchange(x: Vault, y: Vault) {
  (y as any).eng.integrate(allRows(x));
  (x as any).eng.integrate(allRows(y));
}

test('fork-in-the-past creates an isolated branch; main is untouched', () => {
  const v = new Vault(new Uint8Array(32).fill(1), 'v');
  v.writeFile('a.md', enc('m1\n'));
  expect(v.currentBranch()).toBe(MAIN_BRANCH);

  const b = v.forkAt('feature', 2 ** 53); // fork "now" (t = far future)
  expect(v.currentBranch()).toBe(b);
  expect(text(v, 'a.md')).toBe('m1\n');

  v.writeFile('a.md', enc('b2\n'));
  v.writeFile('only-branch.md', enc('x\n'));
  expect(text(v, 'a.md')).toBe('b2\n');

  // main is isolated.
  v.checkout(MAIN_BRANCH);
  expect(text(v, 'a.md')).toBe('m1\n');
  expect(v.readFile('only-branch.md')).toBeUndefined();
  // back to the branch.
  v.checkout(b);
  expect(text(v, 'a.md')).toBe('b2\n');

  // branch list + delete rules.
  expect(v.branches().some((x) => x.branch_id === b && x.name === 'feature')).toBe(true);
  expect(() => v.deleteBranch(MAIN_BRANCH)).toThrow();
  v.deleteBranch(b);
  expect(v.currentBranch()).toBe(MAIN_BRANCH);
  expect(v.branches().some((x) => x.branch_id === b)).toBe(false);
});

test('ALL branches sync between peers, not just the checked-out one', () => {
  const a = new Vault(new Uint8Array(32).fill(1), 'v');
  a.writeFile('a.md', enc('m1\n'));
  const b = a.forkAt('feature', 2 ** 53);
  a.writeFile('a.md', enc('branch2\n'));
  a.writeFile('feat-only.md', enc('x\n'));
  a.checkout(MAIN_BRANCH);
  a.writeFile('a.md', enc('m2\n')); // divergent edit on main

  // B catches up everything A holds.
  const peer = new Vault(new Uint8Array(32).fill(2), 'v');
  exchange(a, peer);

  // B is on main and converges main's state.
  expect(text(peer, 'a.md')).toBe('m2\n');
  expect(peer.readFile('feat-only.md')).toBeUndefined();
  // B learned the branch purely from sync.
  expect(peer.branches().some((x) => x.branch_id === b && x.name === 'feature')).toBe(true);
  // B can check out the synced branch and see its isolated state.
  peer.checkout(b);
  expect(text(peer, 'a.md')).toBe('branch2\n');
  expect(text(peer, 'feat-only.md')).toBe('x\n');
});

test('concurrent same-name branch creation converges to two distinct branches', () => {
  // §7: two devices create a branch with the SAME name → two distinct branch_ids
  // (derive_id includes the authoring site) → both converge everywhere.
  const a = new Vault(new Uint8Array(32).fill(1), 'v');
  a.writeFile('a.md', enc('m\n'));
  const b = new Vault(new Uint8Array(32).fill(2), 'v');
  exchange(a, b); // shared base

  const ida = a.createBranch('feature');
  const idb = b.createBranch('feature');
  expect(ida).not.toBe(idb);

  exchange(a, b);
  for (const v of [a, b]) {
    const ids = v.branches().map((x) => x.branch_id);
    expect(ids).toContain(ida);
    expect(ids).toContain(idb);
  }
});
