import { mock } from 'bun:test';
// Scale test: drive the real <App/> against a mocked backend holding ~1000 files
// with realistic async latency, to reproduce/lock the large-vault bugs:
// virtualization (bounded DOM rows), the create race ("adds every other time"),
// and delete actually removing the row.
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from './test-shim';

const N = 1000;
// Flat vault (worst case for the tree): N files at the root.
function makeFiles() {
  const m: Record<string, string> = { 'README.md': '# Massive\n\nbody\n' };
  for (let i = 0; i < N; i++) m[`note-${String(i).padStart(5, '0')}.md`] = `# Note ${i}\n\nbody ${i}\n`;
  return m;
}
let CONTENT = makeFiles();
const tick = (ms = 5) => new Promise((r) => setTimeout(r, ms)); // simulate IPC latency

// Mutations are slow (like the real O(N) materialize) to expose races.
const writeFile = vi.fn(async (_id: string, path: string, content: string) => { await tick(40); CONTENT[path] = content; });
const deleteFile = vi.fn(async (_id: string, path: string) => { await tick(40); delete CONTENT[path]; });
const renameFile = vi.fn(async (_id: string, oldP: string, newP: string) => { await tick(40); CONTENT[newP] = CONTENT[oldP]; delete CONTENT[oldP]; });
const listFiles = vi.fn(async () => {
  await tick();
  return Object.keys(CONTENT).map((p) => ({ path: p, file_id: p, is_dir: false, merge_class: 'text' }));
});
const readFile = vi.fn(async (_id: string, p: string) => { await tick(); return CONTENT[p] ?? ''; });
const history = vi.fn(async () => { await tick(); return [{ id: 'r', ts: 1_700_000_000, lamport: 1, kind: 'create', path: 'README.md' }]; });
const getStatus = vi.fn(async (id: string) => ({ id, vault_id: 'vid', rows: N, files: N, head: 'h', listening_ticket: null, peers: [], last_ts: 1_700_000_000 }));
const listVaults = vi.fn(async () => [{ id: 'v1', path: '/home/me/massive', vault_id: 'vid', enabled: false, listening_ticket: null }]);

mock.module('@tauri-apps/plugin-dialog', () => ({ open: vi.fn(async () => '/home/me/massive') }));
mock.module('./lib/api', () => ({
  api: {
    startLiveSync: vi.fn(), stopLiveSync: vi.fn(),
    listVaults: () => listVaults(),
    addLocalFolder: vi.fn(async (p: string) => ({ id: 'v1', path: p, vault_id: 'vid', enabled: false, listening_ticket: null })),
    cloneRemote: vi.fn(),
    setAllowConnections: vi.fn(async () => 'tkt'),
    getStatus: (id: string) => getStatus(id),
    getIdentity: vi.fn(async () => 'ssh-ed25519 AAAA me@host'),
    listFiles: (id: string) => listFiles(id),
    readFile: (id: string, p: string) => readFile(id, p),
    writeFile: (id: string, p: string, c: string) => writeFile(id, p, c),
    renameFile: (id: string, o: string, n: string) => renameFile(id, o, n),
    createDir: vi.fn(async () => {}),
    deleteFile: (id: string, p: string) => deleteFile(id, p),
    history: (id: string) => history(id),
    readFileAt: vi.fn(async () => ({ exists: true, content: 'old' })),
    restoreFileAt: vi.fn(async () => {}),
    removeVault: vi.fn(),
  },
}));

import App from './App';

afterEach(cleanup);
beforeEach(() => {
  vi.clearAllMocks();
  CONTENT = makeFiles();
});

async function openMassiveVault() {
  render(<App />);
  const row = await screen.findByText('massive');
  fireEvent.click(row);
  await screen.findByText('Files');
  await waitFor(() => expect(listFiles).toHaveBeenCalledWith('v1'));
}

// The "+" opens a menu in the new design; create a file via New file.
async function clickNewFile() {
  fireEvent.click(document.querySelector('button[title="New note"]') as HTMLElement);
  fireEvent.click(await screen.findByText('New file'));
}

describe('App at scale (~1000 files)', () => {
  it('virtualizes the file tree (bounded DOM rows despite 1000 files)', async () => {
    await openMassiveVault();
    await waitFor(() => expect(document.querySelectorAll('.asp-hover-row').length).toBeGreaterThan(0));
    const rowCount = document.querySelectorAll('.asp-hover-row').length;
    // Only viewport rows render — must be far below N.
    expect(rowCount).toBeLessThan(80);
  });

  it('creates a file each time — no collisions on back-to-back New file', async () => {
    await openMassiveVault();
    // Two New file actions in a row (the synchronous name reservation prevents the
    // "every other time" collision even while the slow backend write is in flight).
    await clickNewFile();
    await clickNewFile();
    await waitFor(() => {
      expect(CONTENT['untitled.md']).toBeDefined();
      expect(CONTENT['untitled-1.md']).toBeDefined();
    });
  });

  it('shows a newly created file as the selection immediately (optimistic)', async () => {
    await openMassiveVault();
    await clickNewFile();
    // It becomes the selected file → appears (breadcrumb + scrolled-to tree row).
    await waitFor(() => expect(screen.getAllByText('untitled.md').length).toBeGreaterThanOrEqual(1));
  });

  // The delete tests target whatever note-* rows are actually rendered (the tree
  // auto-scrolls to README, which sorts last) — robust to scroll position. The
  // real scroll-into-view visual is covered by the WebKit harness (e2e/).
  const renderedNotes = () =>
    (Array.from(document.querySelectorAll('.asp-hover-row')) as HTMLElement[])
      .map((r) => (r.textContent || '').match(/note-\d+\.md/)?.[0])
      .filter((x): x is string => !!x);
  const present = (nm: string) => Array.from(document.querySelectorAll('.asp-hover-row')).some((r) => (r.textContent || '').includes(nm));
  const rowFor = (nm: string) => (Array.from(document.querySelectorAll('.asp-hover-row')) as HTMLElement[]).find((r) => (r.textContent || '').includes(nm))!;

  it('deletes a file and removes it from the tree immediately (optimistic, before the slow backend)', async () => {
    await openMassiveVault();
    await waitFor(() => expect(renderedNotes().length).toBeGreaterThan(0));
    const name = renderedNotes()[0];
    fireEvent.contextMenu(rowFor(name));
    fireEvent.click(await screen.findByText('Delete'));
    fireEvent.click(await screen.findByTestId('confirm-delete'));
    // Synchronously gone from the tree (the 40ms backend hasn't returned yet).
    expect(present(name)).toBe(false);
    await waitFor(() => expect(deleteFile).toHaveBeenCalledWith('v1', name));
  });

  it('deletes several files back-to-back with none surviving (the race that left files stuck)', async () => {
    await openMassiveVault();
    await waitFor(() => expect(renderedNotes().length).toBeGreaterThanOrEqual(4));
    const targets = renderedNotes().slice(0, 4);
    // Fire all deletes in quick succession; each backend call takes 40ms so they
    // overlap — the old read-modify-write-after-await would resurrect some.
    for (const nm of targets) {
      fireEvent.contextMenu(rowFor(nm));
      fireEvent.click(await screen.findByText('Delete'));
      fireEvent.click(await screen.findByTestId('confirm-delete'));
    }
    for (const nm of targets) expect(present(nm)).toBe(false);
    await waitFor(() => targets.forEach((nm) => expect(CONTENT[nm]).toBeUndefined()));
  });

  // ---- editor content correctness (the "random stuff / stale content" class) ----
  // These failed before the in-memory working-copy cache: the resolver re-read the
  // backend on every selection, returning stale/empty content while a write was
  // still draining (worst on the slow debug build).

  it('a newly created file shows its template content, not an empty/stale backend read', async () => {
    await openMassiveVault();
    await clickNewFile();
    const editor = await screen.findByTestId('live-editor');
    // The optimistic template ("# untitled") must show even though the backend
    // write (40ms) hasn't landed when the resolver runs.
    await waitFor(() => expect(editor.textContent || '').toContain('untitled'));
    expect(editor.textContent || '').not.toBe('');
  });

  it('renames a file via the context menu (optimistic, old name leaves the tree)', async () => {
    await openMassiveVault();
    await waitFor(() => expect(renderedNotes().length).toBeGreaterThan(0));
    const name = renderedNotes()[0];
    fireEvent.contextMenu(rowFor(name));
    fireEvent.click(await screen.findByText('Rename'));
    const input = document.querySelector('.asp-hover-row input') as HTMLInputElement;
    expect(input).toBeTruthy();
    fireEvent.change(input, { target: { value: 'aaa-renamed.md' } });
    fireEvent.keyDown(input, { key: 'Enter' });
    // Optimistic: the old name is gone from the tree synchronously.
    expect(present(name)).toBe(false);
    await waitFor(() => expect(renameFile).toHaveBeenCalledWith('v1', name, 'aaa-renamed.md'));
  });
});
import { afterAll as __aa, mock as __mk } from 'bun:test';
__aa(() => __mk.restore());
