import { mock } from 'bun:test';
// Web-platform behavior: in a plain browser (no Tauri) the app uses OPFS browser
// storage — it must NOT ask to open a folder, and "New Vault" creates a vault
// directly via api.createVault.
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from './test-shim';

const createVault = vi.fn(async (name: string) => ({ id: 'w1', path: '', vault_id: 'wv1', enabled: true, listening_ticket: null }));
const listFiles = vi.fn(async () => [{ path: 'README.md', file_id: 'README.md', is_dir: false, merge_class: 'text' }]);
const setAllowConnections = vi.fn(async () => null);

mock.module('@tauri-apps/plugin-dialog', () => ({ open: vi.fn(async () => null) }));
mock.module('./lib/api', () => ({
  api: {
    startLiveSync: vi.fn(), stopLiveSync: vi.fn(), setLocalRelay: vi.fn(async () => false), getLocalRelay: vi.fn(async () => false),
    listVaults: vi.fn(async () => [{ id: 'w0', path: '', vault_id: 'wv0', enabled: true, listening_ticket: null }]),
    getIdentity: vi.fn(async () => 'ssh-ed25519 WEBKEY me@browser'),
    getStatus: vi.fn(async (id: string) => ({ id, vault_id: 'wv1', rows: 1, files: 1, head: '', listening_ticket: null, peers: [], last_ts: null })),
    createVault: (n: string) => createVault(n),
    listFiles: () => listFiles(),
    readFile: vi.fn(async () => '# New vault\n\nhi'),
    writeFile: vi.fn(),
    renameFile: vi.fn(),
    createDir: vi.fn(),
    deleteFile: vi.fn(),
    history: vi.fn(async () => []),
    readFileAt: vi.fn(async () => ({ exists: true, content: '' })),
    restoreFileAt: vi.fn(),
    removeVault: vi.fn(),
    cloneRemote: vi.fn(),
    setAllowConnections: () => setAllowConnections(),
    addLocalFolder: vi.fn(),
  },
}));

import App, { __resetUrlRestore } from './App';
import { buildHash, parseHash } from './vault/tabs';

const w = window as unknown as Record<string, unknown>;
beforeEach(() => {
  vi.clearAllMocks();
  delete w.__TAURI_INTERNALS__;
  delete w.__TAURI__;
  localStorage.clear();
  __resetUrlRestore();
  window.history.replaceState(null, '', '/');
});
afterEach(() => { cleanup(); w.__TAURI_INTERNALS__ = {}; });

describe('App on the web (OPFS, no Tauri)', () => {
  it('shows browser-storage messaging instead of a folder picker', async () => {
    render(<App />);
    expect(await screen.findByText('Your vaults')).toBeTruthy();
    expect(screen.getByText('Saved in this browser')).toBeTruthy();
    // Recent vault rows show browser storage, not a folder path.
    expect(screen.getByText('Using browser storage')).toBeTruthy();
  });

  it('offers "New vault…" (not "Open a folder…") in the switcher', async () => {
    render(<App />);
    fireEvent.click(await screen.findByText('Using browser storage')); // open the web vault
    await screen.findByText('Files');
    fireEvent.click(screen.getByTestId('vault-switcher'));
    expect(await screen.findByText('New vault…')).toBeTruthy();
    expect(screen.queryByText('Open another folder…')).toBeNull();
    fireEvent.click(screen.getByText('New vault…'));
    expect(await screen.findByPlaceholderText('My vault')).toBeTruthy(); // entry modal opened
  });

  it('creates a browser vault with no folder chooser', async () => {
    render(<App />);
    fireEvent.click(await screen.findByText('New Vault'));
    // The entry modal must NOT offer a folder destination on web.
    expect(screen.queryByText('Choose…')).toBeNull();
    expect(screen.queryByText('Location')).toBeNull();
    fireEvent.change(screen.getByPlaceholderText('My vault'), { target: { value: 'Browser Notes' } });
    fireEvent.click(screen.getByText('Create vault'));
    await waitFor(() => expect(createVault).toHaveBeenCalledWith('Browser Notes'));
    // It opens straight into the editor.
    await screen.findByText('Files');
  });

  // The URL-hash restore path must behave identically on web (no Tauri) — same
  // location.hash read, no path-based routing. The seeded vault is wv0/README.md.
  it('restores the vault + file from the URL hash on mount (refresh, web)', async () => {
    window.history.replaceState(null, '', buildHash('wv0', 'README.md'));
    render(<App />);
    // Without a click it lands directly in the editor (a plain load shows the
    // connect screen until a vault is chosen) and writes the active file back.
    await screen.findByText('Files');
    await screen.findByTestId('tab-bar');
    expect(parseHash(window.location.hash)).toEqual({ vaultId: 'wv0', path: 'README.md' });
    expect(screen.getByTestId('tab').getAttribute('data-path')).toBe('README.md');
  });

  it('Share shows an unavailable message (no socket on browser vaults) and never hangs on Generating…', async () => {
    render(<App />);
    fireEvent.click(await screen.findByText('Using browser storage')); // open wv0
    await screen.findByText('Files');
    fireEvent.click(screen.getByTestId('vault-switcher'));
    fireEvent.click(await screen.findByText('Share this vault…'));
    // Honest unavailable copy is shown — no listening ticket is requested.
    expect(await screen.findByText(/Sharing isn’t available for browser vaults/)).toBeTruthy();
    expect(screen.getByText(/Open this vault in the desktop app to share it/)).toBeTruthy();
    expect(screen.queryByText('Generating…')).toBeNull();
    expect(screen.queryByText('Copy')).toBeNull();
    expect(screen.queryByText('Require an access key')).toBeNull();
    expect(setAllowConnections).not.toHaveBeenCalled();
    // Modal still closes on Escape.
    fireEvent.keyDown(screen.getByText('Done'), { key: 'Escape' });
    await waitFor(() => expect(screen.queryByText('Share this vault')).toBeNull());
  });

  it('opens a tab and reflects the active file in the hash (web)', async () => {
    render(<App />);
    fireEvent.click(await screen.findByText('Using browser storage')); // open wv0
    await screen.findByText('Files');
    await screen.findByTestId('tab-bar');
    expect(screen.getByTestId('tab').getAttribute('data-path')).toBe('README.md');
    await waitFor(() => expect(parseHash(window.location.hash)).toEqual({ vaultId: 'wv0', path: 'README.md' }));
  });
});
import { afterAll as __aa, mock as __mk } from 'bun:test';
__aa(() => __mk.restore());
