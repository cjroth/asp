// Verify the EXACT browser code path: the wasm-pack *web* glue + async
// initAsp(bytes) (WebAssembly.instantiate from inlined bytes), then a real
// clone + converge. This is what the built demo runs in the browser.
import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { initEngine, WasmEngine } from '../../sdks/typescript/src/engine-web.ts';

const here = dirname(fileURLToPath(import.meta.url));
const bytes = new Uint8Array(readFileSync(resolve(here, '..', '..', 'crates', 'asp-wasm', 'pkg-web', 'asp_wasm_bg.wasm')));

let fail = 0;
const check = (n, c) => { console.log(`${c ? '✓' : '✗ FAIL'}  ${n}`); if (!c) fail++; };

await initEngine(bytes);              // the async web-target init
await initEngine(bytes);              // idempotent
check('web-target wasm instantiated from inlined bytes', true);

const seed = (n) => { const b = new Uint8Array(32); b[0] = n; return b; };
const enc = new TextEncoder(), dec = new TextDecoder();

const a = new WasmEngine(seed(1), 'demo');
a.record_write('README.md', enc.encode('# Vault\n'));
check('new methods present on web WasmEngine',
  typeof a.version_vector === 'function' && typeof a.rows_after === 'function'
  && typeof a.integrate === 'function' && typeof a.files_detail_json === 'function');

const b = new WasmEngine(seed(2), a.vault_id());
const n = b.integrate(a.rows_after(b.version_vector()));
check('clone catch-up integrated rows', n === 1);
const fb = JSON.parse(b.files_json());
check('cloned content materialized', dec.decode(Uint8Array.from(fb['README.md'])) === '# Vault\n');
check('files_detail_json works on web glue', JSON.parse(a.files_detail_json())[0].merge_class === 'text');

console.log(fail === 0 ? '\nWEB GLUE: ALL PASS' : `\nWEB GLUE: ${fail} FAILURE(S)`);
process.exit(fail === 0 ? 0 : 1);
