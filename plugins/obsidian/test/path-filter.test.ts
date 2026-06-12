import { expect, test } from 'bun:test';
import { PathFilter } from '../src/path-filter.ts';

// ── Scope-closure property ────────────────────────────────────────────────
// The host-side mirror of crates/asp-core/tests/scope_closure.rs. PathFilter is
// the ACTUAL gate for Obsidian sync, and the nested-`.git` blind spot (matching
// only the first path segment) lived here too. We assert the filter partitions
// adversarial vault trees by an INDEPENDENT oracle, so a matcher that drifts
// from the spec — e.g. back to checking only the top segment — fails here.
const HARD = ['.asp', '.context', '.git', '.obsidian', '.trash']; // dirs, any depth
const LOOKALIKES = ['git', 'gitland', 'git-tips', '.gitignore', 'context', 'obsidian', 'obsidian-notes'];
const REAL = ['notes', 'a.md', 'b.txt', 'dir', 'src', 'README.md', 'proj', 'deep'];

// Independent oracle for a DEFAULT filter (no .aspignore): ignored iff a segment
// is a hard-ignored dir, or any segment is `.DS_Store`.
function oracleIgnored(path: string): boolean {
  const segs = path.split('/');
  return segs.some((s) => HARD.includes(s) || s === '.DS_Store');
}

// Deterministic PRNG (mulberry32) so failures reproduce from the seed.
function rng(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a |= 0;
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

test('PathFilter partitions adversarial trees by the oracle (scope-closure)', () => {
  const f = new PathFilter(''); // default: only the hard set applies
  const r = rng(0xc0ffee);
  const pick = <T>(arr: T[]) => arr[Math.floor(r() * arr.length)];
  let sawIgnored = 0;
  let sawSynced = 0;
  for (let i = 0; i < 50000; i++) {
    const depth = 1 + Math.floor(r() * 5);
    const segs: string[] = [];
    for (let d = 0; d < depth; d++) {
      const bucket = Math.floor(r() * 10);
      segs.push(bucket < 3 ? pick(HARD) : bucket < 6 ? pick([...LOOKALIKES, '.DS_Store']) : pick(REAL));
    }
    const path = segs.join('/');
    const want = oracleIgnored(path);
    if (f.ignored(path) !== want) throw new Error(`disagreed on ${JSON.stringify(path)}: got ${f.ignored(path)}, want ${want}`);
    want ? sawIgnored++ : sawSynced++;
  }
  // Both sides of the partition must be exercised (guards a one-sided pass).
  expect(sawIgnored).toBeGreaterThan(1000);
  expect(sawSynced).toBeGreaterThan(1000);
});

test('private .asp dir is always excluded', () => {
  const f = new PathFilter('');
  expect(f.ignored('.asp')).toBe(true);
  expect(f.ignored('.asp/asp.db')).toBe(true);
  expect(f.ignored('notes/a.md')).toBe(false);
});

test('editor/vcs/private dirs are hard-ignored regardless of ignore file', () => {
  // Even a hostile ignore file that tries to RE-INCLUDE them must not win:
  // syncing .obsidian (this plugin's own state/binary) is the self-referential
  // loop that exploded the vault; .context holds the node private key.
  const f = new PathFilter('!.obsidian\n!.git\n!.context\n');
  for (const p of [
    '.obsidian',
    '.obsidian/plugins/agent-sync/engine-state.json',
    '.obsidian/plugins/agent-sync/engine-state.bin',
    '.obsidian/plugins/agent-sync/main.js',
    '.git',
    '.git/objects/ab/cdef',
    '.context',
    '.context/id_ed25519',
    '.trash/old.md',
    '.DS_Store',
    'notes/.DS_Store',
  ]) {
    expect(f.ignored(p)).toBe(true);
  }
  // Real notes (including a non-dot "context" folder) still sync.
  expect(f.ignored('context/note.md')).toBe(false);
  expect(f.ignored('notes/plan.md')).toBe(false);
});

test('hard-ignored dirs are caught at ANY depth (nested cloned repos)', () => {
  const f = new PathFilter('');
  // A repo kept as reference material inside the vault — its packs must not sync.
  expect(f.ignored('context/gridland/.git/objects/pack/pack-abc.pack')).toBe(true);
  expect(f.ignored('notes/proj/.obsidian/workspace.json')).toBe(true);
  expect(f.ignored('a/b/.trash/old.md')).toBe(true);
  expect(f.ignored('deep/dir/.DS_Store')).toBe(true);
  // Lookalikes that are NOT the dir still sync.
  expect(f.ignored('notes/git-tips/howto.md')).toBe(false);
  expect(f.ignored('projects/gitland/readme.md')).toBe(false);
  expect(f.ignored('docs/.gitignore')).toBe(false);
});

test('.aspignore globs + negation', () => {
  const f = new PathFilter('*.log\nbuild/\n!keep.log\n');
  expect(f.ignored('debug.log')).toBe(true);
  expect(f.ignored('keep.log')).toBe(false);
  expect(f.ignored('build/out.js')).toBe(true);
  expect(f.ignored('notes/plan.md')).toBe(false);
});
