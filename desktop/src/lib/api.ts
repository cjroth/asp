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
};
