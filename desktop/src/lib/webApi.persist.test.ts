// Web persistence durability slice: drives the REAL createWebApi (asp-wasm web
// build + the OPFS ByteStore) against a fake OPFS + a fetch shim that serves the
// real wasm bytes off disk, so persist()/engineFor() run unmodified. Guards two
// crash-safety invariants the split rows/blob layout depends on:
//   (1) persist writes every referenced blob (+ index) BEFORE the rows snapshot —
//       a durable `.rows` never points at an absent `.blob.<hash>`.
//   (2) if a referenced blob is nonetheless missing on restore, engineFor loads
//       leniently (empty file) but logs a loud data-loss diagnostic naming the
//       affected path — it is NOT re-fetched by anti-entropy.
import { test, expect, spyOn } from 'bun:test';
import { existsSync, readFileSync } from 'node:fs';
import { join } from 'node:path';

const WASM_PATH = join(import.meta.dir, '../../../crates/asp-wasm/pkg-web/asp_wasm_bg.wasm');
const HAVE_WASM = existsSync(WASM_PATH);

// A minimal in-memory stand-in for FileSystemDirectoryHandle that records the
// order in which files are committed (close()), so we can assert write ordering.
function fakeOpfs() {
  const files = new Map<string, Uint8Array>();
  const writeLog: string[] = [];
  const dir = {
    getFileHandle: async (name: string, opts?: { create?: boolean }) => {
      if (!files.has(name) && !opts?.create) throw new Error('NotFound');
      let pending = new Uint8Array();
      return {
        getFile: async () => {
          const b = files.get(name);
          if (b === undefined) throw new Error('NotFound');
          return { arrayBuffer: async () => b.slice().buffer };
        },
        createWritable: async () => ({
          write: async (data: BufferSource) => {
            pending = new Uint8Array(data instanceof Uint8Array ? data : new Uint8Array(data as ArrayBuffer));
          },
          close: async () => {
            files.set(name, pending);
            writeLog.push(name);
          },
        }),
      };
    },
    removeEntry: async (name: string) => void files.delete(name),
  };
  return { dir, files, writeLog };
}

// One shared fake store for the file: webApi's store() singleton caches the first
// directory it gets, so all tests here share it (they key off distinct vault ids).
const opfs = fakeOpfs();

if (HAVE_WASM) {
  const WASM = readFileSync(WASM_PATH);
  const realFetch = globalThis.fetch;
  // The web wasm build inits via `fetch(new URL('..._bg.wasm', import.meta.url))`;
  // under bun that URL is unfetchable, so serve the real bytes off disk instead.
  globalThis.fetch = (async (input: unknown, init?: unknown) => {
    const u = String((input as { url?: string })?.url ?? input);
    if (u.includes('asp_wasm_bg.wasm')) return new Response(WASM, { headers: { 'content-type': 'application/wasm' } });
    return realFetch(input as Parameters<typeof realFetch>[0], init as Parameters<typeof realFetch>[1]);
  }) as typeof fetch;
  Object.defineProperty(globalThis, 'navigator', {
    value: { storage: { getDirectory: async () => opfs.dir } },
    configurable: true,
    writable: true,
  });
}

test.skipIf(!HAVE_WASM)('persist writes blobs and the blob index BEFORE the rows snapshot', async () => {
  const { createWebApi } = await import('./webApi');
  const api = createWebApi();
  const start = opfs.writeLog.length;
  const v = await api.createVault('order'); // createVault awaits persist() directly
  const seq = opfs.writeLog.slice(start);

  const rowsIdx = seq.indexOf(`vault-${v.id}.rows`);
  const blobIdxIdx = seq.indexOf(`vault-${v.id}.blobs`);
  const blobIdxs = seq.map((n, i) => ({ n, i })).filter((e) => e.n.startsWith(`vault-${v.id}.blob.`));

  expect(rowsIdx).toBeGreaterThanOrEqual(0);
  expect(blobIdxIdx).toBeGreaterThanOrEqual(0);
  expect(blobIdxs.length).toBeGreaterThan(0); // README.md → at least one content blob
  // Every content blob AND the blob index land strictly before the rows snapshot,
  // so a crash mid-persist never leaves durable rows referencing an absent blob.
  for (const b of blobIdxs) expect(b.i).toBeLessThan(rowsIdx);
  expect(blobIdxIdx).toBeLessThan(rowsIdx);
});

test.skipIf(!HAVE_WASM)('restore with a missing referenced blob loads leniently but logs a data-loss diagnostic', async () => {
  const { createWebApi } = await import('./webApi');
  const v = await createWebApi().createVault('missing'); // persists README.md's blob + rows

  // Simulate the durable rows outliving one of their blobs (the exact corruption
  // the write-order invariant prevents, but which we still degrade gracefully on).
  const blobKey = [...opfs.files.keys()].find((k) => k.startsWith(`vault-${v.id}.blob.`));
  expect(blobKey).toBeTruthy();
  opfs.files.delete(blobKey as string);

  const errSpy = spyOn(console, 'error').mockImplementation(() => {});
  let logged: string[];
  try {
    // A fresh backend ⇒ empty engines map ⇒ engineFor re-reads from OPFS.
    const api2 = createWebApi();
    const status = await api2.getStatus(v.id); // does not throw despite the hole
    expect(status.vault_id).toBeTruthy();
    const files = await api2.listFiles(v.id);
    expect(files.some((f) => f.path === 'README.md')).toBe(true); // still present…
    expect(await api2.readFile(v.id, 'README.md')).toBe(''); // …but materialized EMPTY
    // Snapshot the recorded calls BEFORE mockRestore() (bun clears mock.calls on restore).
    logged = errSpy.mock.calls.map((c) => String(c[0]));
  } finally {
    errSpy.mockRestore();
  }

  expect(logged.length).toBe(1); // restore emitted exactly one diagnostic
  const msg = logged[0];
  expect(msg).toContain('missing from OPFS');
  expect(msg).toContain('README.md'); // affected path was derived and named
  expect(msg).toContain('will NOT re-sync'); // the corrected durability framing
});
