// The backend surface, abstracted over platform. On desktop it's a thin
// pass-through to the Tauri command layer (→ asp-desktop-engine → asp-core). On
// web it's the same asp-core engine compiled to wasm, persisted to OPFS (see
// webApi.ts). No protocol logic lives in the app either way.
import { invoke } from '@tauri-apps/api/core';
import { isDesktop } from './platform';

export interface VaultInfo {
  id: string;
  path: string;
  vault_id: string;
  enabled: boolean;
  // The iroh connection ticket this folder is listening on (share to pair), or null.
  listening_ticket: string | null;
}
export interface VaultStatus {
  id: string;
  vault_id: string;
  rows: number;
  files: number;
  head: string;
  listening_ticket: string | null;
  peers: string[];
  // Wall-clock unix SECONDS of the most recent log row, or null for an empty vault.
  last_ts: number | null;
}
export interface FileEntry {
  path: string;
  file_id: string;
  is_dir: boolean;
  merge_class: string;
}
export interface HistEvent {
  id: string;
  // Wall-clock unix SECONDS.
  ts: number;
  lamport: number;
  kind: string; // create | edit | rename | delete | reclass
  path: string;
}
export interface FileAt {
  exists: boolean;
  content: string;
}

export interface Api {
  listVaults(): Promise<VaultInfo[]>;
  addLocalFolder(path: string): Promise<VaultInfo>;
  // Create a fresh browser-storage (OPFS) vault. Web-only.
  createVault(name: string): Promise<VaultInfo>;
  cloneRemote(dest: string, ticket: string, authKey?: string): Promise<VaultInfo>;
  setAllowConnections(id: string, on: boolean, authKey?: string): Promise<string | null>;
  // Sync once against `ticket`. On web, omit `ticket` to re-dial the upstream the
  // vault was cloned from. (Web also holds a live connection — see startLiveSync.)
  syncNow(id: string, ticket?: string, authKey?: string): Promise<void>;
  // Web: open and hold a live connection to the upstream, calling `onChange`
  // whenever a remote push lands (realtime, no polling). Desktop syncs live in
  // its background engine, so this is a no-op there. Idempotent per id.
  startLiveSync(id: string, onChange: () => void): Promise<void>;
  stopLiveSync(id: string): Promise<void>;
  getStatus(id: string): Promise<VaultStatus>;
  getIdentity(): Promise<string>;
  authorize(id: string, pubkey: string): Promise<void>;
  createSnapshot(id: string, name: string): Promise<string>;
  restore(id: string, target: string): Promise<void>;
  listFiles(id: string): Promise<FileEntry[]>;
  readFile(id: string, path: string): Promise<string>;
  writeFile(id: string, path: string, content: string): Promise<void>;
  renameFile(id: string, oldPath: string, newPath: string): Promise<void>;
  createDir(id: string, path: string): Promise<void>;
  deleteFile(id: string, path: string): Promise<void>;
  history(id: string): Promise<HistEvent[]>;
  readFileAt(id: string, path: string, ts: number): Promise<FileAt>;
  restoreFileAt(id: string, path: string, ts: number): Promise<void>;
  rescan(id: string): Promise<void>;
  removeVault(id: string, trash: boolean): Promise<void>;
  // Reveal a folder/file in the OS file manager (Finder/Explorer). Desktop-only.
  revealPath(path: string): Promise<void>;
}

// ---- desktop backend: Tauri commands (a thin pass-through) ----
const tauriApi: Api = {
  listVaults: () => invoke<VaultInfo[]>('list_vaults'),
  addLocalFolder: (path) => invoke<VaultInfo>('add_local_folder', { path }),
  createVault: () => Promise.reject(new Error('createVault is web-only')),
  cloneRemote: (dest, ticket, authKey) => invoke<VaultInfo>('clone_remote', { dest, ticket, authKey }),
  setAllowConnections: (id, on, authKey) => invoke<string | null>('set_allow_connections', { id, on, authKey }),
  syncNow: (id, ticket, authKey) => invoke<void>('sync_now', { id, ticket: ticket ?? null, authKey }),
  // Desktop keeps a standing connection in its background engine; nothing for the
  // frontend to hold open, so these are no-ops (the UI refreshes on its poll).
  startLiveSync: async () => {},
  stopLiveSync: async () => {},
  getStatus: (id) => invoke<VaultStatus>('get_status', { id }),
  getIdentity: () => invoke<string>('get_identity'),
  authorize: (id, pubkey) => invoke<void>('authorize', { id, pubkey }),
  createSnapshot: (id, name) => invoke<string>('create_snapshot', { id, name }),
  restore: (id, target) => invoke<void>('restore', { id, target }),
  listFiles: (id) => invoke<FileEntry[]>('list_files', { id }),
  readFile: (id, path) => invoke<string>('read_file', { id, path }),
  writeFile: (id, path, content) => invoke<void>('write_file', { id, path, content }),
  renameFile: (id, oldPath, newPath) => invoke<void>('rename_file', { id, old: oldPath, new: newPath }),
  createDir: (id, path) => invoke<void>('create_dir', { id, path }),
  deleteFile: (id, path) => invoke<void>('delete_file', { id, path }),
  history: (id) => invoke<HistEvent[]>('history', { id }),
  readFileAt: (id, path, ts) => invoke<FileAt>('read_file_at', { id, path, ts }),
  restoreFileAt: (id, path, ts) => invoke<void>('restore_file_at', { id, path, ts }),
  rescan: (id) => invoke<void>('rescan', { id }),
  removeVault: (id, trash) => invoke<void>('remove_vault', { id, trash }),
  revealPath: (path) => invoke<void>('reveal_path', { path }),
};

// The web backend (wasm + OPFS) is heavy, so it's loaded lazily only when we're
// actually running in a browser.
let webApiPromise: Promise<Api> | null = null;
function backend(): Promise<Api> {
  if (isDesktop()) return Promise.resolve(tauriApi);
  if (!webApiPromise) webApiPromise = import('./webApi').then((m) => m.createWebApi());
  return webApiPromise;
}

// `api` dispatches every call to the active backend at call time.
export const api: Api = new Proxy({} as Api, {
  get(_t, prop: string) {
    return (...args: unknown[]) => backend().then((b) => (b as unknown as Record<string, (...a: unknown[]) => unknown>)[prop](...args));
  },
});
