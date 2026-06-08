// nodejs-target glue for the one Rust engine (asp-core compiled to wasm via
// asp-wasm). The `#engine` imports map selects this under the `default`
// condition (Node/Bun: the SDK's own tests, the parity e2e). The wasm-pack
// `nodejs` package instantiates synchronously at import (via `require`), so the
// engine is ready as soon as this module loads — `initEngine` is a no-op here,
// kept only for API parity with the web glue.
// @ts-ignore - generated bindings have their own .d.ts alongside the .js
import * as wasm from '../../../crates/asp-wasm/pkg/asp_wasm.js';
import type { WasmEngineCtor } from './engine-types.ts';

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

/** No-op: the nodejs glue instantiates the wasm synchronously at import. Present
 * for API parity with the web target (where instantiation is async). */
export async function initEngine(_input?: unknown): Promise<void> {}
