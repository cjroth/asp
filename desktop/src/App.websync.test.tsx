// Web live-sync: browser (wasm/OPFS) vaults have no standing connector, so the
// app's 10s poll must drive catch-up by calling syncNow against each cloned
// vault's stored upstream ticket. Vaults WITHOUT an upstream (locally created)
// must not be synced. Desktop is unaffected (it returns no upstreams).
import { cleanup, render, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const UPSTREAMS = [{ id: 'wc', ticket: 'peer-ticket', authKey: 'k' }];
const syncNow = vi.fn(async () => {});
const webUpstreams = vi.fn(async () => UPSTREAMS);

vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn(async () => null) }));
vi.mock('./lib/api', () => ({
  api: {
    listVaults: vi.fn(async () => [
      { id: 'wc', path: '', vault_id: 'vcloned', enabled: true, listening_ticket: null },
      { id: 'wl', path: '', vault_id: 'vlocal', enabled: true, listening_ticket: null },
    ]),
    getIdentity: vi.fn(async () => 'ssh-ed25519 WEBKEY me@browser'),
    getStatus: vi.fn(async (id: string) => ({ id, vault_id: id, rows: 0, files: 0, head: '', listening_ticket: null, peers: [], last_ts: null })),
    listFiles: vi.fn(async () => []),
    readFile: vi.fn(async () => ''),
    writeFile: vi.fn(),
    renameFile: vi.fn(),
    createDir: vi.fn(),
    deleteFile: vi.fn(),
    history: vi.fn(async () => []),
    readFileAt: vi.fn(async () => ({ exists: false, content: '' })),
    restoreFileAt: vi.fn(),
    removeVault: vi.fn(),
    createVault: vi.fn(),
    cloneRemote: vi.fn(),
    setAllowConnections: vi.fn(async () => null),
    addLocalFolder: vi.fn(),
    syncNow: (id: string, ticket: string, authKey?: string) => syncNow(id, ticket, authKey),
    webUpstreams: () => webUpstreams(),
  },
}));

import App from './App';

const w = window as unknown as Record<string, unknown>;
beforeEach(() => {
  vi.clearAllMocks();
  delete w.__TAURI_INTERNALS__;
  delete w.__TAURI__;
  localStorage.clear();
  window.history.replaceState(null, '', '/');
});
afterEach(() => {
  cleanup();
  w.__TAURI_INTERNALS__ = {};
});

describe('web live-sync from the poll', () => {
  it('syncs cloned web vaults against their stored upstream, and only those', async () => {
    render(<App />);
    // The app's background poll fires every 10s; wait for it to drive catch-up.
    await waitFor(() => expect(webUpstreams).toHaveBeenCalled(), { timeout: 12000 });
    await waitFor(() => expect(syncNow).toHaveBeenCalledWith('wc', 'peer-ticket', 'k'), { timeout: 12000 });
    // The locally-created vault (no upstream) is never synced.
    expect(syncNow).not.toHaveBeenCalledWith('wl', expect.anything(), expect.anything());
  }, 15000);
});
