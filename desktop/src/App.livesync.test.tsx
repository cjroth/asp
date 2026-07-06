import { mock } from 'bun:test';
// Live-sync UI test: when a REMOTE peer pushes changes into the vault while the
// user is sitting in the editor, the desktop backend converges (the engine's
// standing connector materializes the edit) and emits a realtime `vault-changed`
// Tauri event — does the UI reflect it? (No polling: the event IS the trigger.)
//
// We simulate a remote push by mutating the mock backend's CONTENT directly
// (exactly what the engine does when a peer's WireRow lands) and then emitting
// `vault-changed` for that vault, exactly as the native engine's change listener
// does. The file tree and the open editor should both catch up.
import { mock } from 'bun:test';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from './test-shim';

// Capture the app's Tauri event subscriptions so a test can fire them (overrides
// the global no-op `listen` stub from the test preload).
const eventHandlers: Record<string, Array<(e: { payload: unknown }) => void>> = {};
mock.module('@tauri-apps/api/event', () => ({
  listen: async (name: string, handler: (e: { payload: unknown }) => void) => {
    (eventHandlers[name] ||= []).push(handler);
    return () => {
      eventHandlers[name] = (eventHandlers[name] || []).filter((h) => h !== handler);
    };
  },
}));
const emit = (name: string, payload: unknown) => {
  for (const h of eventHandlers[name] || []) h({ payload });
};

let CONTENT: Record<string, string> = {};
function reset() {
  CONTENT = { 'README.md': '# Vault\n\nhello\n', 'note.md': '# Note\n\noriginal body\n' };
}
const tick = (ms = 2) => new Promise((r) => setTimeout(r, ms));

const listFiles = vi.fn(async () => {
  await tick();
  return Object.keys(CONTENT).map((p) => ({ path: p, file_id: p, is_dir: false, merge_class: 'text' }));
});
const readFile = vi.fn(async (_id: string, p: string) => { await tick(); return CONTENT[p] ?? ''; });
const writeFile = vi.fn(async (_id: string, p: string, c: string) => { await tick(); CONTENT[p] = c; });
const getStatus = vi.fn(async (id: string) => ({
  id, vault_id: 'vid', rows: Object.keys(CONTENT).length, files: Object.keys(CONTENT).length,
  head: 'h', listening_ticket: 't', peers: ['peerA'], last_ts: 1_700_000_000,
}));
const listVaults = vi.fn(async () => [{ id: 'v1', path: '/home/me/mynotes', vault_id: 'vid', enabled: true, listening_ticket: 't' }]);
const history = vi.fn(async () => { await tick(); return [{ id: 'r', ts: 1_700_000_000, lamport: 1, kind: 'create', path: 'README.md' }]; });

mock.module('@tauri-apps/plugin-dialog', () => ({ open: vi.fn(async () => '/home/me/mynotes') }));
mock.module('./lib/api', () => ({
  api: {
    gitStatus: vi.fn(async () => null), gitPull: vi.fn(async () => {}), gitPendingDiff: vi.fn(async () => ({ filesChanged: 0, paths: [], unified: '' })), gitPush: vi.fn(async () => ({ pushedSha: null, commits: 0 })), cloneGit: vi.fn(),
    startLiveSync: vi.fn(), stopLiveSync: vi.fn(), setLocalRelay: vi.fn(async () => false), getLocalRelay: vi.fn(async () => false),
    listVaults: () => listVaults(),
    addLocalFolder: vi.fn(),
    cloneRemote: vi.fn(),
    setAllowConnections: vi.fn(async () => 't'),
    getStatus: (id: string) => getStatus(id),
    getIdentity: vi.fn(async () => 'ssh-ed25519 AAAA me@host'),
    listFiles: (id: string) => listFiles(id),
    readFile: (id: string, p: string) => readFile(id, p),
    writeFile: (id: string, p: string, c: string) => writeFile(id, p, c),
    renameFile: vi.fn(),
    createDir: vi.fn(async () => {}),
    deleteFile: vi.fn(),
    history: (id: string) => history(id),
    readFileAt: vi.fn(async () => ({ exists: true, content: 'old' })),
    restoreFileAt: vi.fn(),
    removeVault: vi.fn(),
  },
}));

import App from './App';

afterEach(cleanup);
beforeEach(() => { vi.clearAllMocks(); reset(); });

async function openVault() {
  render(<App />);
  const row = await screen.findByText('mynotes');
  fireEvent.click(row);
  await screen.findByText('Files');
  await waitFor(() => expect(listFiles).toHaveBeenCalledWith('v1'));
}

const treeHas = (name: string) =>
  Array.from(document.querySelectorAll('.asp-hover-row')).some((r) => (r.textContent || '').includes(name));

describe('live-sync: UI reflects remote peer pushes', () => {
  it('a file pushed by a remote peer appears in the tree without a manual refresh', async () => {
    await openVault();
    expect(treeHas('from-peer.md')).toBe(false);

    // Remote peer pushes a brand-new file (the engine materialized it; no local action).
    CONTENT['from-peer.md'] = '# Pushed by a peer\n\nhi from another vault\n';

    // The engine emits `vault-changed` → the UI refreshes the tree in realtime.
    emit('vault-changed', 'vid');
    await waitFor(() => expect(treeHas('from-peer.md')).toBe(true), { timeout: 5000 });
  }, 15000);

  it('a remote edit to the OPEN file updates the editor without a manual refresh', async () => {
    await openVault();
    // Open note.md so it's the active file in the editor.
    fireEvent.click(await screen.findByText('note.md'));
    const editor = await screen.findByTestId('live-editor');
    await waitFor(() => expect(editor.textContent || '').toContain('original body'));

    // A remote peer edits the very file we're viewing (no local action, not dirty).
    CONTENT['note.md'] = '# Note\n\nEDITED BY A PEER over the wire\n';

    // The engine emits `vault-changed` → the open file refreshes in realtime.
    emit('vault-changed', 'vid');
    await waitFor(() => expect(editor.textContent || '').toContain('EDITED BY A PEER'), { timeout: 5000 });
  }, 15000);
});
import { afterAll as __aa, mock as __mk } from 'bun:test';
__aa(() => __mk.restore());
