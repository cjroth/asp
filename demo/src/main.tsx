/* ====================================================================
   main.tsx · entry — decode the inlined wasm, init the engine, render.
   The wasm bytes are inlined at build time by esbuild (build.mjs) as
   `__ASP_WASM_B64__`, so the site is a single self-contained bundle —
   no sibling .wasm to fetch (works from any static host).
   ==================================================================== */
import React from 'react';
import { createRoot } from 'react-dom/client';
import { initAsp } from '../../sdks/typescript/src/index.ts';
import { App } from './ui/App.tsx';
import './asp.css';

declare const __ASP_WASM_B64__: string;
function wasmBytes(): Uint8Array {
  const bin = atob(__ASP_WASM_B64__);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

async function main() {
  // Instantiate the one Rust engine (asp-core in wasm) once, from the inlined
  // bytes. Every node the demo creates is a real WasmEngine over this module.
  await initAsp(wasmBytes());
  createRoot(document.getElementById('root')!).render(<App />);
}

main().catch((e) => {
  const root = document.getElementById('root');
  if (root) root.textContent = `Failed to start: ${String(e)}`;
  console.error(e);
});
