// The wasm engine bindings (asp-core compiled to wasm via asp-wasm). This is the
// REAL full engine — fold, merge, identity, and the sans-IO Session all run here,
// so a TS/wasm node computes byte-identical state to the native `asp` daemon.
// (One engine, thin bindings.)
//
// We import the nodejs-target package built by `scripts/build-wasm.mjs`.
// @ts-ignore - generated bindings have their own .d.ts alongside the .js
import * as wasm from '../../../crates/asp-wasm/pkg/asp_wasm.js';

export const WasmEngine = wasm.WasmEngine as WasmEngineCtor;
export const foldFiles = wasm.fold_files as (rowsJson: string, blobsJson: string) => string;
export const merge3Bytes = wasm.merge3_bytes as (
  cls: string,
  base: Uint8Array,
  ours: Uint8Array,
  theirs: Uint8Array,
) => Uint8Array;
export const contentHash = wasm.content_hash as (bytes: Uint8Array) => string;
export const nodeIdHex = wasm.node_id_hex as (seed: Uint8Array) => string;
export const sshPubkey = wasm.ssh_pubkey as (seed: Uint8Array, comment: string) => string;
export const merkleIdOf = wasm.merkle_id_of as (rowJson: string) => string;

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
