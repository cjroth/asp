// The single editor-facing API surface. Two backends implement it:
//   • TauriVaultApi — invoke() over the Tauri command surface → DesktopEngine
//     → asp-core (the full node: real folder I/O, a real listen/serve iroh
//     socket, real history from the on-disk log).
//   • WebVaultApi   — the @asp/sdk wasm engine in a Web Worker (iroh-in-wasm),
//     OPFS-persisted. The thin-node surface a browser runs.
// The editor never branches on platform — it calls `api`. The wiring picks the
// backend at construction. HARD INVARIANT: no protocol/merge logic here; every
// method is a call into the real engine (native or wasm).

export type { TreeNode, HistoryEvent, FileAtTime, VaultInfo, VaultStatus } from './types';

import type {
  FileAtTime,
  HistoryEvent,
  TreeNode,
  VaultInfo,
  VaultStatus,
} from './types';

export interface VaultApi {
  /** Whether this backend owns the disk (desktop full-node) vs a browser store. */
  readonly isDesktop: boolean;

  // ---- vault lifecycle ----
  listVaults(): Promise<VaultInfo[]>;
  /** Desktop: open a real folder (Engine::init + capture). Web: no-op-ish (a
   * fresh in-browser vault). */
  addLocalFolder(path: string): Promise<VaultInfo>;
  /** Clone from a listening peer by iroh ticket (desktop: into `dest`; web:
   * into browser storage). */
  cloneRemote(dest: string | null, ticket: string, authKey?: string): Promise<VaultInfo>;
  /** Forget a vault. `trash` (desktop+folder only) moves the dir to OS trash. */
  removeVault(id: string, trash: boolean): Promise<string>;
  /** The listening iroh ticket for a folder (share to pair), or null. Toggles
   *  listening on/off — desktop-only (a browser thin node never listens). */
  setAllowConnections(id: string, on: boolean, authKey?: string): Promise<string | null>;
  /** One-shot sync against a peer by ticket. */
  syncNow(id: string, ticket: string, authKey?: string): Promise<void>;
  status(id: string): Promise<VaultStatus>;

  // ---- editor file surface ----
  filesTree(id: string): Promise<TreeNode[]>;
  readFile(id: string, path: string): Promise<string | null>;
  writeFile(id: string, path: string, content: string): Promise<void>;
  deleteFile(id: string, path: string): Promise<void>;
  renameFile(id: string, from: string, to: string): Promise<void>;
  newFile(id: string, name: string, content: string): Promise<string>;

  // ---- history / point-in-time ----
  history(id: string): Promise<HistoryEvent[]>;
  fileAtTime(id: string, path: string, ts: number): Promise<FileAtTime>;
  restoreFileAt(id: string, path: string, ts: number): Promise<boolean>;
  snapshot(id: string, name: string): Promise<string>;
  restore(id: string, target: string): Promise<void>;

  /** Device SSH pubkey fingerprint (for the footer). */
  identity(): Promise<string>;
}

// ---------------- Tauri backend (desktop full-node) ----------------

class TauriVaultApi implements VaultApi {
  readonly isDesktop = true;
  private invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
    // Lazy import so the web bundle (no @tauri-apps) never resolves this.
    return import('@tauri-apps/api/core').then((m) => m.invoke<T>(cmd, args));
  }
  listVaults(): Promise<VaultInfo[]> {
    return this.invoke<VaultInfo[]>('list_vaults');
  }
  addLocalFolder(path: string): Promise<VaultInfo> {
    return this.invoke<VaultInfo>('add_local_folder', { path });
  }
  cloneRemote(dest: string | null, ticket: string, authKey?: string): Promise<VaultInfo> {
    return this.invoke<VaultInfo>('clone_remote', { dest, ticket, authKey });
  }
  removeVault(id: string, trash: boolean): Promise<string> {
    return this.invoke<string>('remove_vault', { id, trash });
  }
  setAllowConnections(id: string, on: boolean, authKey?: string): Promise<string | null> {
    return this.invoke<string | null>('set_allow_connections', { id, on, authKey });
  }
  syncNow(id: string, ticket: string, authKey?: string): Promise<void> {
    return this.invoke<void>('sync_now', { id, ticket, authKey });
  }
  status(id: string): Promise<VaultStatus> {
    return this.invoke<VaultStatus>('get_status', { id });
  }
  filesTree(id: string): Promise<TreeNode[]> {
    return this.invoke<TreeNode[]>('files_tree', { id });
  }
  readFile(id: string, path: string): Promise<string | null> {
    return this.invoke<string | null>('read_file', { id, path });
  }
  writeFile(id: string, path: string, content: string): Promise<void> {
    return this.invoke<void>('write_file', { id, path, content });
  }
  deleteFile(id: string, path: string): Promise<void> {
    return this.invoke<void>('delete_file', { id, path });
  }
  renameFile(id: string, from: string, to: string): Promise<void> {
    return this.invoke<void>('rename_file', { id, from, to });
  }
  newFile(id: string, name: string, content: string): Promise<string> {
    return this.invoke<string>('new_file', { id, name, content });
  }
  history(id: string): Promise<HistoryEvent[]> {
    return this.invoke<HistoryEvent[]>('history', { id });
  }
  fileAtTime(id: string, path: string, ts: number): Promise<FileAtTime> {
    return this.invoke<FileAtTime>('file_at_time', { id, path, ts });
  }
  restoreFileAt(id: string, path: string, ts: number): Promise<boolean> {
    return this.invoke<boolean>('restore_file_at', { id, path, ts });
  }
  snapshot(id: string, name: string): Promise<string> {
    return this.invoke<string>('create_snapshot', { id, name });
  }
  restore(id: string, target: string): Promise<void> {
    return this.invoke<void>('restore', { id, target });
  }
  async identity(): Promise<string> {
    return this.invoke<string>('get_identity');
  }
}

export { TauriVaultApi };
