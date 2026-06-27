// The browser backend for the editor API: drives the real asp wasm engine in a
// Web Worker (iroh-in-wasm), persists per-vault state to OPFS, and keeps the
// vault list + device identity in localStorage. NO protocol logic — every call
// is an RPC into the worker, which calls the same `asp-core` engine as the CLI.

import type { VaultApi } from './api';
import type { FileAtTime, HistoryEvent, TreeNode, VaultInfo, VaultStatus } from './types';
import type { FileMeta } from '../../../sdks/typescript/src/index.ts';
import type { Req } from './engine-worker';

/** Distributive Omit — without this, Omit<Union,'id'> collapses the union into
 *  one type requiring every variant's fields. Distribute so each variant keeps
 *  only its own fields minus `id`. */
type WithoutId<T> = T extends { id: number } ? Omit<T, 'id'> : T;

const META_KEY = 'asp.editor.v1';
const SEED_KEY = 'asp.device.seed';

interface SavedVault {
  id: string;
  name: string;
  hue: number;
  kind: 'browser';
  path: null;
  createdTs: number;
  lastSync: string;
  /** The peer ticket this vault was cloned from / syncs against (browser thin
   *  node makes outbound syncs only). */
  peerTicket?: string;
  peerAuthKey?: string;
}

function randomSeedHex(): string {
  const b = new Uint8Array(32);
  crypto.getRandomValues(b);
  return [...b].map((x) => x.toString(16).padStart(2, '0')).join('');
}
function hashStr(s: string): number {
  let h = 5381;
  for (let i = 0; i < s.length; i++) h = ((h << 5) + h + s.charCodeAt(i)) >>> 0;
  return h;
}

interface MetaState {
  seedHex: string;
  vaults: SavedVault[];
}

function loadMeta(): MetaState {
  try {
    const raw = localStorage.getItem(META_KEY);
    if (raw) {
      const s = JSON.parse(raw) as MetaState;
      if (s.seedHex && Array.isArray(s.vaults)) return s;
    }
  } catch {
    /* fall through */
  }
  const seedHex = randomSeedHex();
  const st: MetaState = { seedHex, vaults: [] };
  try {
    localStorage.setItem(META_KEY, JSON.stringify(st));
  } catch {
    /* ignore */
  }
  return st;
}
function saveMeta(st: MetaState): void {
  try {
    localStorage.setItem(META_KEY, JSON.stringify(st));
  } catch {
    /* ignore */
  }
}

/** Build the editor's tree from flat file metadata (paths). */
function buildTree(files: FileMeta[]): TreeNode[] {
  interface Builder {
    name: string;
    path: string;
    children: Map<string, Builder>;
    isFile: boolean;
  }
  const root: Builder = { name: '', path: '', children: new Map(), isFile: false };
  for (const f of files) {
    if (f.deleted) continue;
    const segs = f.path.split('/').filter((s) => s.length > 0);
    let cur = root;
    let acc = '';
    for (let i = 0; i < segs.length; i++) {
      acc = acc ? `${acc}/${segs[i]}` : segs[i];
      const leaf = i === segs.length - 1;
      let entry = cur.children.get(segs[i]);
      if (!entry) {
        entry = { name: segs[i], path: acc, children: new Map(), isFile: false };
        cur.children.set(segs[i], entry);
      }
      if (leaf) entry.isFile = true;
      cur = entry;
    }
  }
  function finalize(b: Builder): TreeNode[] {
    return [...b.children.values()].map((c) => {
      const isDir = !c.isFile;
      return { name: c.name, path: c.path, is_dir: isDir, children: isDir ? finalize(c) : undefined };
    });
  }
  return finalize(root);
}

export class WebVaultApi implements VaultApi {
  readonly isDesktop = false;
  private worker!: Worker;
  private nextId = 1;
  private pending = new Map<number, { resolve: (v: unknown) => void; reject: (e: Error) => void }>();
  private meta: MetaState;
  private booted = false;
  /** Optional relay URL override (a self-hosted `asp relay`); blank = public
   *  relays. Set by the e2e harness to point at a local relay. */
  relayUrl = '';

  constructor() {
    this.meta = loadMeta();
  }

  /** Lazily start the worker (the editor calls ensureBooted before any op). */
  async ensureBooted(): Promise<void> {
    if (this.booted) return;
    // The worker is emitted as a separate chunk by Vite (new Worker(new URL(...))).
    this.worker = new Worker(new URL('./engine-worker.ts', import.meta.url), { type: 'module' });
    this.worker.onmessage = (e: MessageEvent) => {
      const r = e.data as { id: number; ok: boolean; value?: unknown; error?: string };
      const p = this.pending.get(r.id);
      if (!p) return;
      this.pending.delete(r.id);
      if (r.ok) p.resolve(r.value);
      else p.reject(new Error(r.error ?? 'worker error'));
    };
    this.worker.onerror = (e) => {
      // Reject all pending on a fatal worker error.
      for (const p of this.pending.values()) p.reject(new Error(e.message ?? 'worker crashed'));
      this.pending.clear();
    };
    await this.call({ op: 'boot' });
    this.booted = true;
    // Re-init any previously-known vaults (restoring OPFS state).
    for (const v of this.meta.vaults) {
      await this.call({ op: 'initVault', args: { vaultId: v.id, seedHex: this.meta.seedHex, vaultTag: '' } });
      await this.restoreOpfs(v.id);
    }
  }
  private call<T = unknown>(req: WithoutId<Req>): Promise<T> {
    const id = this.nextId++;
    return new Promise<T>((resolve, reject) => {
      this.pending.set(id, { resolve: resolve as (v: unknown) => void, reject });
      this.worker.postMessage({ id, ...req } as Req);
    });
  }

  /// Pull persisted engine state from OPFS into a vault (best-effort).
  private async restoreOpfs(vaultId: string): Promise<void> {
    try {
      const bytes = await this.readOpfs(vaultId);
      if (bytes) await this.call({ op: 'loadState', vaultId, bytes });
    } catch {
      /* ignore — fresh engine is fine */
    }
  }
  private async readOpfs(vaultId: string): Promise<Uint8Array | null> {
    try {
      const root = await navigator.storage.getDirectory();
      const dir = await root.getDirectoryHandle('asp-vaults', { create: true });
      const fh = await dir.getFileHandle(vaultId);
      const f = await fh.getFile();
      return new Uint8Array(await f.arrayBuffer());
    } catch {
      return null;
    }
  }
  private async writeOpfs(vaultId: string, bytes: Uint8Array): Promise<void> {
    try {
      const root = await navigator.storage.getDirectory();
      const dir = await root.getDirectoryHandle('asp-vaults', { create: true });
      const fh = await dir.getFileHandle(vaultId, { create: true });
      const view = bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength);
      const w = await (fh as unknown as { createWritable: () => Promise<{ write: (d: ArrayBuffer) => Promise<void>; close: () => Promise<void> }> }).createWritable();
      await w.write(view as ArrayBuffer);
      await w.close();
    } catch {
      /* best-effort */
    }
  }

  // ---------------- VaultApi ----------------

  async listVaults(): Promise<VaultInfo[]> {
    await this.ensureBooted();
    return this.meta.vaults.map((v) => ({
      id: v.id,
      path: v.path ?? '',
      vault_id: v.id,
      enabled: true,
      listening_ticket: null,
    }));
  }

  async addLocalFolder(_path: string): Promise<VaultInfo> {
    // Web target: there's no real folder — create a fresh in-browser vault.
    return this.createBrowserVault('Untitled vault');
  }

  /** Create a fresh in-browser vault (the "New vault" action on web). */
  async createBrowserVault(name: string): Promise<VaultInfo> {
    await this.ensureBooted();
    const id = 'v_' + Date.now().toString(36);
    await this.call({ op: 'initVault', args: { vaultId: id, seedHex: this.meta.seedHex, vaultTag: '' } });
    // Seed a README + welcome so the tree isn't empty.
    const v = this.meta.vaults.find((x) => x.id === id);
    const sv: SavedVault = v ?? {
      id,
      name,
      hue: hashStr(id) % 360,
      kind: 'browser',
      path: null,
      createdTs: Date.now(),
      lastSync: 'just now',
    };
    if (!v) this.meta.vaults = [sv, ...this.meta.vaults];
    saveMeta(this.meta);
    await this.call({ op: 'writeFile', vaultId: id, path: 'README.md', bytes: new TextEncoder().encode(`# ${name}\n\nA fresh **asp** vault in this browser. Everything autosaves and syncs.\n`) });
    return { id, path: '', vault_id: id, enabled: true, listening_ticket: null };
  }

  async cloneRemote(dest: string | null, ticket: string, authKey?: string): Promise<VaultInfo> {
    void dest; // web ignores the destination — vaults live in browser storage.
    await this.ensureBooted();
    const ticketClean = ticket.replace(/\s+/g, '');
    const id = 'v_' + hashStr(ticketClean).toString(36);
    // Init the vault (empty tag → adopt the peer's vault id on connect), then sync.
    await this.call({ op: 'initVault', args: { vaultId: id, seedHex: this.meta.seedHex, vaultTag: '' } });
    await this.restoreOpfs(id);
    const n = (await this.call({ op: 'sync', vaultId: id, ticket: ticketClean, authKey, relayUrl: this.relayUrl || undefined })) as number;
    const name = 'Shared vault';
    const sv: SavedVault = {
      id,
      name,
      hue: hashStr(ticketClean) % 360,
      kind: 'browser',
      path: null,
      createdTs: Date.now(),
      lastSync: n > 0 ? 'just now' : 'no changes',
      peerTicket: ticketClean,
      peerAuthKey: authKey,
    };
    this.meta.vaults = [sv, ...this.meta.vaults.filter((x) => x.id !== id)];
    saveMeta(this.meta);
    return { id, path: '', vault_id: id, enabled: true, listening_ticket: null };
  }

  async removeVault(id: string, trash: boolean): Promise<string> {
    void trash; // web: there's no OS trash — just forget + drop OPFS.
    await this.ensureBooted();
    await this.call({ op: 'freeVault', vaultId: id }).catch(() => {});
    this.meta.vaults = this.meta.vaults.filter((v) => v.id !== id);
    saveMeta(this.meta);
    // Drop the OPFS state file.
    try {
      const root = await navigator.storage.getDirectory();
      const dir = await root.getDirectoryHandle('asp-vaults', { create: true });
      await dir.removeEntry(id);
    } catch {
      /* ignore */
    }
    return id;
  }

  async setAllowConnections(_id: string, _on: boolean, _authKey?: string): Promise<string | null> {
    // A browser thin node never listens (it can't — no inbound QUIC). Share is a
    // desktop-only affordance; on web this returns null and the UI hides Share.
    return null;
  }

  async syncNow(id: string, ticket: string, authKey?: string): Promise<void> {
    await this.ensureBooted();
    // Fall back to the stored peer ticket if none is passed (a browser thin node
    // remembers who it cloned from so "Sync now" works without re-entering it).
    const v = this.meta.vaults.find((x) => x.id === id);
    const t = (ticket && ticket.trim()) || v?.peerTicket || '';
    const k = authKey || v?.peerAuthKey;
    if (!t) throw new Error('no peer ticket set for this vault');
    const n = (await this.call({ op: 'sync', vaultId: id, ticket: t.replace(/\s+/g, ''), authKey: k, relayUrl: this.relayUrl || undefined })) as number;
    if (v) {
      v.lastSync = n > 0 ? 'just now' : 'no changes';
      saveMeta(this.meta);
    }
  }

  async status(id: string): Promise<VaultStatus> {
    await this.ensureBooted();
    const rows = (await this.call({ op: 'rowCount', vaultId: id })) as number;
    const detail = (await this.call({ op: 'filesDetail', vaultId: id })) as FileMeta[];
    const files = detail.filter((f) => !f.deleted).length;
    return { id, vault_id: id, rows, files, head: '', listening_ticket: null, peers: [] };
  }

  async filesTree(id: string): Promise<TreeNode[]> {
    await this.ensureBooted();
    const detail = (await this.call({ op: 'filesDetail', vaultId: id })) as FileMeta[];
    return buildTree(detail.filter((f) => !f.deleted));
  }

  async readFile(id: string, path: string): Promise<string | null> {
    await this.ensureBooted();
    return (await this.call({ op: 'readFile', vaultId: id, path })) as string | null;
  }

  async writeFile(id: string, path: string, content: string): Promise<void> {
    await this.ensureBooted();
    await this.call({ op: 'writeFile', vaultId: id, path, bytes: new TextEncoder().encode(content) });
  }

  async deleteFile(id: string, path: string): Promise<void> {
    await this.ensureBooted();
    await this.call({ op: 'deleteFile', vaultId: id, path });
  }

  async renameFile(id: string, from: string, to: string): Promise<void> {
    await this.ensureBooted();
    await this.call({ op: 'renameFile', vaultId: id, from, to });
  }

  async newFile(id: string, name: string, content: string): Promise<string> {
    // Avoid a name clash by suffixing (mirrors the desktop engine).
    const tree = await this.filesTree(id);
    const taken = new Set<string>();
    const walk = (nodes: TreeNode[]) => {
      for (const n of nodes) {
        if (!n.is_dir) taken.add(n.name);
        if (n.children) walk(n.children);
      }
    };
    walk(tree);
    let chosen = name;
    let i = 1;
    while (taken.has(chosen)) {
      chosen = `untitled-${i}.md`;
      i++;
    }
    await this.writeFile(id, chosen, content);
    return chosen;
  }

  async history(id: string): Promise<HistoryEvent[]> {
    await this.ensureBooted();
    return (await this.call({ op: 'history', vaultId: id })) as HistoryEvent[];
  }

  async fileAtTime(id: string, path: string, ts: number): Promise<FileAtTime> {
    await this.ensureBooted();
    return (await this.call({ op: 'fileAtTime', vaultId: id, path, ts })) as FileAtTime;
  }

  async restoreFileAt(id: string, path: string, ts: number): Promise<boolean> {
    await this.ensureBooted();
    return (await this.call({ op: 'restoreFileAt', vaultId: id, path, ts })) as boolean;
  }

  async snapshot(id: string, name: string): Promise<string> {
    // The thin node doesn't keep named snapshots server-side; persist the engine
    // state under a snapshot key in OPFS so "restore" works for this session.
    await this.ensureBooted();
    const bytes = (await this.call({ op: 'dumpState', vaultId: id })) as Uint8Array;
    const snapId = `snap-${name}-${Date.now().toString(36)}`;
    await this.writeOpfs(`${id}:${snapId}`, bytes);
    return snapId;
  }

  async restore(id: string, target: string): Promise<void> {
    await this.ensureBooted();
    const bytes = await this.readOpfs(`${id}:${target}`);
    if (bytes) await this.call({ op: 'loadState', vaultId: id, bytes });
  }

  async identity(): Promise<string> {
    await this.ensureBooted();
    // Device fingerprint from the seed (the wasm node's ssh key isn't per-vault
    // here; derive a stable device label from the seed for the footer).
    const seedHex = this.meta.seedHex;
    // A short SHA256:-style fingerprint, deterministic from the seed.
    const h = hashStr(seedHex);
    const a = 'ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnpqrstuvwxyz0123456789';
    let hh = h;
    let out = '';
    for (let i = 0; i < 6; i++) {
      out += a[hh % a.length];
      hh = (hh * 1103515245 + 12345) >>> 0;
    }
    let h2 = hashStr(seedHex + 'x');
    let tail = '';
    for (let i = 0; i < 4; i++) {
      tail += a[h2 % a.length];
      h2 = (h2 * 1103515245 + 12345) >>> 0;
    }
    return `SHA256:${out}…${tail}`;
  }
}
