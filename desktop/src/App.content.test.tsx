// Editor content-integrity tests on a SMALL flat vault (every row stably
// rendered — no virtualization/scroll fragility), with a deliberately SLOW
// backend. These reproduce the "random stuff / stale content" class that the
// happy-path harness missed: the editor must reflect the in-memory working copy,
// never a stale/empty backend read while a write is still draining.
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

let CONTENT: Record<string, string>;
const reset = () => { CONTENT = { 'a.md': '# A\n\naaa', 'b.md': '# B\n\nbbb', 'c.md': '# C\n\nccc' }; };
reset();
const slow = (ms: number) => new Promise((r) => setTimeout(r, ms));
// Writes are SLOW (like the debug build's O(N) materialize); reads lag writes.
const writeFile = vi.fn(async (_id: string, p: string, c: string) => { await slow(120); CONTENT[p] = c; });
const readFile = vi.fn(async (_id: string, p: string) => { await slow(10); return CONTENT[p] ?? ''; });

vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn(async () => '/v') }));
vi.mock('./lib/api', () => ({
  api: {
    listVaults: async () => [{ id: 'v1', path: '/v', vault_id: 'vid', enabled: false, listening_ticket: null }],
    getStatus: async (id: string) => ({ id, vault_id: 'vid', rows: 3, files: 3, head: 'h', listening_ticket: null, peers: [], last_ts: 1 }),
    getIdentity: async () => 'k',
    listFiles: async () => Object.keys(CONTENT).map((p) => ({ path: p, file_id: p, is_dir: false, merge_class: 'text' })),
    readFile: (id: string, p: string) => readFile(id, p),
    writeFile: (id: string, p: string, c: string) => writeFile(id, p, c),
    deleteFile: async (_id: string, p: string) => { delete CONTENT[p]; },
    renameFile: async () => {},
    history: async () => [],
    readFileAt: async (_id: string, p: string) => ({ exists: true, content: CONTENT[p] ?? '' }),
    restoreFileAt: async () => {},
    addLocalFolder: async () => ({ id: 'v1', path: '/v', vault_id: 'vid', enabled: false, listening_ticket: null }),
    removeVault: async () => {},
  },
}));

import App from './App';

afterEach(cleanup);
beforeEach(() => { vi.clearAllMocks(); reset(); });

async function openVault() {
  render(<App />);
  fireEvent.click(await screen.findByText('v'));
  await screen.findByText('Files');
  return screen.findByTestId('live-editor');
}
const row = (name: string) => Array.from(document.querySelectorAll('.asp-hover-row')).find((r) => (r.textContent || '') === name) as HTMLElement;

describe('editor content integrity (small vault, slow backend)', () => {
  it('a newly created file shows its template, not an empty backend read', async () => {
    const editor = await openVault();
    fireEvent.click(document.querySelector('button[title="New note"]') as HTMLElement);
    await waitFor(() => expect(editor.textContent || '').toContain('untitled'));
  });

  it('edit → switch away → switch back keeps the unsaved edit (no stale re-read)', async () => {
    const editor = await openVault();
    fireEvent.click(row('a.md'));
    await waitFor(() => expect(editor.textContent || '').toContain('A'));
    editor.textContent = '# A edited\n\nKEEPME';
    fireEvent.input(editor);
    fireEvent.click(row('b.md'));
    await waitFor(() => expect(editor.textContent || '').toContain('B'));
    expect(editor.textContent || '').not.toContain('KEEPME');
    fireEvent.click(row('a.md'));
    await waitFor(() => expect(editor.textContent || '').toContain('KEEPME'));
  });

  it('selecting a file always shows THAT file (no flip to a previously-opened one)', async () => {
    const editor = await openVault();
    fireEvent.click(row('a.md'));
    await waitFor(() => expect(editor.textContent || '').toContain('aaa'));
    fireEvent.click(row('c.md'));
    await waitFor(() => expect(editor.textContent || '').toContain('ccc'));
    // Rapidly bounce; the editor must end on the last-clicked file's content.
    fireEvent.click(row('b.md'));
    fireEvent.click(row('a.md'));
    fireEvent.click(row('c.md'));
    await waitFor(() => expect(editor.textContent || '').toContain('ccc'));
    expect(editor.textContent || '').not.toContain('aaa');
    expect(editor.textContent || '').not.toContain('bbb');
  });
});
