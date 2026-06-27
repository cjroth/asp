// Scale test: drive the real <App/> against a mocked backend holding ~1000 files
// with realistic async latency, to reproduce/lock the large-vault bugs:
// virtualization (bounded DOM rows), the create race ("adds every other time"),
// and delete actually removing the row.
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const N = 1000;
// Flat vault (worst case for the tree): N files at the root.
function makeFiles() {
  const m: Record<string, string> = { 'README.md': '# Massive\n\nbody\n' };
  for (let i = 0; i < N; i++) m[`note-${String(i).padStart(5, '0')}.md`] = `# Note ${i}\n\nbody ${i}\n`;
  return m;
}
let CONTENT = makeFiles();
const tick = () => new Promise((r) => setTimeout(r, 5)); // simulate IPC latency

const writeFile = vi.fn(async (_id: string, path: string, content: string) => { await tick(); CONTENT[path] = content; });
const deleteFile = vi.fn(async (_id: string, path: string) => { await tick(); delete CONTENT[path]; });
const listFiles = vi.fn(async () => {
  await tick();
  return Object.keys(CONTENT).map((p) => ({ path: p, file_id: p, is_dir: false, merge_class: 'text' }));
});
const readFile = vi.fn(async (_id: string, p: string) => { await tick(); return CONTENT[p] ?? ''; });
const history = vi.fn(async () => { await tick(); return [{ id: 'r', ts: 1_700_000_000, lamport: 1, kind: 'create', path: 'README.md' }]; });
const getStatus = vi.fn(async (id: string) => ({ id, vault_id: 'vid', rows: N, files: N, head: 'h', listening_ticket: null, peers: [], last_ts: 1_700_000_000 }));
const listVaults = vi.fn(async () => [{ id: 'v1', path: '/home/me/massive', vault_id: 'vid', enabled: false, listening_ticket: null }]);

vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn(async () => '/home/me/massive') }));
vi.mock('./lib/api', () => ({
  api: {
    listVaults: () => listVaults(),
    addLocalFolder: vi.fn(async (p: string) => ({ id: 'v1', path: p, vault_id: 'vid', enabled: false, listening_ticket: null })),
    cloneRemote: vi.fn(),
    setAllowConnections: vi.fn(async () => 'tkt'),
    getStatus: (id: string) => getStatus(id),
    getIdentity: vi.fn(async () => 'ssh-ed25519 AAAA me@host'),
    listFiles: (id: string) => listFiles(id),
    readFile: (id: string, p: string) => readFile(id, p),
    writeFile: (id: string, p: string, c: string) => writeFile(id, p, c),
    renameFile: vi.fn(async () => {}),
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

describe('App at scale (~1000 files)', () => {
  it('virtualizes the file tree (bounded DOM rows despite 1000 files)', async () => {
    await openMassiveVault();
    await waitFor(() => expect(document.querySelectorAll('.asp-hover-row').length).toBeGreaterThan(0));
    const rowCount = document.querySelectorAll('.asp-hover-row').length;
    // Only viewport rows render — must be far below N.
    expect(rowCount).toBeLessThan(80);
  });

  it('creates a file on every click — no collisions on rapid double-click', async () => {
    await openMassiveVault();
    const plus = document.querySelector('button[title="New note"]') as HTMLElement;
    // Two rapid clicks (the "every other time" repro).
    fireEvent.click(plus);
    fireEvent.click(plus);
    // Both distinct files must be written — not the same name twice.
    await waitFor(() => {
      expect(writeFile).toHaveBeenCalledWith('v1', 'untitled.md', expect.any(String));
      expect(writeFile).toHaveBeenCalledWith('v1', 'untitled-1.md', expect.any(String));
    });
    expect(CONTENT['untitled.md']).toBeDefined();
    expect(CONTENT['untitled-1.md']).toBeDefined();
  });

  it('shows a newly created file in the tree immediately (optimistic)', async () => {
    await openMassiveVault();
    fireEvent.click(document.querySelector('button[title="New note"]') as HTMLElement);
    // It becomes the selected file → appears in the breadcrumb AND is scrolled
    // into view in the tree (so >= 2 matches), not silently off-screen.
    await waitFor(() => expect(screen.getAllByText('untitled.md').length).toBeGreaterThanOrEqual(2));
  });

  it('deletes a file and removes it from the tree', async () => {
    await openMassiveVault();
    // Create one we can target by name, then delete it via the context menu.
    fireEvent.click(document.querySelector('button[title="New note"]') as HTMLElement);
    await waitFor(() => expect(screen.getAllByText('untitled.md').length).toBeGreaterThanOrEqual(2));
    // Find the untitled.md row in the tree and right-click it.
    const rows = Array.from(document.querySelectorAll('.asp-hover-row')) as HTMLElement[];
    const row = rows.find((r) => r.textContent?.includes('untitled.md'));
    expect(row).toBeTruthy();
    fireEvent.contextMenu(row!);
    fireEvent.click(await screen.findByText('Delete'));
    await waitFor(() => expect(deleteFile).toHaveBeenCalledWith('v1', 'untitled.md'));
    // Gone from CONTENT and from the rendered tree.
    await waitFor(() => {
      const present = Array.from(document.querySelectorAll('.asp-hover-row')).some((r) => r.textContent?.includes('untitled.md'));
      expect(present).toBe(false);
    });
  });
});
