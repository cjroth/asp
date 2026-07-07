// Build the one Rust engine (crates/asp-wasm → asp-core) to wasm — the FULL
// engine, merge included (one engine everywhere). Produces the nodejs-target
// pkg consumed by the SDK + the Obsidian plugin (Electron) and a web-target
// pkg-web for the browser/WebView bundle.
import { execFileSync, spawnSync } from 'node:child_process';
import { existsSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const crateDir = resolve(here, '..', '..', '..', 'crates', 'asp-wasm');

// ring (pulled in via iroh's tls-ring feature) compiles its BN/limbs C fallback
// to wasm objects, then archives them into a static lib the wasm link consumes.
// Apple's /usr/bin/ar can't archive non–Mach-O members: it silently emits an
// EMPTY archive, so the link dies on `undefined symbol: ring_core_*__limbs_mul_add_limb`
// (and bn_from_montgomery_in_place). GNU ar on Linux tolerates it; macOS does not.
// Fix: point ring's cc build at llvm-ar (wasm-aware) for the wasm target. The Rust
// toolchain ships one under its sysroot (llvm-tools). Respect a caller override.
function wasmArchiver() {
  if (process.env.AR_wasm32_unknown_unknown) return process.env.AR_wasm32_unknown_unknown; // caller-set
  const candidates = [];
  try {
    const sysroot = execFileSync('rustc', ['--print', 'sysroot'], { encoding: 'utf8' }).trim();
    const host = (execFileSync('rustc', ['-vV'], { encoding: 'utf8' }).match(/^host:\s*(\S+)/m) ?? [])[1];
    if (host) candidates.push(join(sysroot, 'lib', 'rustlib', host, 'bin', 'llvm-ar'));
  } catch {
    // rustc not on PATH — cargo/wasm-pack will surface a clearer error below.
  }
  candidates.push('/opt/homebrew/opt/llvm/bin/llvm-ar', '/usr/local/opt/llvm/bin/llvm-ar');
  return candidates.find((p) => existsSync(p)) ?? null;
}

const AR = wasmArchiver();
if (!AR && process.platform === 'darwin') {
  console.warn(
    '[build-wasm] no wasm-capable llvm-ar found; Apple ar will emit an empty ring\n' +
      '            archive and the wasm link will fail. Fix: rustup component add llvm-tools',
  );
}

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
    env: {
      ...process.env,
      RUSTFLAGS: `${process.env.RUSTFLAGS ?? ''} --cfg getrandom_backend="wasm_js"`.trim(),
      // cc reads AR_<target-with-underscores>; steers ring's archiver to llvm-ar (see above).
      ...(AR ? { AR_wasm32_unknown_unknown: AR } : {}),
    },
  });
  if (res.status !== 0) {
    console.error('[build-wasm] wasm-pack failed. Install: cargo install wasm-pack');
    process.exit(res.status ?? 1);
  }
}
console.log('[build-wasm] one engine, two wasm targets (nodejs + web) built.');
