// web-target glue for the one Rust engine — the SAME byte-identical asp-core as
// the nodejs glue (`./engine-node.ts`), via the wasm-pack `web` target
// (`../../../crates/asp-wasm/pkg-web`). The `#engine` imports map selects this
// under the `browser` condition (esbuild for the Obsidian plugin / WebView).
//
// The `web` target instantiates asynchronously: the host inlines the .wasm
// bytes and calls `initEngine(bytes)` once at startup (the Obsidian plugin does
// this in `main.ts`). Every export below is valid only after that resolves.
// @ts-ignore - generated bindings have their own .d.ts alongside the .js
import init, * as wasm from '../../../crates/asp-wasm/pkg-web/asp_wasm.js';
import type { WasmEngineCtor } from './engine-types.ts';

type WasmInput = Uint8Array | ArrayBuffer | Response | URL | WebAssembly.Module | string;
let ready = false;

/** Instantiate the wasm module from inlined bytes (or a URL/Response).
 * Idempotent. The `web` target requires this before any engine use. */
export async function initEngine(input?: WasmInput): Promise<void> {
  if (ready) return;
  // biome-ignore lint/suspicious/noExplicitAny: wasm-bindgen input shape
  await init(input ? ({ module_or_path: input } as any) : undefined);
  ready = true;
}

export const WasmEngine = wasm.WasmEngine as unknown as WasmEngineCtor;
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
