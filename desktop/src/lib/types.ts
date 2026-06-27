// Shared wire types for the editor ↔ engine boundary. These mirror the Rust
// structs in `desktop/engine/src/lib.rs` (VaultInfo, VaultStatus, TreeNode,
// HistoryEvent, FileAtTime) and the wasm engine's FileMeta — the editor treats
// them as the contract, never reaching into engine internals.

export interface VaultInfo {
  id: string;
  path: string;
  vault_id: string;
  enabled: boolean;
  /** The iroh connection ticket this folder is listening on (share to pair), or null. */
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

export interface TreeNode {
  name: string;
  path: string;
  is_dir: boolean;
  children?: TreeNode[];
}

export interface HistoryEvent {
  id: string;
  ts: number;
  lamport: number;
  kind: 'create' | 'edit' | 'rename' | 'delete' | 'reclass';
  path: string | null;
}

export interface FileAtTime {
  exists: boolean;
  content: string | null;
  key: string;
}
