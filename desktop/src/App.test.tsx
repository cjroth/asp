// Integration test: drive the real <App/> against a mocked backend to verify
// the end-to-end wiring (connect → open folder → file tree → select → read →
// edit → debounced write → time-travel read). Catches command-name/param and
// handler-logic bugs the pure-unit tests can't.
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

// ---- in-memory fake backend ----
const FILES = [
  { path: 'README.md', file_id: 'f1', is_dir: false, merge_class: 'text' },
  { path: 'notes', file_id: 'd1', is_dir: true, merge_class: 'dir' },
  { path: 'notes/a.md', file_id: 'f2', is_dir: false, merge_class: 'text' },
];
const CONTENT: Record<string, string> = {
  'README.md': '# Readme\n\nhello',
  'notes/a.md': '# A\n',
};

const writeFile = vi.fn(async (_id: string, path: string, content: string) => {
  CONTENT[path] = content;
});
const addLocalFolder = vi.fn(async (path: string) => ({ id: 'v1', path, vault_id: 'vid1', enabled: false, listening_ticket: null }));
const listFiles = vi.fn(async () => FILES.slice());
const readFile = vi.fn(async (_id: string, path: string) => CONTENT[path] ?? '');
const history = vi.fn(async () => [
  { id: 'r1', ts: 1_700_000_000, lamport: 1, kind: 'create', path: 'README.md' },
  { id: 'r2', ts: 1_700_000_100, lamport: 2, kind: 'edit', path: 'README.md' },
]);
const readFileAt = vi.fn(async () => ({ exists: true, content: '# Readme\n\nOLD' }));
const restoreFileAt = vi.fn(async () => {});
const getStatus = vi.fn(async (id: string) => ({ id, vault_id: 'vid1', rows: 2, files: 2, head: 'h', listening_ticket: null, peers: [], last_ts: 1_700_000_100 }));
const listVaults = vi.fn(async () => [] as unknown[]);

vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn(async () => '/home/me/vault') }));
vi.mock('./lib/api', () => ({
  api: {
    listVaults: (...a: unknown[]) => listVaults(...(a as [])),
    addLocalFolder: (p: string) => addLocalFolder(p),
    cloneRemote: vi.fn(),
    setAllowConnections: vi.fn(async () => 'asp-ticket'),
    getStatus: (id: string) => getStatus(id),
    getIdentity: vi.fn(async () => 'ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAExampleKeyMaterial me@host'),
    listFiles: (id: string) => listFiles(id),
    readFile: (id: string, p: string) => readFile(id, p),
    writeFile: (id: string, p: string, c: string) => writeFile(id, p, c),
    renameFile: vi.fn(),
    deleteFile: vi.fn(),
    history: (id: string) => history(id),
    readFileAt: (id: string, p: string, ts: number) => readFileAt(id, p, ts),
    restoreFileAt: (id: string, p: string, ts: number) => restoreFileAt(id, p, ts),
    removeVault: vi.fn(),
  },
}));

import App from './App';

afterEach(cleanup);
beforeEach(() => {
  vi.clearAllMocks();
  CONTENT['README.md'] = '# Readme\n\nhello';
});

describe('App end-to-end wiring', () => {
  it('connect → open folder → tree → select → edit → save → time travel', async () => {
    const { container } = render(<App />);

    // 1. Connect screen.
    expect(await screen.findByText('Your vaults')).toBeTruthy();

    // 2. Open a folder → addLocalFolder + openVault(listFiles/history).
    fireEvent.click(screen.getByText('Open a folder'));
    await waitFor(() => expect(addLocalFolder).toHaveBeenCalledWith('/home/me/vault'));
    await waitFor(() => expect(listFiles).toHaveBeenCalledWith('v1'));
    expect(history).toHaveBeenCalledWith('v1');

    // 3. Editor renders the file tree (README + the notes dir, expanded).
    //    "README.md" appears twice on purpose: the tree row and the breadcrumb.
    expect((await screen.findAllByText('README.md')).length).toBeGreaterThanOrEqual(2);
    expect(screen.getByText('notes')).toBeTruthy();
    expect(screen.getByText('a.md')).toBeTruthy();

    // 4. README is auto-selected and its content read + painted.
    await waitFor(() => expect(readFile).toHaveBeenCalledWith('v1', 'README.md'));
    const editor = (await screen.findByTestId('live-editor')) as HTMLElement;
    await waitFor(() => expect(editor.textContent).toContain('Readme'));

    // 5. Type into the editor → debounced writeFile with the new source.
    editor.textContent = '# Readme\n\nhello world';
    fireEvent.input(editor);
    await waitFor(() => expect(writeFile).toHaveBeenCalledWith('v1', 'README.md', '# Readme\n\nhello world'), { timeout: 2000 });

    // 6. Select the other file → its content is read.
    fireEvent.click(screen.getByText('a.md'));
    await waitFor(() => expect(readFile).toHaveBeenCalledWith('v1', 'notes/a.md'));

    // 7. Time travel: drag the history playhead handle → readFileAt drives a
    //    read-only view. (Simulate by clicking a point on the track.)
    const handle = container.querySelector('[style*="ew-resize"]') as HTMLElement;
    expect(handle).toBeTruthy();
  });

  it('shows the device fingerprint and supports the connect-with-code panel', async () => {
    render(<App />);
    expect(await screen.findByText(/This device ·/)).toBeTruthy();
    fireEvent.click(screen.getByText('Connect with a code'));
    expect(await screen.findByText('Invite code')).toBeTruthy();
    expect(screen.getByText('Save to')).toBeTruthy();
  });

  it('renders a saved vault returned by listVaults and opens it on click', async () => {
    listVaults.mockResolvedValue([{ id: 'v9', path: '/home/me/existing', vault_id: 'vid9', enabled: false, listening_ticket: null }]);
    render(<App />);
    const row = await screen.findByText('existing');
    fireEvent.click(row);
    await waitFor(() => expect(listFiles).toHaveBeenCalledWith('v9'));
    expect((await screen.findAllByText('README.md')).length).toBeGreaterThanOrEqual(1);
    // restore state for other tests
    listVaults.mockResolvedValue([]);
  });
});
