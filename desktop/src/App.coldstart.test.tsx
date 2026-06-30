// Cold-start: the desktop shell reopens saved folders on a background thread, so
// the initial list_vaults can come back empty while the window is already up. The
// connect screen should show a "Loading your vaults…" hint until the backend's
// `vaults-ready` event fires, then refresh and show the vaults — no manual reload.
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

// Capture the handler App registers for the Tauri `vaults-ready` event so the
// test can fire it (jsdom has no real Tauri IPC). vi.hoisted so the mock factory
// can reach it.
const ev = vi.hoisted(() => ({ handler: null as null | (() => void) }));
vi.mock('@tauri-apps/api/event', () => ({
  listen: async (_name: string, cb: () => void) => {
    ev.handler = cb;
    return () => {};
  },
}));

let VAULTS: { id: string; path: string; vault_id: string; enabled: boolean; listening_ticket: string | null }[] = [];
let READY = false;
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }));
vi.mock('./lib/api', () => ({
  api: {
    listVaults: vi.fn(async () => VAULTS),
    vaultsReady: vi.fn(async () => READY),
    getStatus: vi.fn(async (id: string) => ({ id, vault_id: 'vid', rows: 0, files: 0, head: 'h', listening_ticket: null, peers: [], last_ts: null })),
    getIdentity: vi.fn(async () => 'ssh-ed25519 AAAA me@host'),
    listFiles: vi.fn(async () => []),
    history: vi.fn(async () => []),
    webUpstreams: vi.fn(async () => []),
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
  ev.handler = null;
  VAULTS = [];
  READY = false;
  localStorage.clear();
});

describe('cold-start vaults-ready', () => {
  it('shows a loading hint until vaults-ready, then populates the list', async () => {
    render(<App />);
    // No vaults reopened yet → the loading hint is visible.
    expect(await screen.findByTestId('vaults-loading')).toBeTruthy();

    // The background reopen finishes: the backend now reports a vault and fires
    // the event the shell emits.
    VAULTS = [{ id: 'v1', path: '/home/me/notes', vault_id: 'vid1', enabled: false, listening_ticket: null }];
    await waitFor(() => expect(ev.handler).toBeTruthy());
    ev.handler!();

    // The vault appears and the loading hint goes away — no manual reload.
    await waitFor(() => expect(screen.getByText('notes')).toBeTruthy());
    expect(screen.queryByTestId('vaults-loading')).toBeNull();
  }, 10000);

  // Regression: the one-shot `vaults-ready` event fires from the shell's startup
  // thread and is missed if the reopen finishes before our listener attaches (an
  // empty config reopens instantly). With no vaults and the event never delivered,
  // the loading hint must still clear by querying readiness — not hang forever.
  it('clears the loading hint via readiness query when the event is missed', async () => {
    READY = true; // reopen already finished by the time the webview queries
    render(<App />);
    // The connect screen is up and the perpetual spinner is gone, even though
    // ev.handler is never invoked.
    await waitFor(() => expect(screen.queryByTestId('vaults-loading')).toBeNull());
    expect(screen.getByText('Your vaults')).toBeTruthy();
  }, 10000);
});
