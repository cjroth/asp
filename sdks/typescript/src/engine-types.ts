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
  connect_start(): Uint8Array;
  feed(frame: Uint8Array): string;
  free(): void;
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

export interface FeedResult {
  out: number[][];
  integrated: number;
  authed: boolean;
  closed: string | null;
}
