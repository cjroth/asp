import { mock } from 'bun:test';
// Opening-progress overlay: a slow open (large folder / slow list_files) must
// show a non-dismissable "working…" overlay so the app doesn't look frozen,
// and it must clear once the vault is open. A fast open must NOT flash it.
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from './test-shim';

let LIST_DELAY = 300; // make list_files slow enough to cross the overlay threshold
const tick = (ms: number) => new Promise((r) => setTimeout(r, ms));

const listFiles = vi.fn(async () => {
  await tick(LIST_DELAY);
  return [{ path: 'README.md', file_id: 'README.md', is_dir: false, merge_class: 'text' }];
});
const addLocalFolder = vi.fn(async (p: string) => ({ id: 'v1', path: p, vault_id: 'vid', enabled: false, listening_ticket: null }));
const listVaults = vi.fn(async () => [{ id: 'v1', path: '/home/me/big', vault_id: 'vid', enabled: false, listening_ticket: null }]);

mock.module('@tauri-apps/plugin-dialog', () => ({ open: vi.fn(async () => '/home/me/big') }));
mock.module('./lib/api', () => ({
  api: {
    listVaults: () => listVaults(),
    addLocalFolder: (p: string) => addLocalFolder(p),
    cloneRemote: vi.fn(),
    setAllowConnections: vi.fn(async () => 'tkt'),
    getStatus: vi.fn(async (id: string) => ({ id, vault_id: 'vid', rows: 1, files: 1, head: 'h', listening_ticket: null, peers: [], last_ts: 1 })),
    getIdentity: vi.fn(async () => 'ssh-ed25519 AAAA me@host'),
    listFiles: () => listFiles(),
    readFile: vi.fn(async () => '# README\n'),
    writeFile: vi.fn(async () => {}),
    renameFile: vi.fn(async () => {}),
    createDir: vi.fn(async () => {}),
    deleteFile: vi.fn(async () => {}),
    history: vi.fn(async () => []),
    readFileAt: vi.fn(async () => ({ exists: true, content: 'old' })),
    restoreFileAt: vi.fn(async () => {}),
    removeVault: vi.fn(),
  },
}));

import App from './App';

afterEach(cleanup);
beforeEach(() => {
  vi.clearAllMocks();
  LIST_DELAY = 300;
});

describe('open-progress overlay', () => {
  it('shows the overlay during a slow open, then clears it', async () => {
    render(<App />);
    fireEvent.click(await screen.findByText('big'));
    // The slow list_files (300ms) crosses the 140ms threshold → overlay appears.
    const overlay = await screen.findByTestId('opening-overlay');
    expect(overlay.textContent || '').toContain('Opening');
    // Once the open finishes, the editor is shown and the overlay is gone.
    await screen.findByText('Files');
    await waitFor(() => expect(screen.queryByTestId('opening-overlay')).toBeNull());
  }, 10000);

  it('does NOT flash the overlay on a fast open', async () => {
    LIST_DELAY = 5; // well under the threshold
    render(<App />);
    fireEvent.click(await screen.findByText('big'));
    await screen.findByText('Files');
    // The overlay must never have appeared for a sub-threshold open.
    expect(screen.queryByTestId('opening-overlay')).toBeNull();
  }, 10000);
});

import { afterAll as __aa, mock as __mk } from 'bun:test';
__aa(() => __mk.restore());
