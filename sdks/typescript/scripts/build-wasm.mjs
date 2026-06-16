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
    // iroh-in-wasm pulls getrandom 0.3+, whose browser backend is opt-in via this
    // cfg (no UDP in the sandbox → iroh relays QUIC over a WebSocket).
    env: { ...process.env, RUSTFLAGS: `${process.env.RUSTFLAGS ?? ''} --cfg getrandom_backend="wasm_js"`.trim() },
  });
  if (res.status !== 0) {
    console.error('[build-wasm] wasm-pack failed. Install: cargo install wasm-pack');
    process.exit(res.status ?? 1);
  }
}
console.log('[build-wasm] one engine, two wasm targets (nodejs + web) built.');
