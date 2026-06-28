// Web live-sync: a browser (wasm/OPFS) node can't open a listening socket, so it
// never *receives* a peer's push — the only way it learns of later changes is by
// re-dialing the upstream it cloned from. That pull has to be driven by the
// editor poll (`api.syncNow`); without it the desktop edits a file and the web
// tab shows a stale snapshot forever. This test pushes a peer change into an
// "upstream" map that only `syncNow` drains into the visible CONTENT, then proves
// the web UI catches up on its own — i.e. the poll really does sync.
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

// CONTENT is what the (web) backend currently holds; UPSTREAM is what a peer has
// pushed but the web node hasn't pulled yet. syncNow drains UPSTREAM → CONTENT,
// exactly like dialing the ticket and running bidirectional catch-up.
let CONTENT: Record<string, string> = {};
let UPSTREAM: Record<string, string> = {};
function reset() {
  CONTENT = { 'README.md': '# Vault\n\nhello\n', 'note.md': '# Note\n\noriginal body\n' };
  UPSTREAM = {};
}
const tick = (ms = 2) => new Promise((r) => setTimeout(r, ms));

const listFiles = vi.fn(async () => {
  await tick();
  return Object.keys(CONTENT).map((p) => ({ path: p, file_id: p, is_dir: false, merge_class: 'text' }));
});
const readFile = vi.fn(async (_id: string, p: string) => { await tick(); return CONTENT[p] ?? ''; });
const writeFile = vi.fn(async (_id: string, p: string, c: string) => { await tick(); CONTENT[p] = c; });
// The pull: re-dial upstream and converge. No ticket arg — the web backend falls
// back to the stored upstream, which is the whole point of the fix.
const syncNow = vi.fn(async (_id: string, ticket?: string) => {
  await tick();
  Object.assign(CONTENT, UPSTREAM);
  UPSTREAM = {};
});
const getStatus = vi.fn(async (id: string) => ({
  id, vault_id: 'wv', rows: Object.keys(CONTENT).length, files: Object.keys(CONTENT).length,
  head: '', listening_ticket: null, peers: [], last_ts: null,
}));
const listVaults = vi.fn(async () => [{ id: 'w1', path: '', vault_id: 'wv', enabled: true, listening_ticket: null }]);

vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn(async () => null) }));
vi.mock('./lib/api', () => ({
  api: {
    listVaults: () => listVaults(),
    addLocalFolder: vi.fn(),
    cloneRemote: vi.fn(),
    createVault: vi.fn(),
    setAllowConnections: vi.fn(async () => null),
    syncNow: (id: string, ticket?: string, authKey?: string) => syncNow(id, ticket),
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
  // Web platform: no Tauri shell. This is what makes the poll choose the pull path.
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

describe('web live-sync: the editor poll pulls peer pushes via syncNow', () => {
  it('drives api.syncNow for the active web vault (no ticket → upstream fallback)', async () => {
    await openVault();
    await waitFor(() => expect(syncNow).toHaveBeenCalledWith('w1', undefined), { timeout: 12000 });
  }, 15000);

  it('a file a peer pushed appears in the tree only because the poll syncs', async () => {
    await openVault();
    expect(treeHas('from-peer.md')).toBe(false);
    // A peer (the desktop hub) pushes a new file upstream; the web node hasn't
    // pulled it yet, so it isn't in CONTENT until syncNow drains it.
    UPSTREAM['from-peer.md'] = '# Pushed by a peer\n\nhi from the desktop\n';
    await waitFor(() => expect(treeHas('from-peer.md')).toBe(true), { timeout: 12000 });
  }, 15000);

  it('a peer edit to the OPEN file updates the editor after a poll-driven sync', async () => {
    await openVault();
    fireEvent.click(await screen.findByText('note.md'));
    const editor = await screen.findByTestId('live-editor');
    await waitFor(() => expect(editor.textContent || '').toContain('original body'));
    UPSTREAM['note.md'] = '# Note\n\nEDITED ON THE DESKTOP over the wire\n';
    await waitFor(() => expect(editor.textContent || '').toContain('EDITED ON THE DESKTOP'), { timeout: 12000 });
  }, 15000);
});
