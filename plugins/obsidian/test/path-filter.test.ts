import { expect, test } from 'bun:test';
import { PathFilter } from '../src/path-filter.ts';

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
