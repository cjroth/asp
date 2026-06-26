import { describe, expect, it } from 'vitest';
import type { FileEntry } from '../lib/api';
import { allDirPaths, buildTree, firstSelectable, flatten, freeUntitledName } from './tree';

const f = (path: string, is_dir = false): FileEntry => ({ path, file_id: path, is_dir, merge_class: is_dir ? 'dir' : 'text' });

describe('buildTree', () => {
  it('nests files under implied parent directories, dirs before files, sorted', () => {
    const tree = buildTree([f('README.md'), f('notes/b.md'), f('notes/a.md'), f('z.md')]);
    expect(tree.map((n) => n.name)).toEqual(['notes', 'README.md', 'z.md']);
    const notes = tree.find((n) => n.name === 'notes')!;
    expect(notes.type).toBe('dir');
    expect(notes.children!.map((c) => c.name)).toEqual(['a.md', 'b.md']);
  });

  it('includes explicit empty directories', () => {
    const tree = buildTree([f('empty', true), f('a.md')]);
    expect(tree.find((n) => n.name === 'empty' && n.type === 'dir')).toBeTruthy();
  });
});

describe('flatten honors expanded state', () => {
  it('hides children of collapsed dirs and indents by depth', () => {
    const tree = buildTree([f('d/c.md'), f('a.md')]);
    const collapsed = flatten(tree, {});
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

describe('freeUntitledName', () => {
  it('finds the first free untitled name', () => {
    expect(freeUntitledName([])).toBe('untitled.md');
    expect(freeUntitledName(['untitled.md'])).toBe('untitled-1.md');
    expect(freeUntitledName(['untitled.md', 'untitled-1.md'])).toBe('untitled-2.md');
  });
});
