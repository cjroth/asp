import { mock } from 'bun:test';
// Integration test: drive the real <App/> against a mocked backend to verify
// the end-to-end wiring (connect → open folder → file tree → select → read →
// edit → debounced write → time-travel read). Catches command-name/param and
// handler-logic bugs the pure-unit tests can't.
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from './test-shim';
import { buildHash, parseHash } from './vault/tabs';

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
const renameFile = vi.fn(async () => {});
const deleteFile = vi.fn(async () => {});

mock.module('@tauri-apps/plugin-dialog', () => ({ open: vi.fn(async () => '/home/me/vault') }));
mock.module('./lib/api', () => ({
  api: {
    startLiveSync: vi.fn(), stopLiveSync: vi.fn(),
    listVaults: (...a: unknown[]) => listVaults(...(a as [])),
    addLocalFolder: (p: string) => addLocalFolder(p),
    cloneRemote: vi.fn(),
    setAllowConnections: vi.fn(async () => 'asp-ticket'),
    getStatus: (id: string) => getStatus(id),
    getIdentity: vi.fn(async () => 'ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAExampleKeyMaterial me@host'),
    listFiles: (id: string) => listFiles(id),
    readFile: (id: string, p: string) => readFile(id, p),
    writeFile: (id: string, p: string, c: string) => writeFile(id, p, c),
    renameFile: (...a: unknown[]) => renameFile(...(a as [])),
    createDir: vi.fn(),
    deleteFile: (...a: unknown[]) => deleteFile(...(a as [])),
    history: (id: string) => history(id),
    readFileAt: (id: string, p: string, ts: number) => readFileAt(id, p, ts),
    restoreFileAt: (id: string, p: string, ts: number) => restoreFileAt(id, p, ts),
    removeVault: vi.fn(),
  },
}));

import App, { __resetUrlRestore } from './App';

afterEach(cleanup);
beforeEach(() => {
  vi.clearAllMocks();
  CONTENT['README.md'] = '# Readme\n\nhello';
  localStorage.clear();
  __resetUrlRestore();
  window.history.replaceState(null, '', '/');
});

describe('App end-to-end wiring', () => {
  it('connect → open folder → tree → select → edit → save → time travel', async () => {
    const { container } = render(<App />);

    // 1. Connect screen.
    expect(await screen.findByText('Your vaults')).toBeTruthy();

    // 2. New Vault → choose a folder (native dialog) → Create → addLocalFolder + openVault.
    fireEvent.click(screen.getByText('New Vault'));
    fireEvent.click(await screen.findByText('Choose…'));
    await screen.findByText('/home/me/vault');
    fireEvent.click(screen.getByText('Create vault'));
    await waitFor(() => expect(addLocalFolder).toHaveBeenCalledWith('/home/me/vault'));
    await waitFor(() => expect(listFiles).toHaveBeenCalledWith('v1'));
    // history() scans the whole log, so it's now fetched lazily — only when the
    // History/Log view opens (asserted at step 7), never on the editor poll.
    expect(history).not.toHaveBeenCalled();

    // 3. Editor renders the file tree. Dirs start collapsed (vaults can be huge),
    //    so expand "notes" to reveal a.md. "README.md" appears twice on purpose:
    //    the tree row and the breadcrumb.
    expect((await screen.findAllByText('README.md')).length).toBeGreaterThanOrEqual(2);
    fireEvent.click(screen.getByText('notes'));
    expect(await screen.findByText('a.md')).toBeTruthy();

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

    // 7. Expand the History tab → the time-travel track + playhead handle render.
    fireEvent.click(screen.getByText('History'));
    await waitFor(() => expect(history).toHaveBeenCalledWith('v1')); // fetched on open
    const track = await screen.findByTestId('history-track');
    expect(track).toBeTruthy();
    const handle = container.querySelector('[style*="ew-resize"]') as HTMLElement;
    expect(handle).toBeTruthy();
  });

  it('shows the device fingerprint and supports the connect-with-code modal', async () => {
    render(<App />);
    expect(await screen.findByText(/This device ·/)).toBeTruthy();
    fireEvent.click(screen.getByText('Connect Vault'));
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

  it('seeds a welcome README.md into a brand-new EMPTY vault', async () => {
    // The post-create seed check sees an empty vault; openVault's later
    // listFiles falls back to the default non-empty FILES list.
    listFiles.mockResolvedValueOnce([]);
    render(<App />);
    expect(await screen.findByText('Your vaults')).toBeTruthy();
    fireEvent.click(screen.getByText('New Vault'));
    fireEvent.click(await screen.findByText('Choose…'));
    await screen.findByText('/home/me/vault');
    fireEvent.click(screen.getByText('Create vault'));
    await waitFor(() => expect(addLocalFolder).toHaveBeenCalledWith('/home/me/vault'));
    await waitFor(() => {
      const call = writeFile.mock.calls.find((c) => c[1] === 'README.md');
      expect(call).toBeTruthy();
      const content = call![2];
      expect(content).toContain('---'); // YAML frontmatter
      expect(content).toContain('```mermaid'); // mermaid diagram
      expect(content).toContain('|'); // table
      expect(content).toContain('- [ ]'); // unchecked task
      expect(content).toContain('- [x]'); // checked task
      expect(content).toMatch(/^> /m); // blockquote
      expect(content).toMatch(/```(tsx|bash)/); // highlighted code fence
    });
  });

  it('does NOT seed a vault that already contains files', async () => {
    // Default listFiles returns a non-empty FILES list → no README is written.
    render(<App />);
    expect(await screen.findByText('Your vaults')).toBeTruthy();
    fireEvent.click(screen.getByText('New Vault'));
    fireEvent.click(await screen.findByText('Choose…'));
    await screen.findByText('/home/me/vault');
    fireEvent.click(screen.getByText('Create vault'));
    await waitFor(() => expect(addLocalFolder).toHaveBeenCalledWith('/home/me/vault'));
    await waitFor(() => expect(listFiles).toHaveBeenCalledWith('v1'));
    expect(writeFile.mock.calls.some((c) => c[1] === 'README.md')).toBe(false);
  });
});

// The vault-switcher dropdown must keep the action items (esp. "New vault…" /
// "Open another folder…") reachable no matter how many vaults exist: the vault
// LIST scrolls, the actions stay pinned outside that scroll region.
describe('App — vault-switcher list scrolls with actions pinned', () => {
  const manyVaults = Array.from({ length: 40 }, (_, i) => ({
    id: `v${i}`,
    path: `/home/me/vault-${i}`,
    vault_id: `vid${i}`,
    enabled: false,
    listening_ticket: null,
  }));
  beforeEach(() => listVaults.mockResolvedValue(manyVaults));
  afterEach(() => listVaults.mockResolvedValue([]));

  it('makes the vault list scrollable while pinning the New vault / Open folder action', async () => {
    render(<App />);
    // Open the first vault so the sidebar (with the switcher) is shown.
    fireEvent.click(await screen.findByText('vault-0'));
    fireEvent.click(await screen.findByTestId('vault-switcher'));
    await screen.findByText('Switch vault');

    // (1) The vault-list container is its own bounded, scrollable region.
    const list = screen.getByTestId('vault-list');
    expect(list.style.overflowY).toBe('auto');
    expect(list.style.maxHeight).toBeTruthy();
    expect(list.style.maxHeight).not.toBe('none');

    // (2) The pinned action is present and NOT inside the scrolling list.
    const action = screen.getByText('Open another folder…');
    expect(action).toBeTruthy();
    expect(list.contains(action)).toBe(false);
  });
});

// Tabs + active-file-in-URL (DESKTOP platform — __TAURI_INTERNALS__ is set by
// test-setup). The web counterpart lives in App.web.test.tsx.
describe('App — tabs + URL hash (desktop)', () => {
  const twoVaults = [
    { id: 'v1', path: '/home/me/alpha', vault_id: 'vidA', enabled: false, listening_ticket: null },
    { id: 'v2', path: '/home/me/beta', vault_id: 'vidB', enabled: false, listening_ticket: null },
  ];
  beforeEach(() => listVaults.mockResolvedValue(twoVaults));
  afterEach(() => listVaults.mockResolvedValue([]));

  const tabPaths = () => screen.queryAllByTestId('tab').map((t) => t.getAttribute('data-path'));
  const tab = (p: string) => screen.getAllByTestId('tab').find((t) => t.getAttribute('data-path') === p)!;
  const treeRow = (name: string) => (Array.from(document.querySelectorAll('.asp-hover-row')) as HTMLElement[]).find((r) => r.textContent === name)!;

  const openAlpha = async () => {
    render(<App />);
    fireEvent.click(await screen.findByText('alpha'));
    await screen.findByTestId('live-editor');
  };

  it('opens a tab when a file is selected and adds another on opening a second file', async () => {
    await openAlpha();
    await waitFor(() => expect(tabPaths()).toEqual(['README.md'])); // README auto-opened
    fireEvent.click(await screen.findByText('notes')); // expand
    fireEvent.click(await screen.findByText('a.md'));
    await waitFor(() => expect(tabPaths()).toEqual(['README.md', 'notes/a.md']));
    // The active file is mirrored in the URL hash (vault_id + path).
    await waitFor(() => expect(parseHash(window.location.hash)).toEqual({ vaultId: 'vidA', path: 'notes/a.md' }));
  });

  it('clicking a tab switches the active file and updates the hash', async () => {
    await openAlpha();
    fireEvent.click(await screen.findByText('notes'));
    fireEvent.click(await screen.findByText('a.md'));
    await waitFor(() => expect(parseHash(window.location.hash)?.path).toBe('notes/a.md'));
    fireEvent.click(tab('README.md'));
    await waitFor(() => expect(parseHash(window.location.hash)).toEqual({ vaultId: 'vidA', path: 'README.md' }));
    expect(tab('README.md').getAttribute('aria-selected')).toBe('true');
  });

  it('restores the vault + file from the URL hash on mount (refresh)', async () => {
    window.history.replaceState(null, '', buildHash('vidA', 'notes/a.md'));
    render(<App />); // no click — restoration must open it
    const editor = await screen.findByTestId('live-editor');
    await waitFor(() => expect(editor.textContent).toContain('A'));
    // The restored file is the active tab.
    await waitFor(() => expect(tab('notes/a.md').getAttribute('aria-selected')).toBe('true'));
    expect(parseHash(window.location.hash)).toEqual({ vaultId: 'vidA', path: 'notes/a.md' });
  });

  it('falls back gracefully when the hash points at a now-missing file', async () => {
    window.history.replaceState(null, '', buildHash('vidA', 'ghost-deleted.md'));
    render(<App />);
    const editor = await screen.findByTestId('live-editor');
    // Vault still opens; selection falls back to the default (README).
    await waitFor(() => expect(editor.textContent).toContain('Readme'));
    await waitFor(() => expect(parseHash(window.location.hash)).toEqual({ vaultId: 'vidA', path: 'README.md' }));
  });

  it('closing the active tab selects a neighbor tab', async () => {
    await openAlpha();
    fireEvent.click(await screen.findByText('notes'));
    fireEvent.click(await screen.findByText('a.md')); // tabs: [README, notes/a.md], active a.md
    await waitFor(() => expect(tabPaths()).toEqual(['README.md', 'notes/a.md']));
    fireEvent.click(within(tab('notes/a.md')).getByTestId('tab-close'));
    await waitFor(() => expect(tabPaths()).toEqual(['README.md']));
    expect(tab('README.md').getAttribute('aria-selected')).toBe('true');
    await waitFor(() => expect(parseHash(window.location.hash)?.path).toBe('README.md'));
  });

  it('closing the last tab clears the selection and blanks the hash', async () => {
    await openAlpha();
    await waitFor(() => expect(tabPaths()).toEqual(['README.md']));
    fireEvent.click(within(tab('README.md')).getByTestId('tab-close'));
    expect(await screen.findByText('Select a note to start editing')).toBeTruthy();
    expect(screen.queryByTestId('tab-bar')).toBeNull();
    await waitFor(() => expect(window.location.hash).toBe(''));
  });

  it('persists open tabs per vault (A keeps its tabs after visiting B)', async () => {
    await openAlpha();
    fireEvent.click(await screen.findByText('notes'));
    fireEvent.click(await screen.findByText('a.md'));
    await waitFor(() => expect(tabPaths()).toEqual(['README.md', 'notes/a.md']));
    // Switch to vault B → its own (empty) tab set, just README.
    fireEvent.click(screen.getByTestId('vault-switcher'));
    fireEvent.click(within((await screen.findByText('Switch vault')).parentElement as HTMLElement).getByText('beta'));
    await waitFor(() => expect(tabPaths()).toEqual(['README.md']));
    // Back to A → its two tabs are restored from localStorage.
    fireEvent.click(screen.getByTestId('vault-switcher'));
    fireEvent.click(within((await screen.findByText('Switch vault')).parentElement as HTMLElement).getByText('alpha'));
    await waitFor(() => expect(tabPaths()).toEqual(['README.md', 'notes/a.md']));
  });

  it('renaming the active file via the tab menu updates its tab and the hash', async () => {
    await openAlpha();
    const editor = await screen.findByTestId('live-editor');
    await waitFor(() => expect(editor.textContent).toContain('Readme'));
    fireEvent.contextMenu(tab('README.md'));
    fireEvent.click(await screen.findByText('Rename'));
    const input = screen.getByTestId('tab-rename-input');
    fireEvent.change(input, { target: { value: 'GUIDE.md' } });
    fireEvent.keyDown(input, { key: 'Enter' });
    await waitFor(() => expect(renameFile).toHaveBeenCalledWith('v1', 'README.md', 'GUIDE.md'));
    await waitFor(() => expect(tabPaths()).toEqual(['GUIDE.md']));
    await waitFor(() => expect(parseHash(window.location.hash)).toEqual({ vaultId: 'vidA', path: 'GUIDE.md' }));
  });

  it('double-clicking a tab puts it into inline rename', async () => {
    await openAlpha();
    await screen.findByTestId('live-editor');
    fireEvent.doubleClick(tab('README.md'));
    const input = await screen.findByTestId('tab-rename-input');
    expect((input as HTMLInputElement).value).toBe('README.md');
    fireEvent.change(input, { target: { value: 'GUIDE.md' } });
    fireEvent.keyDown(input, { key: 'Enter' });
    await waitFor(() => expect(renameFile).toHaveBeenCalledWith('v1', 'README.md', 'GUIDE.md'));
  });

  it('moving the active file (drag-drop) remaps its tab and the hash', async () => {
    await openAlpha();
    await screen.findByTestId('live-editor');
    const dt = () => ({ effectAllowed: '', setData: vi.fn(), getData: vi.fn() });
    fireEvent.dragStart(treeRow('README.md'), { dataTransfer: dt() });
    fireEvent.drop(treeRow('notes'));
    await waitFor(() => expect(renameFile).toHaveBeenCalledWith('v1', 'README.md', 'notes/README.md'));
    await waitFor(() => expect(tabPaths()).toEqual(['notes/README.md']));
    await waitFor(() => expect(parseHash(window.location.hash)?.path).toBe('notes/README.md'));
  });

  it('deleting the active file drops its tab and switches to a neighbor', async () => {
    await openAlpha();
    fireEvent.click(await screen.findByText('notes'));
    fireEvent.click(await screen.findByText('a.md')); // active = notes/a.md
    await waitFor(() => expect(tabPaths()).toEqual(['README.md', 'notes/a.md']));
    fireEvent.contextMenu(treeRow('a.md'));
    fireEvent.click(await screen.findByText('Delete'));
    fireEvent.click(await screen.findByTestId('confirm-delete'));
    await waitFor(() => expect(deleteFile).toHaveBeenCalledWith('v1', 'notes/a.md'));
    await waitFor(() => expect(tabPaths()).toEqual(['README.md']));
    await waitFor(() => expect(parseHash(window.location.hash)?.path).toBe('README.md'));
  });

  // ----- new merged-header behaviors: tab right-click menu + drags -----
  const openTwoTabs = async () => {
    await openAlpha();
    fireEvent.click(await screen.findByText('notes'));
    fireEvent.click(await screen.findByText('a.md'));
    await waitFor(() => expect(tabPaths()).toEqual(['README.md', 'notes/a.md']));
  };

  it('Close in the tab right-click menu closes just that tab', async () => {
    await openTwoTabs(); // active = notes/a.md
    fireEvent.contextMenu(tab('README.md'));
    fireEvent.click(await screen.findByText('Close'));
    await waitFor(() => expect(tabPaths()).toEqual(['notes/a.md']));
    expect(tab('notes/a.md').getAttribute('aria-selected')).toBe('true');
  });

  it('lists the tab right-click menu items in order: Close, Close Others, Close to the Left, Close to the Right, Close All, Rename, Delete', async () => {
    await openTwoTabs();
    fireEvent.contextMenu(tab('README.md'));
    const close = await screen.findByText('Close');
    // The menu container holds the items in DOM order.
    const menu = close.closest('div[style*="position: fixed"]') as HTMLElement;
    const rendered = Array.from(menu.querySelectorAll('.asp-hover-soft')).map((el) => el.textContent || '');
    expect(rendered).toEqual(['Close', 'Close Others', 'Close to the Left', 'Close to the Right', 'Close All', 'Rename', 'Delete']);
    // Right-click context menu is text-only: no leading icons or × glyphs.
    expect(menu.querySelector('svg')).toBeNull();
    expect(menu.textContent).not.toContain('×');
  });

  it('Close Others closes every tab but the right-clicked one (switching active when needed)', async () => {
    await openTwoTabs(); // active = notes/a.md
    fireEvent.contextMenu(tab('README.md'));
    fireEvent.click(await screen.findByText('Close Others'));
    await waitFor(() => expect(tabPaths()).toEqual(['README.md']));
    expect(tab('README.md').getAttribute('aria-selected')).toBe('true');
  });

  it('Close to the Left closes the tabs before the right-clicked one', async () => {
    await openTwoTabs(); // tabs [README, notes/a.md], active notes/a.md
    fireEvent.contextMenu(tab('notes/a.md'));
    fireEvent.click(await screen.findByText('Close to the Left'));
    await waitFor(() => expect(tabPaths()).toEqual(['notes/a.md']));
    // The right-clicked tab was already active, so it stays active.
    expect(tab('notes/a.md').getAttribute('aria-selected')).toBe('true');
  });

  it('Close to the Right closes the tabs after the right-clicked one (switching active when needed)', async () => {
    await openTwoTabs(); // tabs [README, notes/a.md], active notes/a.md
    fireEvent.contextMenu(tab('README.md'));
    fireEvent.click(await screen.findByText('Close to the Right'));
    await waitFor(() => expect(tabPaths()).toEqual(['README.md']));
    expect(tab('README.md').getAttribute('aria-selected')).toBe('true');
  });

  it('Close All closes every tab and lands on the vault default file', async () => {
    await openTwoTabs();
    fireEvent.contextMenu(tab('notes/a.md'));
    fireEvent.click(await screen.findByText('Close All'));
    await waitFor(() => expect(tabPaths()).toEqual(['README.md']));
    expect(tab('README.md').getAttribute('aria-selected')).toBe('true');
    await waitFor(() => expect(parseHash(window.location.hash)?.path).toBe('README.md'));
  });

  it('Close to the Right with three tabs keeps the left side and reselects a neighbor', async () => {
    await openTwoTabs(); // [README, notes/a.md], active notes/a.md
    // Create a third tab (active = the new untitled.md), to the RIGHT of notes/a.md.
    fireEvent.click(screen.getByTitle('New note'));
    fireEvent.click(await screen.findByText('New file'));
    await waitFor(() => expect(tabPaths()).toEqual(['README.md', 'notes/a.md', 'untitled.md']));
    fireEvent.contextMenu(tab('notes/a.md'));
    fireEvent.click(await screen.findByText('Close to the Right'));
    await waitFor(() => expect(tabPaths()).toEqual(['README.md', 'notes/a.md']));
    // The active file (untitled.md) was closed → nearest surviving neighbor wins.
    expect(tab('notes/a.md').getAttribute('aria-selected')).toBe('true');
  });

  it('does NOT delete files when closing (Close Others is non-destructive)', async () => {
    await openTwoTabs();
    fireEvent.contextMenu(tab('README.md'));
    fireEvent.click(await screen.findByText('Close Others'));
    await waitFor(() => expect(tabPaths()).toEqual(['README.md']));
    expect(deleteFile).not.toHaveBeenCalled();
  });

  it('Delete in the tab right-click menu deletes the file and closes its tab', async () => {
    await openTwoTabs();
    fireEvent.contextMenu(tab('notes/a.md'));
    fireEvent.click(await screen.findByText('Delete'));
    fireEvent.click(await screen.findByTestId('confirm-delete'));
    await waitFor(() => expect(deleteFile).toHaveBeenCalledWith('v1', 'notes/a.md'));
    await waitFor(() => expect(tabPaths()).toEqual(['README.md']));
  });

  it('the tab Delete asks for confirmation first and deletes nothing until confirmed', async () => {
    await openTwoTabs();
    fireEvent.contextMenu(tab('notes/a.md'));
    fireEvent.click(await screen.findByText('Delete'));
    // The confirm modal is up, naming the file; nothing deleted yet.
    expect(await screen.findByTestId('delete-confirm')).toBeTruthy();
    expect(screen.getByTestId('delete-confirm').textContent).toContain('a.md');
    expect(deleteFile).not.toHaveBeenCalled();
    expect(tabPaths()).toEqual(['README.md', 'notes/a.md']);
  });

  it('canceling the confirm modal (Cancel button) deletes nothing', async () => {
    await openTwoTabs();
    fireEvent.contextMenu(tab('notes/a.md'));
    fireEvent.click(await screen.findByText('Delete'));
    fireEvent.click(await screen.findByText('Cancel'));
    await waitFor(() => expect(screen.queryByTestId('delete-confirm')).toBeNull());
    expect(deleteFile).not.toHaveBeenCalled();
    expect(tabPaths()).toEqual(['README.md', 'notes/a.md']);
  });

  it('Escape on the confirm modal closes it without deleting', async () => {
    await openTwoTabs();
    fireEvent.contextMenu(tab('notes/a.md'));
    fireEvent.click(await screen.findByText('Delete'));
    fireEvent.keyDown(await screen.findByTestId('delete-confirm'), { key: 'Escape' });
    await waitFor(() => expect(screen.queryByTestId('delete-confirm')).toBeNull());
    expect(deleteFile).not.toHaveBeenCalled();
    expect(tabPaths()).toEqual(['README.md', 'notes/a.md']);
  });

  it('clicking the confirm-modal overlay closes it without deleting', async () => {
    await openTwoTabs();
    fireEvent.contextMenu(tab('notes/a.md'));
    fireEvent.click(await screen.findByText('Delete'));
    fireEvent.click(await screen.findByTestId('delete-confirm-overlay'));
    await waitFor(() => expect(screen.queryByTestId('delete-confirm')).toBeNull());
    expect(deleteFile).not.toHaveBeenCalled();
  });

  it('Enter on the confirm modal confirms the delete', async () => {
    await openTwoTabs();
    fireEvent.contextMenu(tab('notes/a.md'));
    fireEvent.click(await screen.findByText('Delete'));
    fireEvent.keyDown(await screen.findByTestId('delete-confirm'), { key: 'Enter' });
    await waitFor(() => expect(deleteFile).toHaveBeenCalledWith('v1', 'notes/a.md'));
  });

  it('the keyboard Delete key routes through the confirm modal', async () => {
    await openTwoTabs(); // active = notes/a.md, single selection
    fireEvent.keyDown(document.body, { key: 'Delete' });
    // Nothing deleted until the modal is confirmed.
    expect(await screen.findByTestId('delete-confirm')).toBeTruthy();
    expect(deleteFile).not.toHaveBeenCalled();
    fireEvent.click(screen.getByTestId('confirm-delete'));
    await waitFor(() => expect(deleteFile).toHaveBeenCalledWith('v1', 'notes/a.md'));
  });

  it('reorders tabs by dragging and persists the new order to localStorage', async () => {
    await openTwoTabs();
    // Reordering now goes through @dnd-kit/sortable (pointer/keyboard) rather than
    // native HTML5 drag. jsdom has no layout, so we drive it through the
    // KeyboardSensor: give the tabs a synthetic horizontal layout, then pick up
    // notes/a.md (idx 1), ArrowLeft, drop → it lands before README.md (idx 0).
    const tick = () => new Promise((r) => setTimeout(r, 0));
    screen.getAllByTestId('tab').forEach((t, i) => {
      (t as HTMLElement).getBoundingClientRect = () =>
        ({ left: i * 100, right: i * 100 + 90, top: 0, bottom: 30, width: 90, height: 30, x: i * 100, y: 0, toJSON() {} }) as DOMRect;
    });
    const grabbed = screen.getAllByTestId('tab')[1]; // notes/a.md
    grabbed.focus();
    fireEvent.keyDown(grabbed, { key: ' ', code: 'Space' }); // pick up
    await tick();
    fireEvent.keyDown(grabbed, { key: 'ArrowLeft', code: 'ArrowLeft' });
    await tick();
    fireEvent.keyDown(grabbed, { key: ' ', code: 'Space' }); // drop
    await tick();
    await waitFor(() => expect(tabPaths()).toEqual(['notes/a.md', 'README.md']));
    await waitFor(() => expect(JSON.parse(localStorage.getItem('asp.tabs.vidA')!)).toEqual(['notes/a.md', 'README.md']));
  }, 15000);

  it('dropping a tree file on the tab strip OPENS it as a tab (does not move it)', async () => {
    await openAlpha();
    await waitFor(() => expect(tabPaths()).toEqual(['README.md']));
    fireEvent.click(await screen.findByText('notes')); // expand to reveal a.md
    // A dataTransfer that actually stores what FileTree.onDragStart sets.
    const store: Record<string, string> = {};
    const dt = { effectAllowed: '', setData: (k: string, v: string) => { store[k] = v; }, getData: (k: string) => store[k] ?? '' };
    fireEvent.dragStart(treeRow('a.md'), { dataTransfer: dt });
    fireEvent.drop(screen.getByTestId('tab-bar'), { dataTransfer: dt });
    await waitFor(() => expect(tabPaths()).toEqual(['README.md', 'notes/a.md']));
    expect(renameFile).not.toHaveBeenCalled(); // opened, NOT moved
  });

  it('still MOVES a file when it is dropped on a folder row (not the tab strip)', async () => {
    await openAlpha();
    await screen.findByTestId('live-editor');
    const dt = () => ({ effectAllowed: '', setData: vi.fn(), getData: vi.fn() });
    fireEvent.dragStart(treeRow('README.md'), { dataTransfer: dt() });
    fireEvent.drop(treeRow('notes'));
    await waitFor(() => expect(renameFile).toHaveBeenCalledWith('v1', 'README.md', 'notes/README.md'));
  });
});
import { afterAll as __aa, mock as __mk } from 'bun:test';
__aa(() => __mk.restore());
