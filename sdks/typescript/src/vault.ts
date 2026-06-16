// The high-level Vault: a thin host-facing API over the wasm engine + a
// WebSocket transport that drives the one sans-IO Session (handshake +
// version-vector catch-up). All protocol/merge logic is in the engine; this file
// is host glue only.

import { WasmEngine, type WasmEngineInstance } from './engine.ts';
import type { FileMeta } from './engine-types.ts';

const enc = new TextEncoder();
const dec = new TextDecoder();

function toBytes(c: Uint8Array | string): Uint8Array {
  return typeof c === 'string' ? enc.encode(c) : c;
}

/**
 * Normalize a pasted peer spec. With iroh, a peer is an opaque **ticket** (or a
 * bare node id) — there is no URL scheme to default. We just trim; empty stays
 * empty so callers can detect "no peer set". (Kept as a named export for callers
 * that previously normalized a URL.)
 */
export function normalizePeerUrl(spec: string): string {
  return spec.trim();
}

export interface SyncOptions {
  authKey?: string;
  /** Override the default public relays with a specific relay URL — e.g. a
   * self-hosted `asp relay`, or a local relay in tests. */
  relayUrl?: string;
}

/**
 * The slice of the engine a host (the Obsidian bridge) drives. Both the
 * in-process {@link Vault} (synchronous) and the worker-backed `WorkerVault`
 * (asynchronous, off-thread) satisfy it — methods may return a value OR a
 * Promise of it, so callers `await` uniformly. `nodeSsh` stays synchronous (the
 * worker proxy caches the identity from init).
 */
export interface EngineVault {
  writeFile(path: string, bytes: Uint8Array): void | Promise<void>;
  /** Stage a batch of files (create/edit, no deletes) in one fold. */
  writeFiles(files: Record<string, Uint8Array>): void | Promise<void>;
  deleteFile(path: string): void | Promise<void>;
  /** Author deletes for a batch of paths in one fold — the startup-reconcile
   * seam for capturing files deleted while the host app was closed. */
  deleteFiles(paths: string[]): void | Promise<void>;
  renameFile(from: string, to: string): void | Promise<void>;
  files(): Record<string, Uint8Array> | Promise<Record<string, Uint8Array>>;
  /** Per-file fold metadata (path + content hash, NO content) — cheap, so a
   * large vault can be materialized incrementally without serializing every
   * file's bytes into one giant JSON (which OOMs the worker). */
  filesDetail(): FileMeta[] | Promise<FileMeta[]>;
  /** One file's materialized bytes (binary — no JSON number-array blowup). */
  readFile(path: string): (Uint8Array | undefined) | Promise<Uint8Array | undefined>;
  sync(url: string, opts?: SyncOptions): Promise<number>;
  /** Abort an in-flight {@link sync} (closes the socket; the pending sync
   * rejects with a "cancelled" error). A no-op if nothing is in flight. Lets a
   * UI offer "Cancel" on a connect that's hanging (e.g. a mistyped URL). */
  cancel(): void | Promise<void>;
  /** Serialize the whole engine state (all rows + blobs) so a thin client can
   * persist it and {@link load} it on next launch — WITHOUT this, a client that
   * rebuilds its engine each run re-imports its own materialized tree as new
   * files, which collide and multiply (the duplicate-explosion loop).
   * LEGACY JSON form — kept only so an existing persisted file can still be
   * read once and migrated; prefer {@link dumpState}/{@link loadState}. */
  dump(): string | Promise<string>;
  /** Restore engine state produced by {@link dump} (re-integrates the rows). */
  load(stateJson: string): void | Promise<void>;
  /** Serialize the whole engine state as compact msgpack BYTES — rows + each
   * blob stored once. The {@link dump} JSON form duplicates blobs per row and
   * inflates every content byte to ~4 chars, which OOMs a mobile WebView on a
   * large vault; this form is what thin clients persist. */
  dumpState(): Uint8Array | Promise<Uint8Array>;
  /** Restore a {@link dumpState} snapshot (validates row ids + blob hashes).
   * Returns the number of rows newly integrated. */
  loadState(bytes: Uint8Array): number | Promise<number>;
  nodeSsh(): string;
  free(): void | Promise<void>;
}

export class Vault {
  private eng: WasmEngineInstance;

  /** Create a thin node. An empty `vaultId` adopts the peer's vault on connect. */
  constructor(seed: Uint8Array, vaultId = '') {
    this.eng = new WasmEngine(seed, vaultId);
  }

  nodeId(): string {
    return this.eng.node_id();
  }
  nodeSsh(): string {
    return this.eng.node_ssh();
  }
  vaultId(): string {
    return this.eng.vault_id();
  }
  rowCount(): number {
    return this.eng.row_count();
  }

  writeFile(path: string, content: Uint8Array | string): void {
    this.eng.record_write(path, toBytes(content));
  }
  deleteFile(path: string): void {
    this.eng.record_remove(path);
  }
  /** Author deletes for a batch of paths with a single fold. */
  deleteFiles(paths: string[]): void {
    this.eng.remove_files(JSON.stringify(paths));
  }
  renameFile(from: string, to: string): void {
    this.eng.record_rename(from, to);
  }

  readFile(path: string): Uint8Array | undefined {
    return this.eng.read_file(path);
  }
  readTextFile(path: string): string | undefined {
    const b = this.eng.read_file(path);
    return b ? dec.decode(b) : undefined;
  }

  /** The materialized working tree as `path -> bytes`. */
  files(): Record<string, Uint8Array> {
    const raw = JSON.parse(this.eng.files_json()) as Record<string, number[]>;
    const out: Record<string, Uint8Array> = {};
    for (const [p, arr] of Object.entries(raw)) out[p] = Uint8Array.from(arr);
    return out;
  }

  /** Per-file fold metadata (path + content hash, no content). */
  filesDetail(): FileMeta[] {
    return JSON.parse(this.eng.files_detail_json()) as FileMeta[];
  }

  /** Serialize all rows+blobs as JSON (LEGACY — see {@link EngineVault.dump}). */
  dump(): string {
    return this.eng.rows_after(JSON.stringify({}));
  }
  /** Re-integrate a {@link dump} into this engine. */
  load(stateJson: string): void {
    this.eng.integrate(stateJson);
  }

  /** Compact binary engine state (rows + each blob once) — what thin clients
   * persist across launches. */
  dumpState(): Uint8Array {
    return this.eng.dump_state();
  }
  /** Restore a {@link dumpState} snapshot; returns rows newly integrated. */
  loadState(bytes: Uint8Array): number {
    return this.eng.load_state(bytes);
  }

  /** Seed the engine from the host's current vault contents (whole-set commit). */
  commitFiles(files: Record<string, Uint8Array | string>): void {
    const obj: Record<string, number[]> = {};
    for (const [p, c] of Object.entries(files)) obj[p] = Array.from(toBytes(c));
    this.eng.commit_files(JSON.stringify(obj));
  }

  /** Stage a batch of files (create/edit, no deletes) with a single fold — the
   * startup reconcile seam, so a large vault doesn't re-fold per file. */
  writeFiles(files: Record<string, Uint8Array | string>): void {
    const obj: Record<string, number[]> = {};
    for (const [p, c] of Object.entries(files)) obj[p] = Array.from(toBytes(c));
    this.eng.write_files(JSON.stringify(obj));
  }

  /**
   * One-shot sync against a listening `asp` peer over **iroh**: dial the peer's
   * ticket (via a relay — a browser can't do UDP), run the mutual-auth handshake
   * + bidirectional version-vector catch-up, converge, and close. Resolves with
   * the number of rows integrated FROM the peer this pass — so a caller can skip
   * an expensive re-materialize when nothing new arrived (0). iroh runs inside
   * the wasm engine; this method is host glue only.
   */
  async sync(ticket: string, opts: SyncOptions = {}): Promise<number> {
    const spec = ticket.trim();
    if (!spec) throw new Error('sync: empty peer ticket');
    return await this.eng.sync(spec, opts.authKey, opts.relayUrl);
  }

  /** Abort an in-flight {@link sync}. iroh drives the connection inside the wasm
   * engine with its own timeouts, so there is no socket to close from here; this
   * is a no-op kept for API compatibility with the {@link EngineVault} interface. */
  cancel(): void {}

  free(): void {
    this.eng.free();
  }
}
