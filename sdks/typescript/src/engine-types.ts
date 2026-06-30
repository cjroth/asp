// Shared engine types — the reduced wasm surface, identical for the nodejs and
// web glue (`#engine` resolves to `engine-node.ts` or `engine-web.ts`). Types
// only, so both glue variants can import them without pulling in either wasm
// package.

export interface WasmEngineInstance {
  node_id(): string;
  node_ssh(): string;
  vault_id(): string;
  row_count(): number;
  record_write(path: string, content: Uint8Array): void;
  record_remove(path: string): void;
  record_rename(from: string, to: string): void;
  commit_files(filesJson: string): void;
  /** Stage a batch of files (create/edit, no deletes) with a single fold. */
  write_files(filesJson: string): void;
  /** Author deletes for a JSON array of paths with a single fold. */
  remove_files(pathsJson: string): void;
  /** Full engine state as compact msgpack bytes (rows + each blob once). */
  dump_state(): Uint8Array;
  /** Restore a `dump_state` snapshot; returns rows newly integrated. */
  load_state(bytes: Uint8Array): number;
  files_json(): string;
  read_file(path: string): Uint8Array | undefined;
  /** This node's version vector as JSON `{site_id: max_seq}`. */
  version_vector(): string;
  /** Wire rows a peer (at `peerVvJson`) is missing, as JSON `WireRow[]`. */
  rows_after(peerVvJson: string): string;
  /** Integrate JSON `WireRow[]`; returns the count of newly-integrated rows. */
  integrate(wireRowsJson: string): number;
  /** Per-file fold metadata as JSON `FileMeta[]`. */
  files_detail_json(): string;
  /** Sync over iroh: dial `ticket` (an iroh ticket) via a relay, run the
   * handshake + version-vector catch-up, converge, and close. Resolves with the
   * number of rows integrated from the peer. `relayUrl` overrides the default
   * public relays (a private/test relay). iroh runs inside the wasm module. */
  sync(ticket: string, authKey?: string, relayUrl?: string): Promise<number>;
  // ---- branches (§2, §7): scoped views over the shared log ----
  /** The checked-out branch id (HEAD). */
  current_branch(): string;
  /** All live branches as JSON `BranchInfo[]`. */
  branches_json(): string;
  /** Create a branch off `parent` (forks at its current vv); returns its id. */
  create_branch(name: string, parent: string): string;
  /** Edit-in-the-past ⇒ branch: fork HEAD at wall-clock `t` and switch to it. */
  fork_at(name: string, t: number): string;
  /** Switch HEAD and re-materialize the branch's scoped state. */
  checkout(branchId: string): void;
  /** Soft-delete a branch (main cannot be deleted). */
  delete_branch(branchId: string): void;
  free(): void;
}

/** A branch record (`branches_json`). */
export interface BranchInfo {
  branch_id: string;
  name: string;
  parent: string | null;
  created_lamport: number;
}

/** Per-file fold metadata (`files_detail_json`). */
export interface FileMeta {
  file_id: string;
  path: string;
  result_hash: string | null;
  merge_class: 'text' | 'code' | 'binary' | 'dir';
  deleted: boolean;
  conflict: boolean;
}

/** A content blob shipped with a row (`bytes` is a JSON number array). */
export interface WireBlob {
  hash: string;
  bytes: number[];
}

/** A log row plus the blobs the peer may lack — the catch-up payload unit. */
export interface WireRow {
  row: unknown;
  blobs: WireBlob[];
}

export type WasmEngineCtor = new (seed: Uint8Array, vaultId: string) => WasmEngineInstance;
