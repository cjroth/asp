// Bundle the plugin to a single main.js. `obsidian` is provided by the host at
// runtime (external). The one engine (asp-core) is bundled via the wasm SDK: the
// nodejs-target pkg works on Obsidian desktop (Electron); the wasm binary is
// emitted alongside as a `file` and loaded by the generated glue. For the mobile
// (Capacitor WebView) bundle, switch the SDK's `#engine` import to the web target
// and inline `pkg-web/asp_wasm_bg.wasm` as base64 (csp's approach) — documented,
// not wired here.
import esbuild from 'esbuild';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const prod = process.argv.includes('production');

await esbuild
  .build({
    entryPoints: [resolve(here, 'src/main.ts')],
    bundle: true,
    format: 'cjs',
    platform: 'node',
    target: 'es2020',
    external: ['obsidian', 'electron'],
    loader: { '.wasm': 'file' },
    outfile: resolve(here, 'main.js'),
    sourcemap: prod ? false : 'inline',
    minify: prod,
    logLevel: 'info',
  })
  .catch((e) => {
    console.error(e);
    process.exit(1);
  });
