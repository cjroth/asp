// Web git-bridge slice (git-bridge §7.3). Drives the REAL wasm engine's browser
// clone path against the checked-in smart-HTTP wire fixtures via a mock transport —
// no network, no relay, no browser. Uses the nodejs-target wasm build (loads
// synchronously under bun); if it isn't built yet (`desktop/scripts/sync-wasm.sh`),
// the wasm-dependent test skips rather than failing the suite.
import { test, expect } from 'bun:test';
import { existsSync, readFileSync } from 'node:fs';
import { join } from 'node:path';

const PKG = join(import.meta.dir, '../../../crates/asp-wasm/pkg/asp_wasm.js');
const HAVE_WASM = existsSync(PKG);

const FIX = join(import.meta.dir, '../../../tests/e2e/fixtures');
const readFix = (n: string) => new Uint8Array(readFileSync(join(FIX, n)));
const dec = new TextDecoder();

// A mock of the JS `fetch_fn` the wasm calls: routes by HTTP method + the git
// command carried in the POST body, replaying the recorded response bytes.
function mockGitFetch() {
  const info = readFix('info_refs_v2.bin');
  const ls = readFix('ls_refs_v2.bin');
  const fetchResp = readFix('fetch_v2.bin');
  const calls: { method: string; url: string }[] = [];
  const fn = async (method: string, url: string, _headers: Record<string, string>, body: Uint8Array | null) => {
    calls.push({ method, url });
    if (method === 'GET' && url.endsWith('/info/refs?service=git-upload-pack')) return { status: 200, body: info };
    if (method === 'POST' && url.endsWith('/git-upload-pack')) {
      const b = body ? dec.decode(body) : '';
      if (b.includes('command=ls-refs')) return { status: 200, body: ls };
      if (b.includes('command=fetch')) return { status: 200, body: fetchResp };
    }
    return { status: 404, body: new Uint8Array() };
  };
  return { fn, calls };
}

test.skipIf(!HAVE_WASM)('git_proxy_urls shapes the proxy endpoints per the gitproxy contract', async () => {
  const wasm = (await import(PKG)) as typeof import('asp-wasm');
  const u = JSON.parse(wasm.git_proxy_urls('https://relay.test/', 'https://github.com/owner/repo.git'));
  expect(u.base).toBe('https://relay.test/git/github.com/owner/repo.git');
  expect(u.info_refs).toBe('https://relay.test/git/github.com/owner/repo.git/info/refs?service=git-upload-pack');
  expect(u.upload_pack).toBe('https://relay.test/git/github.com/owner/repo.git/git-upload-pack');
  // ssh URLs are native-only; the browser proxy path rejects them.
  expect(() => wasm.git_proxy_urls('https://relay.test', 'git@github.com:o/r')).toThrow();
});

test.skipIf(!HAVE_WASM)('WasmEngine.git_clone folds the repo tree from recorded git wire bytes', async () => {
  const wasm = (await import(PKG)) as typeof import('asp-wasm');
  const eng = new wasm.WasmEngine(new Uint8Array(32).fill(7), ''); // pristine → adopts the git vault id
  const { fn, calls } = mockGitFetch();

  const reportJson = await eng.git_clone('https://github.com/owner/repo', undefined, 'https://relay.test', undefined, fn, undefined);
  const report = JSON.parse(reportJson) as { vault_id: string; commits: number; default_branch: string; remote_ref: string };

  // The transport was hit with correctly-shaped proxy URLs (GET info/refs, POST pack).
  expect(calls[0]).toEqual({ method: 'GET', url: 'https://relay.test/git/github.com/owner/repo/info/refs?service=git-upload-pack' });
  expect(calls.some((c) => c.method === 'POST' && c.url === 'https://relay.test/git/github.com/owner/repo/git-upload-pack')).toBe(true);

  // The engine folded the `linear_basic` tip tree: a.txt→a2.txt (3 lines), dir/b.txt
  // deleted, dir/c.txt added, plus the clone-seeded .aspignore.
  const files = JSON.parse(eng.files_json()) as Record<string, number[]>;
  const text = (p: string) => dec.decode(new Uint8Array(files[p]));
  expect(text('a2.txt')).toBe('alpha\nalpha2\nalpha3\n');
  expect(text('dir/c.txt')).toBe('charlie\n');
  expect(files['a.txt']).toBeUndefined();
  expect(files['dir/b.txt']).toBeUndefined();
  expect(files['.aspignore']).toBeDefined();

  // Report + ledger are coherent.
  expect(report.default_branch).toBe('main');
  expect(report.remote_ref).toBe('refs/heads/main');
  expect(eng.vault_id()).toBe(report.vault_id);
  const ledger = JSON.parse(eng.git_ledger_json()) as { at_sha: string | null; ingested: number };
  expect(ledger.at_sha).toBe('89d2010db2188ca6e11e8eaf7a844e7eea72f869');
  expect(ledger.ingested).toBe(report.commits);
});
