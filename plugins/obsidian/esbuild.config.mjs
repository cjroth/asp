// Bundle the plugin to a single, self-contained `main.js` for desktop AND
// mobile. The community/BRAT install path downloads only `main.js` +
// `manifest.json`, so both the wasm engine AND the engine Web Worker are inlined
// — never emitted as sibling files.
//
// One engine everywhere: the plugin runs the *real* Rust engine via the
// `@asp/sdk`, whose `#engine` imports map resolves to the wasm-pack **web** glue
// under esbuild's `browser` condition. The engine runs inside a Web Worker (off
// the renderer thread, so a sync never freezes the UI):
//   • Phase 1 bundles `engine-worker-entry.ts` to a standalone IIFE string,
//     inlined as `__ASP_ENGINE_WORKER__` and started as a Blob Worker.
//   • Phase 2 bundles `main.ts`, inlining the wasm bytes (`__ASP_WASM_B64__`,
//     base64) — `main.ts` decodes them and ships them to the worker's `init`,
//     so the worker never has to fetch a separate file.
import { existsSync, readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import esbuild from 'esbuild';

const here = dirname(fileURLToPath(import.meta.url));
const prod = process.argv.includes('production');

const wasmPath = resolve(here, '..', '..', 'crates', 'asp-wasm', 'pkg-web', 'asp_wasm_bg.wasm');
if (!existsSync(wasmPath)) {
  console.error(
    `[esbuild] asp wasm not found at ${wasmPath}\n` +
      '[esbuild] Run `bun run build:wasm` (builds the nodejs + web wasm targets) first.',
  );
  process.exit(1);
}
const wasmB64 = readFileSync(wasmPath).toString('base64');

// Node builtins are reached only on desktop via guarded requires; externalize
// so the browser/mobile bundle never tries to resolve them.
const externalNode = ['node:fs', 'node:os', 'node:path', 'node:module'];

// ---- Phase 1: the engine Web Worker, bundled to a standalone IIFE string. ----
const workerResult = await esbuild.build({
  entryPoints: [resolve(here, 'src/engine-worker-entry.ts')],
  bundle: true,
  format: 'iife',
  platform: 'browser',
  target: 'es2020',
  conditions: ['browser'],
  external: ['obsidian', 'electron', ...externalNode],
  loader: { '.wasm': 'binary' },
  define: {
    // The worker always receives wasm bytes via its init message; neutralize
    // the web glue's no-arg `new URL(..., import.meta.url)` branch.
    'import.meta.url': JSON.stringify('asp-plugin://engine-worker'),
    'process.env.NODE_ENV': JSON.stringify(prod ? 'production' : 'development'),
  },
  write: false,
  sourcemap: false,
  treeShaking: true,
  minify: prod,
  logLevel: 'warning',
});
const engineWorkerSrc = workerResult.outputFiles[0].text;

// ---- Phase 2: the plugin main bundle. ----
const buildOpts = {
  entryPoints: [resolve(here, 'src/main.ts')],
  bundle: true,
  format: 'cjs',
  platform: 'browser',
  target: 'es2020',
  conditions: ['browser'],
  external: ['obsidian', 'electron', ...externalNode],
  loader: { '.wasm': 'binary' },
  define: {
    __ASP_WASM_B64__: JSON.stringify(wasmB64),
    __ASP_ENGINE_WORKER__: JSON.stringify(engineWorkerSrc),
    'import.meta.url': JSON.stringify('asp-plugin://main'),
    'process.env.NODE_ENV': JSON.stringify(prod ? 'production' : 'development'),
  },
  outfile: resolve(here, 'main.js'),
  sourcemap: prod ? false : 'inline',
  minify: prod,
  logLevel: 'info',
};

if (prod) {
  await esbuild.build(buildOpts).catch((e) => {
    console.error(e);
    process.exit(1);
  });
} else {
  const ctx = await esbuild.context(buildOpts);
  await ctx.watch();
  console.log('[esbuild] watching for changes…');
}
