// The high-level Vault: a thin host-facing API over the wasm engine + a
// WebSocket transport that drives the one sans-IO Session (handshake +
// version-vector catch-up). All protocol/merge logic is in the engine; this file
// is host glue only.

import { type FeedResult, WasmEngine, type WasmEngineInstance } from './engine.ts';

const enc = new TextEncoder();
const dec = new TextDecoder();

function toBytes(c: Uint8Array | string): Uint8Array {
  return typeof c === 'string' ? enc.encode(c) : c;
}

/**
 * Default the scheme of a peer URL. A bare host (`hub:9000`, `example.com/path`)
 * is assumed to be a *secure* WebSocket — `wss://`. Explicit `ws://`/`wss://` is
 * left untouched; `http(s)://` is mapped to the `ws(s)://` equivalent (a common
 * paste). Empty stays empty so callers can detect "no peer set".
 */
export function normalizePeerUrl(url: string): string {
  const u = url.trim();
  if (!u) return '';
  if (/^wss?:\/\//i.test(u)) return u;
  if (/^https:\/\//i.test(u)) return u.replace(/^https:\/\//i, 'wss://');
  if (/^http:\/\//i.test(u)) return u.replace(/^http:\/\//i, 'ws://');
  return `wss://${u.replace(/^\/+/, '')}`;
}

export interface SyncOptions {
  authKey?: string;
  /** Idle window (ms) after which a one-shot sync is considered converged. */
  idleMs?: number;
  /** Overall timeout (ms). */
  timeoutMs?: number;
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
  renameFile(from: string, to: string): void | Promise<void>;
  files(): Record<string, Uint8Array> | Promise<Record<string, Uint8Array>>;
  sync(url: string, opts?: SyncOptions): Promise<number>;
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
   * One-shot sync against a listening `asp` peer: connect, run the mutual-auth
   * handshake + bidirectional version-vector catch-up, converge, and close.
   * Resolves with the number of rows integrated FROM the peer this pass — so a
   * caller can skip an expensive re-materialize when nothing new arrived (0).
   */
  async sync(url: string, opts: SyncOptions = {}): Promise<number> {
    const idleMs = opts.idleMs ?? 800;
    const timeoutMs = opts.timeoutMs ?? 15000;
    const base = normalizePeerUrl(url);
    const fullUrl = opts.authKey
      ? `${base}${base.includes('?') ? '&' : '?'}auth_key=${encodeURIComponent(opts.authKey)}`
      : base;

    const ws = new WebSocket(fullUrl);
    ws.binaryType = 'arraybuffer';

    await new Promise<void>((resolve, reject) => {
      const onErr = (e: unknown) => reject(new Error(`ws connect failed: ${String(e)}`));
      ws.addEventListener('open', () => resolve(), { once: true });
      ws.addEventListener('error', onErr, { once: true });
    });

    return await new Promise<number>((resolve, reject) => {
      let integrated = 0;
      let idle: ReturnType<typeof setTimeout> | undefined;
      const hardStop = setTimeout(() => {
        try {
          ws.close();
        } catch {}
        reject(new Error('sync timed out'));
      }, timeoutMs);

      const done = (err?: Error) => {
        clearTimeout(idle);
        clearTimeout(hardStop);
        try {
          ws.close();
        } catch {}
        if (err) reject(err);
        else resolve(integrated);
      };
      const resetIdle = () => {
        clearTimeout(idle);
        idle = setTimeout(() => done(), idleMs);
      };

      ws.addEventListener('message', (ev: MessageEvent) => {
        const frame = new Uint8Array(ev.data as ArrayBuffer);
        let r: FeedResult;
        try {
          r = JSON.parse(this.eng.feed(frame)) as FeedResult;
        } catch (e) {
          return done(e as Error);
        }
        integrated += r.integrated;
        for (const out of r.out) ws.send(Uint8Array.from(out));
        if (r.closed) {
          return done(r.closed.includes('denied') ? new Error(r.closed) : undefined);
        }
        resetIdle();
      });
      ws.addEventListener('close', () => done());
      ws.addEventListener('error', () => done(new Error('ws error during sync')));

      // Opening Hello.
      ws.send(this.eng.connect_start());
      resetIdle();
    });
  }

  free(): void {
    this.eng.free();
  }
}
