// Build the demo to a single self-contained bundle: esbuild bundles the React
// app + the @asp/sdk (its `#engine` import resolves to the wasm-pack `web` glue
// under the `browser` condition) and inlines the web wasm as base64
// (`__ASP_WASM_B64__`), exactly like the Obsidian plugin. Output: dist/main.js
// + dist/main.css + dist/index.html — host anywhere, no sibling .wasm.
import { copyFileSync, existsSync, mkdirSync, readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import esbuild from 'esbuild';

const here = dirname(fileURLToPath(import.meta.url));
const prod = process.argv.includes('production');

const wasmPath = resolve(here, '..', 'crates', 'asp-wasm', 'pkg-web', 'asp_wasm_bg.wasm');
if (!existsSync(wasmPath)) {
  console.error(`[build] asp web wasm not found at ${wasmPath}\n[build] Run \`bun run build:wasm\` first.`);
  process.exit(1);
}
const wasmB64 = readFileSync(wasmPath).toString('base64');
console.log(`[build] inlining wasm: ${(wasmB64.length / 1024).toFixed(0)} KiB base64`);

const dist = resolve(here, 'dist');
mkdirSync(dist, { recursive: true });

const opts = {
  entryPoints: [resolve(here, 'src/main.tsx')],
  bundle: true,
  format: 'esm',
  platform: 'browser',
  target: 'es2020',
  // Resolve @asp/sdk's `#engine` subpath import to the web wasm glue.
  conditions: ['browser'],
  define: {
    __ASP_WASM_B64__: JSON.stringify(wasmB64),
    'process.env.NODE_ENV': JSON.stringify(prod ? 'production' : 'development'),
  },
  outfile: resolve(dist, 'main.js'),
  sourcemap: prod ? false : 'inline',
  minify: prod,
  logLevel: 'info',
};

await esbuild.build(opts).catch((e) => { console.error(e); process.exit(1); });
copyFileSync(resolve(here, 'index.html'), resolve(dist, 'index.html'));
console.log('[build] done → demo/dist (open index.html or `bun run serve`)');
