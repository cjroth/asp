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
  connect_start(): Uint8Array;
  feed(frame: Uint8Array): string;
  free(): void;
}

export type WasmEngineCtor = new (seed: Uint8Array, vaultId: string) => WasmEngineInstance;

export interface FeedResult {
  out: number[][];
  integrated: number;
  authed: boolean;
  closed: string | null;
}
