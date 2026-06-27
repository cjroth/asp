// The browser engine Web Worker: holds the REAL asp engine (wasm) and manages
// MULTIPLE vaults (one `Vault` per vault id), so one worker serves the whole
// multi-vault editor. Every heavy synchronous engine call (fold/merge/hash/feed)
// runs here, off the renderer thread — the UI never freezes during a sync.
//
// State for each vault is persisted to OPFS (a file per vault id) so a reload
// restores real file ids instead of re-importing the materialized tree (the
// duplicate-explosion loop). This mirrors the Obsidian plugin's persistence
// discipline, adapted to a multi-vault browser app.
//
// The protocol is a small request/reply RPC over postMessage (correlated by
// `id`), structured-clone-safe (strings/numbers/Uint8Arrays).

import { initAsp, Vault, foldFiles } from '../../../sdks/typescript/src/index.ts';
import type { FileMeta } from '../../../sdks/typescript/src/index.ts';

// Bundled at build time by esbuild/vite (see vite.config.ts): the wasm bytes
// inlined as base64 so the worker never fetches (sandbox/WebView-safe).
declare const __ASP_WASM_B64__: string;
function wasmBytes(): Uint8Array {
  const bin = atob(__ASP_WASM_B64__);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

export interface InitVaultArgs {
  vaultId: string;
  /** 32-byte device seed (hex). */
  seedHex: string;
  /** Empty adopts the peer's vault on first connect (clone). */
  vaultTag: string;
}
export type Req =
  | { id: number; op: 'boot' }
  | { id: number; op: 'initVault'; args: InitVaultArgs }
  | { id: number; op: 'freeVault'; vaultId: string }
  | { id: number; op: 'writeFile'; vaultId: string; path: string; bytes: Uint8Array }
  | { id: number; op: 'deleteFile'; vaultId: string; path: string }
  | { id: number; op: 'renameFile'; vaultId: string; from: string; to: string }
  | { id: number; op: 'filesDetail'; vaultId: string }
  | { id: number; op: 'readFile'; vaultId: string; path: string }
  | { id: number; op: 'sync'; vaultId: string; ticket: string; authKey?: string; relayUrl?: string }
  | { id: number; op: 'dumpState'; vaultId: string }
  | { id: number; op: 'loadState'; vaultId: string; bytes: Uint8Array }
  | { id: number; op: 'nodeSsh'; vaultId: string }
  | { id: number; op: 'rowCount'; vaultId: string }
  | { id: number; op: 'history'; vaultId: string }
  | { id: number; op: 'fileAtTime'; vaultId: string; path: string; ts: number }
  | { id: number; op: 'restoreFileAt'; vaultId: string; path: string; ts: number };

export interface Reply {
  id: number;
  ok: boolean;
  value?: unknown;
  error?: string;
}

const VAULT_DIR = 'asp-vaults';
const enc = new TextEncoder();
const dec = new TextDecoder();

function hexToBytes(h: string): Uint8Array {
  const out = new Uint8Array(h.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = Number.parseInt(h.slice(i * 2, i * 2 + 2), 16);
  return out;
}

/// OPFS persistence helpers — one file per vault id inside `asp-vaults/`.
async function opfsRoot(): Promise<FileSystemDirectoryHandle> {
  const root = await navigator.storage.getDirectory();
  return await root.getDirectoryHandle(VAULT_DIR, { create: true });
}
async function opfsRead(vaultId: string): Promise<Uint8Array | null> {
  try {
    const dir = await opfsRoot();
    const fh = await dir.getFileHandle(vaultId);
    const f = await fh.getFile();
    const buf = await f.arrayBuffer();
    return new Uint8Array(buf);
  } catch {
    return null;
  }
}
async function opfsWrite(vaultId: string, bytes: Uint8Array): Promise<void> {
  const dir = await opfsRoot();
  const fh = await dir.getFileHandle(vaultId, { create: true });
  // Slice to the view's exact range — write requires a BufferSource.
  const view = bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength);
  const w = await (fh as unknown as { createWritable: () => Promise<{ write: (d: ArrayBuffer) => Promise<void>; close: () => Promise<void> }> }).createWritable();
  await w.write(view as ArrayBuffer);
  await w.close();
}

class Host {
  private vaults = new Map<string, Vault>();
  private ready = false;

  reply(id: number, ok: boolean, value?: unknown, error?: string): void {
    const r: Reply = { id, ok, value, error };
    (self as unknown as { postMessage(m: unknown): void }).postMessage(r);
  }

  async handle(req: Req): Promise<void> {
    try {
      switch (req.op) {
        case 'boot': {
          if (!this.ready) {
            await initAsp(wasmBytes());
            this.ready = true;
          }
          return this.reply(req.id, true, true);
        }
        case 'initVault': {
          if (!this.ready) await initAsp(wasmBytes()), (this.ready = true);
          const seed = hexToBytes(req.args.seedHex);
          const v = new Vault(seed, req.args.vaultTag);
          this.vaults.set(req.args.vaultId, v);
          return this.reply(req.id, true, {
            nodeSsh: v.nodeSsh(),
            nodeId: v.nodeId(),
            vaultId: v.vaultId(),
          });
        }
        case 'freeVault': {
          const v = this.vaults.get(req.vaultId);
          if (v) {
            v.free();
            this.vaults.delete(req.vaultId);
          }
          return this.reply(req.id, true, true);
        }
        case 'writeFile': {
          this.v(req.vaultId).writeFile(req.path, req.bytes);
          await this.persist(req.vaultId);
          return this.reply(req.id, true, true);
        }
        case 'deleteFile': {
          this.v(req.vaultId).deleteFile(req.path);
          await this.persist(req.vaultId);
          return this.reply(req.id, true, true);
        }
        case 'renameFile': {
          this.v(req.vaultId).renameFile(req.from, req.to);
          await this.persist(req.vaultId);
          return this.reply(req.id, true, true);
        }
        case 'filesDetail': {
          return this.reply(req.id, true, this.v(req.vaultId).filesDetail());
        }
        case 'readFile': {
          const b = this.v(req.vaultId).readFile(req.path);
          return this.reply(req.id, true, b ? dec.decode(b) : null);
        }
        case 'sync': {
          const n = await this.v(req.vaultId).sync(req.ticket, { authKey: req.authKey, relayUrl: req.relayUrl });
          await this.persist(req.vaultId);
          return this.reply(req.id, true, n);
        }
        case 'dumpState': {
          return this.reply(req.id, true, this.v(req.vaultId).dumpState());
        }
        case 'loadState': {
          const n = this.v(req.vaultId).loadState(req.bytes);
          await this.persist(req.vaultId);
          return this.reply(req.id, true, n);
        }
        case 'nodeSsh': {
          return this.reply(req.id, true, this.v(req.vaultId).nodeSsh());
        }
        case 'rowCount': {
          return this.reply(req.id, true, this.v(req.vaultId).rowCount());
        }
        case 'history': {
          // The full log as wire rows (rows_after({}) = everything); surface the
          // row fields the timeline renders, in (ts, lamport) order.
          const all = this.v(req.vaultId).dump(); // JSON WireRow[]
          const rows = (JSON.parse(all) as Array<{ row: { id: string; ts: number; lamport: number; kind: string; path: string | null } }>)
            .map((w) => w.row)
            .sort((a, b) => a.ts - b.ts || a.lamport - b.lamport);
          return this.reply(req.id, true, rows);
        }
        case 'fileAtTime': {
          // Fold the log up to `ts` and read the path. dump() gives every wire
          // row + its blobs; fold_files computes the deterministic state.
          const all = this.v(req.vaultId).dump();
          const wire = JSON.parse(all) as Array<{ row: { ts: number } & Record<string, unknown>; blobs: Array<{ hash: string; bytes: number[] }> }>;
          const upto = wire.filter((w) => w.row.ts <= req.ts);
          const rowsJson = JSON.stringify(upto.map((w) => w.row));
          const blobs: Record<string, number[]> = {};
          for (const w of upto) for (const b of w.blobs) blobs[b.hash] = b.bytes;
          const filesJson = foldFiles(rowsJson, JSON.stringify(blobs));
          const files = JSON.parse(filesJson) as Record<string, number[]>;
          const bytes = files[req.path];
          if (!bytes) return this.reply(req.id, true, { exists: false, content: null, key: 'gone' });
          const out = new Uint8Array(bytes);
          return this.reply(req.id, true, { exists: true, content: dec.decode(out), key: `${req.path}:${req.ts}` });
        }
        case 'restoreFileAt': {
          // Re-author the past content as a new write (deterministic restore).
          const all = this.v(req.vaultId).dump();
          const wire = JSON.parse(all) as Array<{ row: { ts: number } & Record<string, unknown>; blobs: Array<{ hash: string; bytes: number[] }> }>;
          const upto = wire.filter((w) => w.row.ts <= req.ts);
          const rowsJson = JSON.stringify(upto.map((w) => w.row));
          const blobs: Record<string, number[]> = {};
          for (const w of upto) for (const b of w.blobs) blobs[b.hash] = b.bytes;
          const filesJson = foldFiles(rowsJson, JSON.stringify(blobs));
          const files = JSON.parse(filesJson) as Record<string, number[]>;
          const bytes = files[req.path];
          if (!bytes) return this.reply(req.id, true, false);
          const content = new Uint8Array(bytes);
          this.v(req.vaultId).writeFile(req.path, content);
          await this.persist(req.vaultId);
          return this.reply(req.id, true, true);
        }
      }
    } catch (e) {
      this.reply(req.id, false, undefined, e instanceof Error ? e.message : String(e));
    }
  }

  private v(vaultId: string): Vault {
    const v = this.vaults.get(vaultId);
    if (!v) throw new Error(`worker: vault ${vaultId} not initialized`);
    return v;
  }

  /// Persist a vault's engine state to OPFS (debounced caller-side in the host
  /// proxy; here it's a straight dump+write).
  private async persist(vaultId: string): Promise<void> {
    const v = this.vaults.get(vaultId);
    if (!v) return;
    try {
      const bytes = v.dumpState();
      await opfsWrite(vaultId, bytes);
    } catch {
      // OPFS may be unavailable (private mode); persistence is best-effort — the
      // in-memory engine is still correct for the session.
    }
  }

  /// Restore a vault's state from OPFS on init (call after initVault).
  async restore(vaultId: string): Promise<number> {
    const bytes = await opfsRead(vaultId);
    if (!bytes) return 0;
    return this.v(vaultId).loadState(bytes);
  }
}

const host = new Host();
(self as unknown as { onmessage: ((e: MessageEvent) => void) | null }).onmessage = (e: MessageEvent) => {
  const req = e.data as Req;
  void host.handle(req);
};
// Expose restore via a side-channel message so the host proxy can call it
// after initVault without bloating the RPC surface.
(self as unknown as { __aspRestore: (vaultId: string) => Promise<number> }).__aspRestore = (vaultId: string) =>
  host.restore(vaultId);

// Unused export to keep the type referenced for tooling.
export type { FileMeta };
