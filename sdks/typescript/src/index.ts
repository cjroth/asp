// @asp/sdk — the TypeScript SDK for the Agent Sync Protocol. A thin shim over the
// one Rust engine (asp-core) compiled to wasm. One engine everywhere: a TS/wasm
// node computes byte-identical state to the native `asp` daemon.

export { Vault, type SyncOptions } from './vault.ts';
export {
  contentHash,
  foldFiles,
  merge3Bytes,
  merkleIdOf,
  nodeIdHex,
  sshPubkey,
  WasmEngine,
} from './engine.ts';
