import { describe, expect, it } from 'vitest';
import type { FileEntry } from '../lib/api';
import { allDirPaths, buildTree, firstSelectable, flatten } from './tree';

const f = (path: string, is_dir = false): FileEntry => ({ path, file_id: path, is_dir, merge_class: is_dir ? 'dir' : 'text' });

describe('buildTree', () => {
  it('nests files under implied parents; ALL-CAPS notes float up, then natural order', () => {
    const tree = buildTree([f('README.md'), f('notes/b.md'), f('notes/a.md'), f('z.md')]);
    // README (all-caps stem) sorts first; dirs and files intermix by name after.
    expect(tree.map((n) => n.name)).toEqual(['README.md', 'notes', 'z.md']);
    const notes = tree.find((n) => n.name === 'notes')!;
    expect(notes.type).toBe('dir');
    expect(notes.children!.map((c) => c.name)).toEqual(['a.md', 'b.md']);
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
    expect(collapsed.map((r) => r.node.name)).toEqual(['a.md', 'd']);
    const open = flatten(tree, { d: true });
    expect(open.map((r) => r.node.name)).toEqual(['a.md', 'd', 'c.md']);
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
