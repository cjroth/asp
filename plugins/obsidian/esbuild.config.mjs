// Bundle the plugin to a single, self-contained `main.js` for desktop AND
// mobile. The community/BRAT install path downloads only `main.js` +
// `manifest.json`, so the wasm engine MUST be inlined — never emitted as a
// sibling file.
//
// One engine everywhere: the plugin runs the *real* Rust engine via the
// `@asp/sdk`, whose `#engine` imports map resolves to the wasm-pack **web** glue
// under esbuild's `browser` condition. The web glue instantiates from bytes, so
// we read the SDK's `pkg-web/asp_wasm_bg.wasm` and inline it as base64
// (`__ASP_WASM_B64__`); `main.ts` decodes it and passes it to `initAsp()` once
// at startup, so the host never has to fetch a separate file (mobile WebViews
// can't fetch arbitrary local URLs).
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

const buildOpts = {
  entryPoints: [resolve(here, 'src/main.ts')],
  bundle: true,
  format: 'cjs',
  platform: 'browser',
  target: 'es2020',
  // Resolve `@asp/sdk`'s `#engine` import to the web wasm glue.
  conditions: ['browser'],
  external: [
    'obsidian',
    'electron',
    // Node builtins are reached only on desktop via guarded requires;
    // externalize so the browser/mobile bundle never tries to resolve them.
    'node:fs',
    'node:os',
    'node:path',
    'node:module',
  ],
  loader: { '.wasm': 'binary' },
  define: {
    __ASP_WASM_B64__: JSON.stringify(wasmB64),
    // The web glue's no-arg init branch touches `import.meta.url`; we always
    // pass bytes explicitly, so neutralize it (esbuild warns on `import.meta`
    // under format=cjs otherwise).
    'import.meta.url': JSON.stringify('asp-plugin://main'),
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
