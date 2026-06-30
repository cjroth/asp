// @asp/sdk — the TypeScript SDK for the Agent Sync Protocol. A thin shim over the
// one Rust engine (asp-core) compiled to wasm. One engine everywhere: a TS/wasm
// node computes byte-identical state to the native `asp` daemon.

export { type EngineVault, MAIN_BRANCH, normalizePeerUrl, Vault, type SyncOptions } from './vault.ts';

// Engine Web Worker: run the wasm engine + transport off the renderer thread.
export { type Port, linkedPorts, selfPort, workerPort } from './worker/channel.ts';
export { EngineWorkerHost } from './worker/engine-host.ts';
export type { Command, FromWorker, Identity, InitPayload, Reply, ToWorker } from './worker/protocol.ts';
export { WorkerVault } from './worker/worker-vault.ts';
export type { BranchInfo, FileMeta, WasmEngineInstance, WireBlob, WireRow } from './engine-types.ts';
export {
  contentHash,
  foldFiles,
  // Initialize the wasm engine. Node/Bun: a no-op (the nodejs glue loads
  // synchronously at import). Browser/WebView (Obsidian): the host inlines the
  // .wasm bytes and `await`s this once before constructing a `Vault`.
  initEngine as initAsp,
  merge3Bytes,
  merkleIdOf,
  nodeIdHex,
  sshPubkey,
  WasmEngine,
} from './engine.ts';
