import react from '@vitejs/plugin-react';
import { defineConfig, type Plugin } from 'vite';
import { existsSync, readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { exit } from 'node:process';

// Inline the asp wasm bytes (base64) as `__ASP_WASM_B64__` so the engine Web
// Worker is fully self-contained — it never fetches a sibling .wasm file (the
// same constraint that drives the Obsidian plugin's inlining; a sandboxed
// WebView or a static host without WASM MIME config would otherwise break).
//
// The wasm is GENERATED (gitignored) — build it once with `bun run build:wasm`
// (or `cargo install wasm-pack && bun run build:wasm`) before `dev`/`build`.
function aspWasmInline(): Plugin {
  const wasmPath = resolve(__dirname, '../crates/asp-wasm/pkg-web/asp_wasm_bg.wasm');
  if (!existsSync(wasmPath)) {
    console.error(
      `\n[asp] the wasm engine is not built yet. The browser/web target needs it.\n` +
        `[asp] build it once (from the repo root or desktop/):\n` +
        `       bun run build:wasm\n` +
        `[asp] (requires wasm-pack: cargo install wasm-pack)\n`,
    );
    exit(1);
  }
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
