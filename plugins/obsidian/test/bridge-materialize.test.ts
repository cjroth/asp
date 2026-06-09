// Regression: materializeToHost must scale to large vaults and be incremental.
//
// The old path pulled EVERY file's content at once via files() → files_json
// serialized all bytes into one giant JSON number-array string, which OOMed /
// truncated the worker on a large vault ("Unexpected end of JSON input"). The
// fixed path lists cheap per-file metadata (filesDetail) and fetches only
// changed files' bytes one at a time (readFile), with a path→hash cache. This
// asserts correctness + incrementality (and exercises a multi-MB file that the
// old JSON path choked on).
import { expect, test } from 'bun:test';
import { Vault } from '../../../sdks/typescript/src/index.ts';
import { Bridge } from '../src/bridge.ts';
import { PathFilter } from '../src/path-filter.ts';

class FakeHost {
  files = new Map<string, Uint8Array>();
  async read(p: string) {
    return this.files.get(p) ?? null;
  }
  async write(p: string, b: Uint8Array) {
    this.files.set(p, b);
  }
  async remove(p: string) {
    this.files.delete(p);
  }
  async list() {
    return [...this.files.keys()];
  }
}

const dec = new TextDecoder();
const enc = (s: string) => new TextEncoder().encode(s);

test('materializeToHost scales + is incremental (writes all, then nothing, then only deltas)', async () => {
  const v = new Vault(new Uint8Array(32).fill(3), '');
  const N = 1500;
  const files: Record<string, Uint8Array> = {};
  for (let i = 0; i < N; i++) files[`notes/n${i}.md`] = enc(`file ${i}`);
  files['assets/big.bin'] = new Uint8Array(3 * 1024 * 1024).fill(7); // 3 MiB
  v.writeFiles(files);

  const host = new FakeHost();
  const bridge = new Bridge(v as never, host as never, new PathFilter());

  // First materialize: writes the whole tree.
  const r1 = await bridge.materializeToHost();
  expect(r1.written).toBe(N + 1);
  expect(host.files.size).toBe(N + 1);
  expect(dec.decode(host.files.get('notes/n7.md')!)).toBe('file 7');
  expect(host.files.get('assets/big.bin')!.length).toBe(3 * 1024 * 1024);

  // Second materialize: nothing changed → writes nothing (incremental cache).
  const r2 = await bridge.materializeToHost();
  expect(r2.written).toBe(0);
  expect(r2.removed).toBe(0);

  // Change one file → only that file is rewritten.
  v.writeFile('notes/n7.md', enc('changed'));
  const r3 = await bridge.materializeToHost();
  expect(r3.written).toBe(1);
  expect(dec.decode(host.files.get('notes/n7.md')!)).toBe('changed');

  // Delete one file → removed from the host.
  v.deleteFile('notes/n3.md');
  const r4 = await bridge.materializeToHost();
  expect(r4.removed).toBe(1);
  expect(host.files.has('notes/n3.md')).toBe(false);
});
