import react from '@vitejs/plugin-react';
import { defineConfig, type Plugin } from 'vite';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';

// Inline the asp wasm bytes (base64) as `__ASP_WASM_B64__` so the engine Web
// Worker is fully self-contained — it never fetches a sibling .wasm file (the
// same constraint that drives the Obsidian plugin's inlining; a sandboxed
// WebView or a static host without WASM MIME config would otherwise break).
function aspWasmInline(): Plugin {
  const wasmPath = resolve(__dirname, '../crates/asp-wasm/pkg-web/asp_wasm_bg.wasm');
  const wasmB64 = readFileSync(wasmPath).toString('base64');
  return {
    name: 'asp-wasm-inline',
    config() {
      return { define: { __ASP_WASM_B64__: JSON.stringify(wasmB64) } };
    },
  };
}

export default defineConfig({
  plugins: [react(), aspWasmInline()],
  clearScreen: false,
  // Tauri expects a fixed dev port; the web build is a static SPA served from dist/.
  server: { port: 1420, strictPort: true },
  build: { outDir: 'dist', target: 'es2021' },
  worker: { format: 'es' },
});
