import { mock } from 'bun:test';
// Web live-sync: a browser (wasm/OPFS) node can't accept inbound connections, so
// it dials the upstream it cloned from and holds that link OPEN — rows stream
// both ways in realtime, no polling. The app starts that connection when a web
// vault is open (`api.startLiveSync`) and the engine calls back on every remote
// push; the app's callback refreshes the tree + open file. This test drives a
// remote push by invoking that callback (exactly what the live connection does
// when a peer's row lands) and asserts the UI catches up — immediately, not on a
// 10s tick.
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from './test-shim';

let CONTENT: Record<string, string> = {};
function reset() {
  CONTENT = { 'README.md': '# Vault\n\nhello\n', 'note.md': '# Note\n\noriginal body\n' };
}
const tick = (ms = 2) => new Promise((r) => setTimeout(r, ms));

// The app registers an onChange callback via startLiveSync; we keep it so the
// test can fire it to simulate a remote push landing in the engine.
let onRemotePush: (() => void) | null = null;
const startLiveSync = vi.fn(async (_id: string, onChange: () => void) => { onRemotePush = onChange; });
const stopLiveSync = vi.fn(async () => { onRemotePush = null; });

const listFiles = vi.fn(async () => {
  await tick();
  return Object.keys(CONTENT).map((p) => ({ path: p, file_id: p, is_dir: false, merge_class: 'text' }));
});
const readFile = vi.fn(async (_id: string, p: string) => { await tick(); return CONTENT[p] ?? ''; });
const writeFile = vi.fn(async (_id: string, p: string, c: string) => { await tick(); CONTENT[p] = c; });
const getStatus = vi.fn(async (id: string) => ({
  id, vault_id: 'wv', rows: Object.keys(CONTENT).length, files: Object.keys(CONTENT).length,
  head: '', listening_ticket: null, peers: [], last_ts: null,
}));
const listVaults = vi.fn(async () => [{ id: 'w1', path: '', vault_id: 'wv', enabled: true, listening_ticket: null }]);

mock.module('@tauri-apps/plugin-dialog', () => ({ open: vi.fn(async () => null) }));
mock.module('./lib/api', () => ({
  api: {
    listVaults: () => listVaults(),
    addLocalFolder: vi.fn(),
    cloneRemote: vi.fn(),
    createVault: vi.fn(),
    setAllowConnections: vi.fn(async () => null),
    syncNow: vi.fn(async () => {}),
    startLiveSync: (id: string, onChange: () => void) => startLiveSync(id, onChange),
    stopLiveSync: (id: string) => stopLiveSync(id),
    getStatus: (id: string) => getStatus(id),
    getIdentity: vi.fn(async () => 'ssh-ed25519 AAAA me@browser'),
    listFiles: (id: string) => listFiles(id),
    readFile: (id: string, p: string) => readFile(id, p),
    writeFile: (id: string, p: string, c: string) => writeFile(id, p, c),
    renameFile: vi.fn(),
    createDir: vi.fn(async () => {}),
    deleteFile: vi.fn(),
    history: vi.fn(async () => []),
    readFileAt: vi.fn(async () => ({ exists: true, content: 'old' })),
    restoreFileAt: vi.fn(),
    removeVault: vi.fn(),
  },
}));

import App from './App';

const w = window as unknown as Record<string, unknown>;
beforeEach(() => {
  vi.clearAllMocks();
  reset();
  onRemotePush = null;
  // Web platform: no Tauri shell. This is what makes the app use the live link.
  delete w.__TAURI_INTERNALS__;
  delete w.__TAURI__;
});
afterEach(() => { cleanup(); w.__TAURI_INTERNALS__ = {}; });

async function openVault() {
  render(<App />);
  // A browser-storage vault card renders "Using browser storage" instead of a path.
  fireEvent.click(await screen.findByText('Using browser storage'));
  await screen.findByText('Files');
  await waitFor(() => expect(listFiles).toHaveBeenCalledWith('w1'));
}

const treeHas = (name: string) =>
  Array.from(document.querySelectorAll('.asp-hover-row')).some((r) => (r.textContent || '').includes(name));

describe('web live-sync: a held-open connection pushes peer changes into the UI', () => {
  it('opens a live connection for the active web vault', async () => {
    await openVault();
    await waitFor(() => expect(startLiveSync).toHaveBeenCalledWith('w1', expect.any(Function)));
  }, 15000);

  it('a file a peer pushed appears in the tree as soon as the push lands', async () => {
    await openVault();
    expect(treeHas('from-peer.md')).toBe(false);
    // A peer pushes a new file over the live connection: the engine integrates it
    // and calls the app's onChange — simulated here by mutating CONTENT then firing.
    CONTENT['from-peer.md'] = '# Pushed by a peer\n\nhi from the desktop\n';
    onRemotePush?.();
    await waitFor(() => expect(treeHas('from-peer.md')).toBe(true), { timeout: 5000 });
  }, 15000);

  it('a peer edit to the OPEN file updates the editor when the push lands', async () => {
    await openVault();
    fireEvent.click(await screen.findByText('note.md'));
    const editor = await screen.findByTestId('live-editor');
    await waitFor(() => expect(editor.textContent || '').toContain('original body'));
    CONTENT['note.md'] = '# Note\n\nEDITED ON THE DESKTOP over the wire\n';
    onRemotePush?.();
    await waitFor(() => expect(editor.textContent || '').toContain('EDITED ON THE DESKTOP'), { timeout: 5000 });
  }, 15000);

  it('tears the live connection down when the vault view goes away', async () => {
    render(<App />);
    fireEvent.click(await screen.findByText('Using browser storage'));
    await screen.findByText('Files');
    await waitFor(() => expect(startLiveSync).toHaveBeenCalledWith('w1', expect.any(Function)));
    // Unmounting runs the effect cleanup, which must stop the live connection.
    cleanup();
    await waitFor(() => expect(stopLiveSync).toHaveBeenCalledWith('w1'));
  }, 15000);
});
import { afterAll as __aa, mock as __mk } from 'bun:test';
__aa(() => __mk.restore());
