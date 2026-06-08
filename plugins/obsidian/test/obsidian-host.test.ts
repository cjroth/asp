// Regression tests for ObsidianHost file I/O against a mock DataAdapter that
// mirrors Obsidian's real semantics: `writeBinary` THROWS when the parent
// folder doesn't exist, and `mkdir` is NOT recursive. This reproduces the
// "parent folder doesn't exist" error a synced note in a fresh subfolder hit,
// and pins the fix (create the parent chain first; prune empty dirs on remove).

import { expect, test } from 'bun:test';
import type { DataAdapter } from 'obsidian';
import { ObsidianHost } from '../src/obsidian-host.ts';

function dirOf(p: string): string {
  const i = p.lastIndexOf('/');
  return i < 0 ? '' : p.slice(0, i);
}

/** In-memory adapter with Obsidian-like, non-forgiving folder semantics. */
class MockAdapter implements DataAdapter {
  files = new Map<string, ArrayBuffer>();
  folders = new Set<string>();

  async exists(path: string): Promise<boolean> {
    return this.files.has(path) || this.folders.has(path);
  }
  async mkdir(path: string): Promise<void> {
    if (this.folders.has(path)) throw new Error('Folder already exists.');
    const parent = dirOf(path);
    if (parent && !this.folders.has(parent)) throw new Error('ENOENT: parent missing');
    this.folders.add(path);
  }
  async writeBinary(path: string, data: ArrayBuffer): Promise<void> {
    const parent = dirOf(path);
    if (parent && !this.folders.has(parent)) {
      throw new Error('parent folder does not exist'); // the real Obsidian error
    }
    this.files.set(path, data);
  }
  async readBinary(path: string): Promise<ArrayBuffer> {
    const b = this.files.get(path);
    if (!b) throw new Error('ENOENT');
    return b;
  }
  async remove(path: string): Promise<void> {
    this.files.delete(path);
  }
  async rmdir(path: string, _recursive: boolean): Promise<void> {
    this.folders.delete(path);
  }
  async list(path: string): Promise<{ files: string[]; folders: string[] }> {
    const files = [...this.files.keys()].filter((p) => dirOf(p) === path);
    const folders = [...this.folders].filter((p) => dirOf(p) === path);
    return { files, folders };
  }
  // Unused by ObsidianHost but required by the interface.
  async read(): Promise<string> {
    throw new Error('not used');
  }
  async write(): Promise<void> {
    throw new Error('not used');
  }
}

test('write() creates missing parent folders (no "parent folder doesn\'t exist")', async () => {
  const a = new MockAdapter();
  const host = new ObsidianHost(a);

  // Deeply nested path, none of whose folders exist yet.
  await host.write('notes/2026/june/day.md', new TextEncoder().encode('# hi\n'));

  expect(a.folders.has('notes')).toBe(true);
  expect(a.folders.has('notes/2026')).toBe(true);
  expect(a.folders.has('notes/2026/june')).toBe(true);
  expect(new TextDecoder().decode(new Uint8Array((await host.read('notes/2026/june/day.md'))!))).toBe(
    '# hi\n',
  );
});

test('write() is idempotent over an existing folder', async () => {
  const a = new MockAdapter();
  const host = new ObsidianHost(a);
  await host.write('f/one.md', new TextEncoder().encode('1'));
  await host.write('f/two.md', new TextEncoder().encode('2')); // folder already there
  expect(a.files.has('f/one.md')).toBe(true);
  expect(a.files.has('f/two.md')).toBe(true);
});

test('a top-level file needs no folder', async () => {
  const a = new MockAdapter();
  const host = new ObsidianHost(a);
  await host.write('root.md', new TextEncoder().encode('r'));
  expect(a.files.has('root.md')).toBe(true);
});

test('remove() prunes now-empty ancestor folders, keeps in-use ones', async () => {
  const a = new MockAdapter();
  const host = new ObsidianHost(a);
  await host.write('p/q/only.md', new TextEncoder().encode('x'));
  await host.write('p/keep.md', new TextEncoder().encode('y'));

  await host.remove('p/q/only.md');

  expect(a.folders.has('p/q')).toBe(false); // emptied → pruned
  expect(a.folders.has('p')).toBe(true); // still holds keep.md → kept
  expect(a.files.has('p/keep.md')).toBe(true);
});
