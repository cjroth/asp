// The one Rust engine (asp-core compiled to wasm via asp-wasm) — fold, merge,
// identity, and the sans-IO Session all run here, so a TS/wasm node computes
// byte-identical state to the native `asp` daemon. (One engine, thin bindings.)
//
// This barrel re-exports the per-runtime glue selected by the package `imports`
// map: `#engine` → `engine-node.ts` under the `default` condition (Node/Bun) and
// `engine-web.ts` under the `browser` condition (esbuild/Obsidian/WebView). The
// node glue loads the wasm synchronously; the web glue needs an explicit
// `await initEngine(bytes)` first (see `initAsp` in `index.ts`).

export {
  contentHash,
  foldFiles,
  initEngine,
  merge3Bytes,
  merkleIdOf,
  nodeIdHex,
  sshPubkey,
  WasmEngine,
} from '#engine';

export type {
  FeedResult,
  FileMeta,
  WasmEngineCtor,
  WasmEngineInstance,
  WireBlob,
  WireRow,
} from './engine-types.ts';
