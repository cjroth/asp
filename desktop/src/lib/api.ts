// Thin wrapper over the Tauri command surface (which is itself a thin
// pass-through to asp-desktop-engine → asp-core). No protocol logic in the app.
import { invoke } from '@tauri-apps/api/core';

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

export const api = {
  listVaults: () => invoke<VaultInfo[]>('list_vaults'),
  addLocalFolder: (path: string) => invoke<VaultInfo>('add_local_folder', { path }),
  // `ticket` is an iroh ticket or bare node id (replaces the old ws:// URL).
  cloneRemote: (dest: string, ticket: string, authKey?: string) =>
    invoke<VaultInfo>('clone_remote', { dest, ticket, authKey }),
  setAllowConnections: (id: string, on: boolean, authKey?: string) =>
    invoke<string | null>('set_allow_connections', { id, on, authKey }),
  syncNow: (id: string, ticket: string, authKey?: string) => invoke<void>('sync_now', { id, ticket, authKey }),
  getStatus: (id: string) => invoke<VaultStatus>('get_status', { id }),
  getIdentity: () => invoke<string>('get_identity'),
  authorize: (id: string, pubkey: string) => invoke<void>('authorize', { id, pubkey }),
  createSnapshot: (id: string, name: string) => invoke<string>('create_snapshot', { id, name }),
  restore: (id: string, target: string) => invoke<void>('restore', { id, target }),

  // ---- file surface ----
  listFiles: (id: string) => invoke<FileEntry[]>('list_files', { id }),
  readFile: (id: string, path: string) => invoke<string>('read_file', { id, path }),
  writeFile: (id: string, path: string, content: string) => invoke<void>('write_file', { id, path, content }),
  renameFile: (id: string, oldPath: string, newPath: string) =>
    invoke<void>('rename_file', { id, old: oldPath, new: newPath }),
  deleteFile: (id: string, path: string) => invoke<void>('delete_file', { id, path }),
  history: (id: string) => invoke<HistEvent[]>('history', { id }),
  // `ts` is wall-clock unix SECONDS.
  readFileAt: (id: string, path: string, ts: number) => invoke<FileAt>('read_file_at', { id, path, ts }),
  restoreFileAt: (id: string, path: string, ts: number) => invoke<void>('restore_file_at', { id, path, ts }),
  rescan: (id: string) => invoke<void>('rescan', { id }),
  removeVault: (id: string, trash: boolean) => invoke<void>('remove_vault', { id, trash }),
};
