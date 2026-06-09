/* ====================================================================
   main.tsx · entry — start the engine worker, hand it the wasm, render.
   The wasm bytes AND the engine Web Worker are inlined at build time by
   esbuild (build.mjs) as `__ASP_WASM_B64__` / `__ASP_NETWORK_WORKER__`,
   so the site is a single self-contained bundle — no sibling .wasm and no
   sibling worker file to fetch (works from any static host).

   The whole mesh sim runs inside the worker (off the renderer thread); the
   main thread only holds a thin NetworkProxy + the React UI.
   ==================================================================== */
import React from 'react';
import { createRoot } from 'react-dom/client';
import { NetworkProxy } from './engine/network-proxy.ts';
import { App } from './ui/App.tsx';
import './asp.css';

declare const __ASP_WASM_B64__: string;
declare const __ASP_NETWORK_WORKER__: string;

function wasmBytes(): Uint8Array {
  const bin = atob(__ASP_WASM_B64__);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

async function main() {
  // Start the engine worker from the inlined IIFE (Blob Worker — no sibling
  // file), then ship it the wasm bytes so it can instantiate the engine. Every
  // node the demo creates is a real WasmEngine, now living off the main thread.
  const blob = new Blob([__ASP_NETWORK_WORKER__], { type: 'text/javascript' });
  const worker = new Worker(URL.createObjectURL(blob));
  const proxy = new NetworkProxy(worker);
  await proxy.init(wasmBytes());
  createRoot(document.getElementById('root')!).render(<App api={proxy} />);
}

main().catch((e) => {
  const root = document.getElementById('root');
  if (root) root.textContent = `Failed to start: ${String(e)}`;
  console.error(e);
});
