// Web backend: the real asp-core engine compiled to wasm (asp-wasm), with each
// vault's state persisted to the browser's Origin Private File System (OPFS) via
// `dump_state()`/`load_state()`. This is the "thin client" model from the design
// — no folders, no native FS; notes live in browser storage and converge over
// iroh sync. Implements the same `Api` surface as the desktop (Tauri) backend.
import init, { WasmEngine } from 'asp-wasm';
// Let Vite serve the .wasm as a hashed asset with the correct `application/wasm`
// MIME (in both dev and build). Relying on wasm-bindgen's default
// `new URL('…', import.meta.url)` breaks under Vite dev: for a linked package it
// resolves to a `file://` URL the browser refuses to fetch.
import wasmUrl from 'asp-wasm/asp_wasm_bg.wasm?url';
import type { Api, CloneProgress, FileAt, FileEntry, HistEvent, VaultInfo, VaultStatus } from './api';
import { WELCOME_MD } from '../vault/welcome';
import { makeCoalescer } from './coalesce';

const enc = new TextEncoder();
const dec = new TextDecoder();
const randHex = (n: number) => Array.from(crypto.getRandomValues(new Uint8Array(n)), (b) => b.toString(16).padStart(2, '0')).join('');

interface RegEntry {
  id: string;
  vault_id: string;
  // The upstream this vault was cloned from. Browsers can't open a listening
  // socket, so a web node can only stay in sync by re-dialing this ticket — the
  // poll calls `syncNow` and we fall back to these when no ticket is passed.
  ticket?: string;
  authKey?: string | null;
}

// ---- byte store: OPFS when available, in-memory fallback otherwise ----
// OPFS needs a secure context and browser support; where it's missing (e.g. some
// private-browsing modes, older engines, headless drivers) we degrade to a
// non-persistent in-memory store so the app still runs.
interface ByteStore {
  read(name: string): Promise<Uint8Array | null>;
  write(name: string, bytes: Uint8Array): Promise<void>;
  remove(name: string): Promise<void>;
}

function memoryStore(): ByteStore {
  const m = new Map<string, Uint8Array>();
  return {
    read: async (n) => m.get(n) ?? null,
    write: async (n, b) => void m.set(n, b),
    remove: async (n) => void m.delete(n),
  };
}

function opfsStore(dir: FileSystemDirectoryHandle): ByteStore {
  return {
    read: async (n) => {
      try {
        const fh = await dir.getFileHandle(n);
        return new Uint8Array(await (await fh.getFile()).arrayBuffer());
      } catch {
        return null;
      }
    },
    write: async (n, b) => {
      const fh = await dir.getFileHandle(n, { create: true });
      const w = await fh.createWritable();
      await w.write(b as BufferSource);
      await w.close();
    },
    remove: async (n) => {
      try {
        await dir.removeEntry(n);
      } catch {
        /* already gone */
      }
    },
  };
}

let storePromise: Promise<ByteStore> | null = null;
function store(): Promise<ByteStore> {
  return (storePromise ??= (async () => {
    try {
      if (navigator.storage?.getDirectory) return opfsStore(await navigator.storage.getDirectory());
    } catch {
      /* fall through to the in-memory store */
    }
    console.warn('[asp] OPFS is unavailable in this browser — notes will not persist across reloads.');
    return memoryStore();
  })());
}

const readBytes = (name: string) => store().then((s) => s.read(name));
const writeBytes = (name: string, bytes: Uint8Array) => store().then((s) => s.write(name, bytes));
const removeFile = (name: string) => store().then((s) => s.remove(name));
async function readJson<T>(name: string, fallback: T): Promise<T> {
  const b = await readBytes(name);
  if (!b) return fallback;
  try {
    return JSON.parse(dec.decode(b)) as T;
  } catch {
    return fallback;
  }
}
const writeJson = (name: string, obj: unknown) => writeBytes(name, enc.encode(JSON.stringify(obj)));

const stateName = (id: string) => `vault-${id}.state`;

export function createWebApi(): Api {
  let wasmReady: Promise<void> | null = null;
  const ensureWasm = () => (wasmReady ??= init({ module_or_path: wasmUrl }).then(() => undefined));

  let seed: Uint8Array | null = null;
  async function deviceSeed(): Promise<Uint8Array> {
    if (seed) return seed;
    let s = await readBytes('device-seed');
    if (!s || s.length < 32) {
      s = crypto.getRandomValues(new Uint8Array(32));
      await writeBytes('device-seed', s);
    }
    seed = s;
    return s;
  }

  const registry = () => readJson<RegEntry[]>('registry.json', []);
  const engines = new Map<string, WasmEngine>();
  // Live connections, one per vault id. A browser can't accept inbound
  // connections, but once it dials the upstream it holds the link open and rows
  // stream both ways in realtime — no polling.
  const live = new Map<string, { stop: boolean }>();

  async function engineFor(id: string): Promise<WasmEngine> {
    const cached = engines.get(id);
    if (cached) return cached;
    await ensureWasm();
    const reg = await registry();
    const entry = reg.find((r) => r.id === id);
    const eng = new WasmEngine(await deviceSeed(), entry?.vault_id ?? '');
    const state = await readBytes(stateName(id));
    if (state) eng.load_state(state);
    engines.set(id, eng);
    return eng;
  }
  const persist = (id: string, eng: WasmEngine) => writeBytes(stateName(id), eng.dump_state());
  // `dump_state` re-serializes the WHOLE engine (every row + blob) to OPFS, so on
  // a big vault doing it on every keystroke-save / synced row is the dominant
  // cost. Coalesce it: edits return immediately and the state is written at most
  // once per quiet window. Durability is the live peer sync; OPFS is a cache that
  // a reload re-syncs — and we flush on page-hide so nothing pending is lost.
  const persistQueue = makeCoalescer<WasmEngine>((id, eng) => void persist(id, eng).catch(() => {}), 700);
  if (typeof document !== 'undefined') {
    document.addEventListener('visibilitychange', () => {
      if (document.visibilityState === 'hidden') persistQueue.flush();
    });
    window.addEventListener('pagehide', () => persistQueue.flush());
  }

  const fileList = (eng: WasmEngine): FileEntry[] => {
    const detail = JSON.parse(eng.files_detail_json()) as { file_id: string; path: string; merge_class: string; deleted: boolean }[];
    return detail
      .filter((f) => !f.deleted)
      .map((f) => ({ path: f.path, file_id: f.file_id, is_dir: f.merge_class === 'dir', merge_class: f.merge_class }));
  };

  const info = (e: RegEntry): VaultInfo => ({ id: e.id, path: '', vault_id: e.vault_id, enabled: true, listening_ticket: null });

  // Look up the upstream a vault was cloned from, so the poll-driven `syncNow`
  // (called without an explicit ticket) can re-dial it.
  const upstreamOf = async (id: string): Promise<{ ticket: string; authKey?: string | null } | null> => {
    const entry = (await registry()).find((r) => r.id === id);
    return entry?.ticket ? { ticket: entry.ticket, authKey: entry.authKey ?? null } : null;
  };

  return {
    listVaults: async () => (await registry()).map(info),

    getIdentity: async () => {
      await ensureWasm();
      // A throwaway engine just to derive the device's ssh identity.
      return new WasmEngine(await deviceSeed(), randHex(8)).node_ssh();
    },

    createVault: async (_name: string): Promise<VaultInfo> => {
      await ensureWasm();
      const id = 'w_' + randHex(8);
      const vault_id = randHex(16);
      const eng = new WasmEngine(await deviceSeed(), vault_id);
      eng.record_write('README.md', enc.encode(WELCOME_MD));
      engines.set(id, eng);
      await persist(id, eng);
      const reg = await registry();
      reg.unshift({ id, vault_id });
      await writeJson('registry.json', reg);
      return info({ id, vault_id });
    },

    cloneRemote: async (_dest: string, ticket: string, authKey?: string, onProgress?: CloneProgress): Promise<VaultInfo> => {
      await ensureWasm();
      const id = 'w_' + randHex(8);
      const eng = new WasmEngine(await deviceSeed(), ''); // empty → adopt the peer's vault
      let lastDone = 0;
      let lastTotal = 0;
      // Stream catch-up progress to the UI, then flip to the 'saving' phase for the
      // (one) OPFS write — on a big vault that final write is itself a few seconds.
      const cb = onProgress
        ? (done: number, total: number) => {
            lastDone = done;
            lastTotal = total;
            onProgress(done, total, 'receiving');
          }
        : undefined;
      await eng.sync(ticket, authKey ?? null, null, cb);
      const vault_id = eng.vault_id();
      engines.set(id, eng);
      onProgress?.(lastDone, lastTotal || lastDone, 'saving');
      await persist(id, eng);
      const reg = await registry();
      // Remember the upstream so the poll can keep re-syncing against it — a
      // browser node has no other way to pull a peer's later pushes.
      reg.unshift({ id, vault_id, ticket, authKey: authKey ?? null });
      await writeJson('registry.json', reg);
      return info({ id, vault_id });
    },

    // Hold a live connection to the upstream open, reconnecting if it drops. The
    // engine integrates remote pushes in realtime and fires `onChange` so the UI
    // refreshes; locally-authored rows push out over the same connection. A vault
    // with no upstream (created locally) has nothing to connect to.
    startLiveSync: async (id, onChange) => {
      if (live.has(id)) return;
      const up = await upstreamOf(id);
      if (!up) return;
      const eng = await engineFor(id);
      const handle = { stop: false };
      live.set(id, handle);
      void (async () => {
        while (!handle.stop) {
          try {
            // Resolves when the connection closes; on_change fires per remote push.
            await eng.connect_live(up.ticket, up.authKey ?? undefined, null, () => {
              persistQueue.schedule(id, eng);
              try {
                onChange();
              } catch {
                /* listener threw — ignore */
              }
            });
          } catch {
            /* connect/dial failed — back off and retry below */
          }
          if (handle.stop) break;
          await new Promise((r) => setTimeout(r, 1500)); // reconnect backoff
        }
        live.delete(id);
      })();
    },

    stopLiveSync: async (id) => {
      const h = live.get(id);
      if (h) h.stop = true;
      live.delete(id);
      persistQueue.flushKey(id); // leaving the vault — write its latest state now
    },

    syncNow: async (id, ticket, authKey) => {
      // Called from the editor poll with no ticket: fall back to the upstream we
      // cloned from. A vault created locally (no upstream) simply has nothing to
      // sync against, so this is a no-op.
      let t = ticket;
      let k = authKey;
      if (!t) {
        const up = await upstreamOf(id);
        if (!up) return;
        t = up.ticket;
        k = up.authKey ?? undefined;
      }
      const eng = await engineFor(id);
      await eng.sync(t, k ?? null, null);
      persistQueue.schedule(id, eng);
    },

    getStatus: async (id): Promise<VaultStatus> => {
      const eng = await engineFor(id);
      return { id, vault_id: eng.vault_id(), rows: eng.row_count(), files: fileList(eng).length, head: '', listening_ticket: null, peers: [], last_ts: null };
    },

    listFiles: async (id) => fileList(await engineFor(id)),

    readFile: async (id, path) => {
      const bytes = (await engineFor(id)).read_file(path);
      return bytes ? dec.decode(bytes) : '';
    },

    writeFile: async (id, path, content) => {
      const eng = await engineFor(id);
      eng.record_write(path, enc.encode(content));
      persistQueue.schedule(id, eng);
    },

    renameFile: async (id, oldPath, newPath) => {
      const eng = await engineFor(id);
      eng.record_rename(oldPath, newPath);
      persistQueue.schedule(id, eng);
    },

    deleteFile: async (id, path) => {
      const eng = await engineFor(id);
      eng.record_remove(path);
      persistQueue.schedule(id, eng);
    },

    // Empty directories aren't first-class in the thin (wasm) engine — the folder
    // shows optimistically in the UI and persists once it holds a file.
    createDir: async () => {},

    // Time-travel and on-disk rescan are desktop-only; degrade gracefully on web.
    history: async () => [] as HistEvent[],
    readFileAt: async (id, path): Promise<FileAt> => {
      const bytes = (await engineFor(id)).read_file(path);
      return bytes ? { exists: true, content: dec.decode(bytes) } : { exists: false, content: '' };
    },
    restoreFileAt: async () => {},
    rescan: async () => {},

    removeVault: async (id) => {
      persistQueue.cancel(id); // don't let a debounced write resurrect the state file
      engines.delete(id);
      await removeFile(stateName(id));
      const reg = (await registry()).filter((r) => r.id !== id);
      await writeJson('registry.json', reg);
    },

    // Browsers can't accept inbound connections (no listening socket), so sharing
    // FROM a web node isn't available; these are no-ops on web.
    addLocalFolder: () => Promise.reject(new Error('addLocalFolder is desktop-only')),
    setAllowConnections: async () => null,
    // A browser can't co-host a relay (no listening socket); always off on web.
    setLocalRelay: async () => false,
    getLocalRelay: async () => false,
    authorize: async () => {},
    createSnapshot: async () => '',
    restore: async () => {},

    // No native file manager in the browser — revealing a folder is a no-op.
    revealPath: async () => {},
  };
}
