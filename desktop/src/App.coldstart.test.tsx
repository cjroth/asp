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
let READY = false;
mock.module('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }));
mock.module('./lib/api', () => ({
  api: {
    gitStatus: vi.fn(async () => null), gitPull: vi.fn(async () => {}), gitPendingDiff: vi.fn(async () => ({ filesChanged: 0, paths: [], unified: '' })), gitPush: vi.fn(async () => ({ pushedSha: null, commits: 0 })), cloneGit: vi.fn(),
    listVaults: vi.fn(async () => VAULTS),
    vaultsReady: vi.fn(async () => READY),
    listBranches: vi.fn(async () => [{ branch_id: 'main', name: 'main', parent: null, current: true }]),
    currentBranch: vi.fn(async () => 'main'),
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
  READY = false;
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

  it('shows a determinate progress bar while a large vault reconciles', async () => {
    render(<App />);
    expect(await screen.findByTestId('vaults-loading')).toBeTruthy();
    // The shell streams scan progress for the (still-loading) vault.
    await waitFor(() => expect(handlers['vault-scan-progress']).toBeTruthy());
    handlers['vault-scan-progress']!({ payload: { done: 14000, total: 28000, phase: 'hashing' } });
    // A determinate bar + a live count replace the indeterminate spinner text.
    await waitFor(() => expect(screen.getByTestId('scan-bar')).toBeTruthy());
    expect(screen.getByTestId('scan-count').textContent).toContain('14,000');
    expect(screen.getByText(/Reading changed files/)).toBeTruthy();
    const fill = screen.getByTestId('scan-bar').firstChild as HTMLElement;
    expect(fill.style.width).toBe('50%');
  }, 10000);
});

import { afterAll as __aa, mock as __mk } from 'bun:test';
__aa(() => __mk.restore());
