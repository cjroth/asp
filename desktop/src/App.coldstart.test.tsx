import { mock } from 'bun:test';
// Cold-start: the desktop shell reopens saved folders on a background thread, so
// the initial list_vaults can come back empty while the window is already up. The
// connect screen shows a "Loading your vaults…" hint until the engine emits a
// `vaults-changed` event (a reopened folder landed), then refreshes and shows the
// vaults — no manual reload.
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from './test-shim';

// Capture the handler App registers per Tauri event so the test can fire it
// (jsdom has no real Tauri IPC). Keyed by event name — App registers several.
const handlers: Record<string, (e?: unknown) => void> = {};
mock.module('@tauri-apps/api/event', () => ({
  listen: async (name: string, cb: (e?: unknown) => void) => {
    handlers[name] = cb;
    return () => {};
  },
}));

let VAULTS: { id: string; path: string; vault_id: string; enabled: boolean; listening_ticket: string | null }[] = [];
mock.module('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }));
mock.module('./lib/api', () => ({
  api: {
    listVaults: vi.fn(async () => VAULTS),
    getStatus: vi.fn(async (id: string) => ({ id, vault_id: 'vid', rows: 0, files: 0, head: 'h', listening_ticket: null, peers: [], last_ts: null })),
    getIdentity: vi.fn(async () => 'ssh-ed25519 AAAA me@host'),
    listFiles: vi.fn(async () => []),
    history: vi.fn(async () => []),
    addLocalFolder: vi.fn(),
    cloneRemote: vi.fn(),
    createVault: vi.fn(),
    readFile: vi.fn(async () => ''),
    writeFile: vi.fn(async () => {}),
    removeVault: vi.fn(),
  },
}));

import App from './App';

afterEach(cleanup);
beforeEach(() => {
  for (const k of Object.keys(handlers)) delete handlers[k];
  VAULTS = [];
  localStorage.clear();
});

describe('cold-start loading hint', () => {
  it('shows a loading hint until a vault lands, then populates the list', async () => {
    render(<App />);
    // No vaults reopened yet → the loading hint is visible.
    expect(await screen.findByTestId('vaults-loading')).toBeTruthy();

    // The background reopen finishes: the backend now reports a vault and fires
    // the `vaults-changed` event the shell emits as each folder lands.
    VAULTS = [{ id: 'v1', path: '/home/me/notes', vault_id: 'vid1', enabled: false, listening_ticket: null }];
    await waitFor(() => expect(handlers['vaults-changed']).toBeTruthy());
    handlers['vaults-changed']!();

    // The vault appears and the loading hint goes away — no manual reload.
    await waitFor(() => expect(screen.getByText('notes')).toBeTruthy());
    expect(screen.queryByTestId('vaults-loading')).toBeNull();
  }, 10000);
});

import { afterAll as __aa, mock as __mk } from 'bun:test';
__aa(() => __mk.restore());
