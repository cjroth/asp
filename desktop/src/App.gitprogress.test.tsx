import { mock } from 'bun:test';
// Desktop git clone must drive the connect-modal's determinate bar from the shell's
// `vault-scan-progress` event (phases fetching→replaying→saving→materialize).
// Regression: those events were routed to the cold-start reconcile bar, not the clone
// bar, so the modal sat on a static "Fetching…" for the whole clone. The fix gates the
// listener on an in-flight-clone ref so a clone's events land in `cloneProg`.
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from './test-shim';

const handlers: Record<string, (e?: unknown) => void> = {};
mock.module('@tauri-apps/api/event', () => ({
  listen: async (name: string, cb: (e?: unknown) => void) => { handlers[name] = cb; return () => {}; },
}));
mock.module('@tauri-apps/plugin-dialog', () => ({ open: vi.fn(async () => '/home/me/dest') }));

// Hold the clone "in flight" so the progress event arrives mid-clone (cloningRef=true).
let resolveClone: (v: unknown) => void = () => {};
const cloneGit = vi.fn(() => new Promise((res) => { resolveClone = res; }));
mock.module('./lib/api', () => ({
  api: {
    listVaults: vi.fn(async () => []), vaultsReady: vi.fn(async () => true),
    getIdentity: vi.fn(async () => 'ssh-ed25519 KEY me@host'),
    listBranches: vi.fn(async () => [{ branch_id: 'main', name: 'main', parent: null, current: true }]),
    currentBranch: vi.fn(async () => 'main'),
    getStatus: vi.fn(async (id: string) => ({ id, vault_id: 'v', rows: 0, files: 0, head: '', listening_ticket: null, peers: [], last_ts: null })),
    gitStatus: vi.fn(async () => null), listFiles: vi.fn(async () => []), history: vi.fn(async () => []),
    readFile: vi.fn(async () => ''), writeFile: vi.fn(), removeVault: vi.fn(), createVault: vi.fn(), addLocalFolder: vi.fn(),
    cloneRemote: vi.fn(), cloneGit: (...a: unknown[]) => (cloneGit as (...x: unknown[]) => unknown)(...a),
  },
}));

import App from './App';

const w = window as unknown as Record<string, unknown>;
afterEach(cleanup);
beforeEach(() => { for (const k of Object.keys(handlers)) delete handlers[k]; localStorage.clear(); w.__TAURI_INTERNALS__ = {}; });

describe('desktop git clone progress', () => {
  it('routes vault-scan-progress to the clone bar (not the scan bar) during a clone', async () => {
    render(<App />);
    fireEvent.click(await screen.findByText('Connect Vault'));
    // Desktop needs a destination folder (open() is mocked to return one).
    fireEvent.click(screen.getByText('Choose…'));
    await waitFor(() => expect(screen.getByText('/home/me/dest')).toBeTruthy());
    fireEvent.change(screen.getByPlaceholderText(/Paste an invite code/), { target: { value: 'https://github.com/octo/repo.git' } });
    await screen.findByTestId('git-token-field');
    fireEvent.click(screen.getByRole('button', { name: 'Connect' }));
    await waitFor(() => expect(cloneGit).toHaveBeenCalled());

    // The shell streams a clone-phase progress event mid-clone.
    await waitFor(() => expect(handlers['vault-scan-progress']).toBeTruthy());
    handlers['vault-scan-progress']!({ payload: { path: '/home/me/dest', done: 5000, total: 10000, phase: 'replaying' } });

    // It drives the CLONE bar (determinate weighted %), not the cold-start scan bar.
    const bar = await screen.findByTestId('clone-progress');
    await waitFor(() => expect(bar.getAttribute('data-phase')).toBe('replaying'));
    expect(bar.getAttribute('data-done')).toBe('5000');
    expect(bar.getAttribute('data-total')).toBe('10000');
    // replaying occupies the [25%,60%] slice; 50% through → ~43% overall.
    const pct = Number(bar.getAttribute('data-pct'));
    expect(pct).toBeGreaterThan(25);
    expect(pct).toBeLessThanOrEqual(60);
    expect(screen.queryByTestId('scan-bar')).toBeNull();

    resolveClone({ id: 'g1', path: '/home/me/dest', vault_id: 'gv1', enabled: true, listening_ticket: null });
  }, 10000);
});

import { afterAll as __aa, mock as __mk } from 'bun:test';
__aa(() => __mk.restore());
