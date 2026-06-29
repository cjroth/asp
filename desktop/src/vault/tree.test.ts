import { describe, expect, it } from '../test-shim';
import type { FileEntry } from '../lib/api';
import { allDirPaths, buildTree, firstSelectable, flatten } from './tree';

const f = (path: string, is_dir = false): FileEntry => ({ path, file_id: path, is_dir, merge_class: is_dir ? 'dir' : 'text' });

describe('buildTree', () => {
  it('nests files under implied parents; ALL-CAPS notes float up, then folders, then files', () => {
    const tree = buildTree([f('README.md'), f('notes/b.md'), f('notes/a.md'), f('z.md')]);
    // README (all-caps stem) sorts first; then the 'notes' folder; then 'z.md'.
    expect(tree.map((n) => n.name)).toEqual(['README.md', 'notes', 'z.md']);
    const notes = tree.find((n) => n.name === 'notes')!;
    expect(notes.type).toBe('dir');
    expect(notes.children!.map((c) => c.name)).toEqual(['a.md', 'b.md']);
  });

  it('pins the full group order: ALL-CAPS files, then folders, then files (natural within each)', () => {
    // Mixed bag in deliberately scrambled input order. The contract:
    //   1. ALL-CAPS-stem files first  -> README.md, TODO.md  (natural: R < T)
    //   2. then folders               -> apple, zebra        (natural: a < z)
    //   3. then everyone else (files) -> notes.md
    const tree = buildTree([f('notes.md'), f('zebra', true), f('TODO.md'), f('apple', true), f('README.md')]);
    expect(tree.map((n) => n.name)).toEqual(['README.md', 'TODO.md', 'apple', 'zebra', 'notes.md']);
    // And the order survives flatten() unchanged (this is the order the UI renders).
    expect(flatten(tree, {}).map((r) => r.node.name)).toEqual(['README.md', 'TODO.md', 'apple', 'zebra', 'notes.md']);
  });

  it('puts folders before files even when the file name sorts first alphabetically', () => {
    const tree = buildTree([f('apple.md'), f('zebra/x.md')]);
    // 'zebra' is a folder, so it comes before 'apple.md' despite a > z ordering.
    expect(tree.map((n) => n.name)).toEqual(['zebra', 'apple.md']);
  });

  it('keeps ALL-CAPS notes above folders', () => {
    const tree = buildTree([f('LICENSE'), f('src/main.ts'), f('readme-lower.md')]);
    // LICENSE (all-caps) floats above the 'src' folder; lower-case file last.
    expect(tree.map((n) => n.name)).toEqual(['LICENSE', 'src', 'readme-lower.md']);
  });

  it('orders numerically (note-2 before note-10)', () => {
    const tree = buildTree([f('note-10.md'), f('note-2.md'), f('note-1.md')]);
    expect(tree.map((n) => n.name)).toEqual(['note-1.md', 'note-2.md', 'note-10.md']);
  });

  it('includes explicit empty directories', () => {
    const tree = buildTree([f('empty', true), f('a.md')]);
    expect(tree.find((n) => n.name === 'empty' && n.type === 'dir')).toBeTruthy();
  });

  it('skips empty paths and ranks dotfiles / no-extension / numeric stems', () => {
    const tree = buildTree([f(''), f('.gitignore'), f('Makefile'), f('123.md'), f('READ.md')]);
    // '' is skipped; READ.md (all-caps stem) floats first, then natural order.
    expect(tree.map((n) => n.name)).toEqual(['READ.md', '.gitignore', '123.md', 'Makefile']);
  });
});

describe('flatten honors expanded state', () => {
  it('hides children of collapsed dirs and indents by depth', () => {
    const tree = buildTree([f('d/c.md'), f('a.md')]);
    const collapsed = flatten(tree, {});
    // Folder 'd' sorts before file 'a.md'.
    expect(collapsed.map((r) => r.node.name)).toEqual(['d', 'a.md']);
    const open = flatten(tree, { d: true });
    expect(open.map((r) => r.node.name)).toEqual(['d', 'c.md', 'a.md']);
    expect(open.find((r) => r.node.name === 'c.md')!.depth).toBe(1);
  });
});

describe('allDirPaths', () => {
  it('lists every directory path', () => {
    const tree = buildTree([f('a/b/c.md')]);
    expect(allDirPaths(tree).sort()).toEqual(['a', 'a/b']);
  });
});

describe('firstSelectable', () => {
  it('prefers a README at any depth, else the first file', () => {
    expect(firstSelectable(buildTree([f('x.md'), f('docs/README.md')]))).toBe('docs/README.md');
    expect(firstSelectable(buildTree([f('x.md'), f('y.md')]))).toBe('x.md');
    expect(firstSelectable(buildTree([f('only', true)]))).toBeNull();
  });
});
