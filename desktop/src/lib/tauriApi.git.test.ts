// Invoke-arg round-trip guard (the f6c1d07 lesson: a Tauri command silently gets
// `null` when the JS `invoke` arg keys don't match the Rust `#[tauri::command]`
// param names — a mismatch neither the TS compiler nor `cargo` catches). This test
// pins the git-bridge desktop bindings' command names + arg-key shapes so a rename
// on either side trips a red test instead of a runtime no-op.
//
// It drives the REAL `tauriApi` path in api.ts (forcing `isDesktop()` true via a
// faux `window.__TAURI__`) with a mocked `invoke` that records `(cmd, args)`.
import { test, expect, mock, beforeAll, afterAll } from 'bun:test';

const calls: { cmd: string; args: Record<string, unknown> }[] = [];

mock.module('@tauri-apps/api/core', () => ({
  invoke: async (cmd: string, args?: Record<string, unknown>) => {
    calls.push({ cmd, args: args ?? {} });
    return null;
  },
}));

// Stub the wasm module so importing `webApi` (to assert its push rejection) stays
// cheap — the 5.8MB wasm never loads, and `webApi.gitPush` throws before it would.
mock.module('asp-wasm', () => ({ default: async () => {}, WasmEngine: class {} }));
mock.module('asp-wasm/asp_wasm_bg.wasm?url', () => ({ default: '' }));

// Make api.ts pick the Tauri backend (not the wasm web backend).
beforeAll(() => {
  (window as unknown as Record<string, unknown>).__TAURI__ = {};
});

// The test process is shared across files (see `bun-isolated`), so revert the faux
// desktop flag — `isDesktop()` then returns false again, routing other files' `api`
// calls back to the web backend (and past the mocked `invoke`), so nothing leaks.
afterAll(() => {
  delete (window as unknown as Record<string, unknown>).__TAURI__;
});

// Imported after the mock is registered so api.ts binds the mocked `invoke`.
const { api } = await import('./api');

test('cloneGit → clone_git with exactly {dest,url,token,depth}', async () => {
  calls.length = 0;
  await api.cloneGit('/dest', 'https://example.com/r.git', 'tok', 3);
  expect(calls).toHaveLength(1);
  expect(calls[0].cmd).toBe('clone_git');
  expect(Object.keys(calls[0].args).sort()).toEqual(['depth', 'dest', 'token', 'url']);
  expect(calls[0].args).toEqual({ dest: '/dest', url: 'https://example.com/r.git', token: 'tok', depth: 3 });
});

test('cloneGit passes undefined token/depth through under the same keys', async () => {
  calls.length = 0;
  await api.cloneGit('/dest', 'https://example.com/r.git', undefined, undefined);
  expect(calls[0].cmd).toBe('clone_git');
  // Keys must still be present (Rust Option<_> params) — arg names are the contract.
  expect(Object.keys(calls[0].args).sort()).toEqual(['depth', 'dest', 'token', 'url']);
  expect(calls[0].args.token).toBeUndefined();
  expect(calls[0].args.depth).toBeUndefined();
});

test('gitPull → git_pull with {id}', async () => {
  calls.length = 0;
  await api.gitPull('vault-1');
  expect(calls[0].cmd).toBe('git_pull');
  expect(calls[0].args).toEqual({ id: 'vault-1' });
});

test('gitStatus → git_status with {id}', async () => {
  calls.length = 0;
  await api.gitStatus('vault-1');
  expect(calls[0].cmd).toBe('git_status');
  expect(calls[0].args).toEqual({ id: 'vault-1' });
});

test('gitPush → git_push with exactly {id,message}', async () => {
  calls.length = 0;
  await api.gitPush('vault-1', 'asp: 2 file(s) changed');
  expect(calls[0].cmd).toBe('git_push');
  expect(Object.keys(calls[0].args).sort()).toEqual(['id', 'message']);
  expect(calls[0].args).toEqual({ id: 'vault-1', message: 'asp: 2 file(s) changed' });
});

test('gitPendingDiff → git_pending_diff with {id}', async () => {
  calls.length = 0;
  await api.gitPendingDiff('vault-1');
  expect(calls[0].cmd).toBe('git_pending_diff');
  expect(calls[0].args).toEqual({ id: 'vault-1' });
});

// Push is a spec non-goal in the browser — the web backend must reject clearly
// (never silently no-op) so the UI can point the user at desktop/CLI.
test('webApi.gitPush rejects (browser is clone/pull only)', async () => {
  const { createWebApi } = await import('./webApi');
  const webApi = createWebApi();
  await expect(webApi.gitPush('vault-1', 'msg')).rejects.toThrow(/isn't supported/);
});
