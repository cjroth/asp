// Build the one Rust engine (crates/asp-wasm → asp-core) to wasm — the FULL
// engine, merge included (one engine everywhere). Produces the nodejs-target
// pkg consumed by the SDK + the Obsidian plugin (Electron) and a web-target
// pkg-web for the browser/WebView bundle.
import { spawnSync } from 'node:child_process';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const crateDir = resolve(here, '..', '..', '..', 'crates', 'asp-wasm');

for (const [target, out] of [
  ['nodejs', resolve(crateDir, 'pkg')],
  ['web', resolve(crateDir, 'pkg-web')],
]) {
  console.log(`[build-wasm] wasm-pack build --release --target ${target} → ${out}`);
  const res = spawnSync('wasm-pack', ['build', '--release', '--target', target, '--out-dir', out], {
    cwd: crateDir,
    stdio: 'inherit',
  });
  if (res.status !== 0) {
    console.error('[build-wasm] wasm-pack failed. Install: cargo install wasm-pack');
    process.exit(res.status ?? 1);
  }
}
console.log('[build-wasm] one engine, two wasm targets (nodejs + web) built.');
