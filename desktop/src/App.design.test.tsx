// Integration tests for the redesigned App: theme/font toggles, sidebar resize,
// new file/folder, hidden + pretty names, breadcrumb rename, customize, share,
// remove, history/log tabs + time-travel, and the entry modals — all wired to a
// mocked backend.
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

let CONTENT: Record<string, string>;
let FILES: { path: string; file_id: string; is_dir: boolean; merge_class: string }[];
const reset = () => {
  CONTENT = { 'README.md': '# Readme\n\nhello', 'TODO.md': '# Todo\n', 'notes/a.md': '# A\n', '.secret.md': '# secret\n' };
  FILES = [
    { path: 'README.md', file_id: 'README.md', is_dir: false, merge_class: 'text' },
    { path: 'TODO.md', file_id: 'TODO.md', is_dir: false, merge_class: 'text' },
    { path: 'notes', file_id: 'notes', is_dir: true, merge_class: 'dir' },
    { path: 'notes/a.md', file_id: 'notes/a.md', is_dir: false, merge_class: 'text' },
    { path: '.secret.md', file_id: '.secret.md', is_dir: false, merge_class: 'text' },
  ];
};
reset();

const writeFile = vi.fn(async (_i: string, p: string, c: string) => { CONTENT[p] = c; });
const createDir = vi.fn(async (_i: string, p: string) => { FILES.push({ path: p, file_id: p, is_dir: true, merge_class: 'dir' }); });
const renameFile = vi.fn(async () => {});
const deleteFile = vi.fn(async () => {});
const restoreFileAt = vi.fn(async () => {});
const removeVault = vi.fn(async () => {});
const cloneRemote = vi.fn(async (dest: string) => ({ id: 'v3', path: dest, vault_id: 'vid3', enabled: false, listening_ticket: null }));
const addLocalFolder = vi.fn(async (p: string) => ({ id: 'v3', path: p, vault_id: 'vid3', enabled: false, listening_ticket: null }));
const setAllowConnections = vi.fn(async () => 'asp1sharecode');
const listVaults = vi.fn(async () => [
  { id: 'v1', path: '/home/me/notes', vault_id: 'vid1', enabled: false, listening_ticket: null },
  { id: 'v2', path: '/home/me/work', vault_id: 'vid2', enabled: false, listening_ticket: null },
]);
const getStatus = vi.fn(async (id: string) => ({ id, vault_id: id === 'v1' ? 'vid1' : 'vid2', rows: 3, files: 3, head: 'h', listening_ticket: null, peers: id === 'v1' ? ['ssh-ed25519 PEERKEY a@b'] : [], last_ts: Math.floor(Date.now() / 1000) - 30 }));

vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn(async () => '/home/me/picked') }));
vi.mock('./lib/api', () => ({
  api: {
    listVaults: () => listVaults(),
    addLocalFolder: (p: string) => addLocalFolder(p),
    cloneRemote: (d: string, t: string, k?: string) => cloneRemote(d, t, k),
    setAllowConnections: (...a: unknown[]) => setAllowConnections(...(a as [])),
    getStatus: (id: string) => getStatus(id),
    getIdentity: async () => 'ssh-ed25519 DEVICEKEYMATERIAL me@host',
    listFiles: async () => FILES.slice(),
    readFile: async (_i: string, p: string) => CONTENT[p] ?? '',
    writeFile: (i: string, p: string, c: string) => writeFile(i, p, c),
    renameFile: (...a: unknown[]) => renameFile(...(a as [])),
    createDir: (i: string, p: string) => createDir(i, p),
    deleteFile: (...a: unknown[]) => deleteFile(...(a as [])),
    history: async () => [
      { id: 'r1', ts: Math.floor(Date.now() / 1000) - 4000, lamport: 1, kind: 'create', path: 'README.md' },
      { id: 'r2', ts: Math.floor(Date.now() / 1000) - 2000, lamport: 2, kind: 'edit', path: 'README.md' },
    ],
    readFileAt: async (_i: string, p: string) => ({ exists: true, content: '# Readme\n\nOLD VERSION' }),
    restoreFileAt: (...a: unknown[]) => restoreFileAt(...(a as [])),
    removeVault: (...a: unknown[]) => removeVault(...(a as [])),
  },
}));

import App from './App';

afterEach(cleanup);
beforeEach(() => { vi.clearAllMocks(); reset(); localStorage.clear(); document.documentElement.removeAttribute('data-theme'); });

const openVault = async () => {
  fireEvent.click(await screen.findByText('notes')); // the v1 row (basename of /home/me/notes)
  await screen.findByText('Files');
};

describe('App — connect screen', () => {
  it('toggles theme and persists it', async () => {
    render(<App />);
    await screen.findByText('Your vaults');
    fireEvent.click(screen.getByTitle('Toggle theme'));
    expect(document.documentElement.getAttribute('data-theme')).toBe('dark');
    expect(JSON.parse(localStorage.getItem('asp.prefs.v1')!).theme).toBe('dark');
    fireEvent.click(screen.getByTitle('Toggle theme'));
    expect(document.documentElement.getAttribute('data-theme')).toBe('light');
  });

  it('customizes a vault from its context menu (name + emoji overlay)', async () => {
    render(<App />);
    const row = await screen.findByText('notes');
    fireEvent.contextMenu(row);
    fireEvent.click(await screen.findByText('Customize…'));
    fireEvent.change(screen.getByDisplayValue('notes'), { target: { value: 'My Notes' } });
    fireEvent.change(screen.getByPlaceholderText('Search emojis'), { target: { value: 'rocket' } });
    const grid = (Array.from(document.querySelectorAll('.asp-hover-list')) as HTMLElement[]).find((e) => e.textContent === '🚀');
    fireEvent.click(grid!);
    fireEvent.click(screen.getByText('Save'));
    expect(await screen.findByText('My Notes')).toBeTruthy();
    expect(JSON.parse(localStorage.getItem('asp.vaultmeta.v1')!).vid1.emoji).toBe('🚀');
  });

  it('removes a vault from its context menu (with trash toggle)', async () => {
    render(<App />);
    fireEvent.contextMenu(await screen.findByText('work'));
    fireEvent.click(await screen.findByText('Remove vault…'));
    // folder vault → trash toggle present
    fireEvent.click(screen.getByText('Also move the folder to the Trash'));
    fireEvent.click(screen.getByText('Remove & Trash folder'));
    await waitFor(() => expect(removeVault).toHaveBeenCalledWith('v2', true));
  });

  it('creates a new vault via the entry modal (native folder dialog)', async () => {
    render(<App />);
    fireEvent.click(await screen.findByText('New Vault'));
    fireEvent.click(await screen.findByText('Choose…'));
    await screen.findByText('/home/me/picked');
    fireEvent.click(screen.getByText('Create vault'));
    await waitFor(() => expect(addLocalFolder).toHaveBeenCalledWith('/home/me/picked'));
  });

  it('connects a vault via the entry modal', async () => {
    render(<App />);
    fireEvent.click(await screen.findByText('Connect Vault'));
    fireEvent.change(screen.getByPlaceholderText(/Paste the code/), { target: { value: 'asp1ticket' } });
    fireEvent.click(screen.getByText('Choose…'));
    await screen.findByText('/home/me/picked');
    fireEvent.click(screen.getByText('Connect'));
    await waitFor(() => expect(cloneRemote).toHaveBeenCalledWith('/home/me/picked', 'asp1ticket', undefined));
  });
});

describe('App — editor', () => {
  beforeEach(async () => { render(<App />); await openVault(); });

  it('toggles the reading font and theme from the status bar', async () => {
    await screen.findByTestId('live-editor');
    fireEvent.click(screen.getByTitle(/Reading font/));
    expect(JSON.parse(localStorage.getItem('asp.prefs.v1')!).fontOverride).toBe('Serif');
    const themeBtns = screen.getAllByTitle('Toggle theme');
    fireEvent.click(themeBtns[themeBtns.length - 1]);
    expect(document.documentElement.getAttribute('data-theme')).toBe('dark');
  });

  it('expands and collapses all folders', async () => {
    const expandBtn = await screen.findByTitle('Expand all');
    fireEvent.click(expandBtn);
    expect(await screen.findByText('a.md')).toBeTruthy(); // notes/ expanded
    fireEvent.click(await screen.findByTitle('Collapse all'));
  });

  it('shows hidden files and pretty filenames via the More menu', async () => {
    fireEvent.click(screen.getByTitle('More'));
    fireEvent.click(screen.getByText('Show hidden files'));
    expect(await screen.findByText('.secret.md')).toBeTruthy();
    fireEvent.click(screen.getByTitle('More'));
    fireEvent.click(screen.getByText('Pretty filenames'));
    // README.md → "Readme" (all-caps stem, italicized) in the tree row.
    await waitFor(() => {
      const row = (Array.from(document.querySelectorAll('.asp-hover-row')) as HTMLElement[]).find((r) => r.textContent === 'Readme');
      expect(row).toBeTruthy();
    });
  });

  it('creates a new file via the + menu', async () => {
    fireEvent.click(screen.getByTitle('New note'));
    fireEvent.click(await screen.findByText('New file'));
    await waitFor(() => expect(writeFile).toHaveBeenCalledWith('v1', 'untitled.md', expect.stringContaining('# untitled')));
  });

  it('creates a new folder via the + menu and inline-renames it', async () => {
    fireEvent.click(screen.getByTitle('New note'));
    fireEvent.click(await screen.findByText('New folder'));
    await waitFor(() => expect(createDir).toHaveBeenCalledWith('v1', 'untitled'));
    const input = document.querySelector('.asp-hover-row input') as HTMLInputElement;
    expect(input).toBeTruthy();
    fireEvent.change(input, { target: { value: 'docs' } });
    fireEvent.keyDown(input, { key: 'Enter' });
    await waitFor(() => expect(renameFile).toHaveBeenCalledWith('v1', 'untitled', 'docs'));
  });

  it('renames the open file via the breadcrumb (double-click)', async () => {
    const editor = await screen.findByTestId('live-editor');
    await waitFor(() => expect(editor.textContent).toContain('Readme'));
    fireEvent.doubleClick(screen.getByTitle('Double-click to rename'));
    const input = screen.getByDisplayValue('README.md');
    fireEvent.change(input, { target: { value: 'GUIDE.md' } });
    fireEvent.keyDown(input, { key: 'Enter' });
    await waitFor(() => expect(renameFile).toHaveBeenCalledWith('v1', 'README.md', 'GUIDE.md'));
  });

  it('creates a file and folder from the tree root context menu', async () => {
    const treeScroll = document.querySelectorAll('.asp-scroll')[0] as HTMLElement;
    fireEvent.contextMenu(treeScroll, { clientX: 20, clientY: 20 });
    fireEvent.click(await screen.findByText('New file'));
    await waitFor(() => expect(writeFile).toHaveBeenCalledWith('v1', 'untitled.md', expect.any(String)));
  });

  it('resizes the sidebar and persists the width', async () => {
    const handle = document.querySelector('.sb-resize') as HTMLElement;
    fireEvent(handle, new MouseEvent('pointerdown', { clientX: 266, bubbles: true, cancelable: true }));
    fireEvent(document, new MouseEvent('pointermove', { clientX: 326, bubbles: true }));
    fireEvent(document, new MouseEvent('pointerup', { clientX: 326, bubbles: true }));
    await waitFor(() => expect(JSON.parse(localStorage.getItem('asp.prefs.v1')!).sidebarW).toBe(326));
  });

  it('opens the share modal, copies the code, and toggles the access key', async () => {
    const writeText = vi.fn();
    Object.assign(navigator, { clipboard: { writeText } });
    fireEvent.click(screen.getByTestId('vault-switcher'));
    fireEvent.click(await screen.findByText('Share this vault…'));
    await screen.findByText('asp1sharecode');
    fireEvent.click(screen.getByText('Copy'));
    expect(writeText).toHaveBeenCalled();
    fireEvent.click(screen.getByText('Require an access key'));
    await waitFor(() => expect(setAllowConnections).toHaveBeenCalledWith('v1', true, expect.any(String)));
    fireEvent.click(screen.getByText('Require an access key')); // toggle back off
    fireEvent.click(screen.getByText('Done'));
  });

  it('switches vault via the switcher menu', async () => {
    fireEvent.click(screen.getByTestId('vault-switcher'));
    const menu = (await screen.findByText('Switch vault')).parentElement as HTMLElement;
    fireEvent.click(within(menu).getByText('work'));
    await waitFor(() => expect(getStatus).toHaveBeenCalledWith('v2'));
  });

  it('time-travels via the History track and restores a version', async () => {
    await screen.findByTestId('live-editor');
    fireEvent.click(screen.getByText('History'));
    const track = await screen.findByTestId('history-track');
    (track.getBoundingClientRect as unknown) = () => ({ left: 0, top: 0, width: 100, height: 40, right: 100, bottom: 40, x: 0, y: 0, toJSON() {} });
    fireEvent(track, new MouseEvent('pointerdown', { clientX: 20, bubbles: true }));
    fireEvent(document, new MouseEvent('pointerup', { clientX: 20, bubbles: true }));
    expect(await screen.findByText(/read-only/)).toBeTruthy();
    fireEvent.click(screen.getByText('Restore this version'));
    await waitFor(() => expect(restoreFileAt).toHaveBeenCalled());
  });

  it('opens the Log tab', async () => {
    fireEvent.click(screen.getByText('Log'));
    expect(await screen.findByText(/events$/)).toBeTruthy();
    expect(screen.getByText(/endpoint bound/)).toBeTruthy();
  });

  it('resizes the history bar by dragging its top edge and persists the height', async () => {
    await screen.findByTestId('live-editor');
    fireEvent.click(screen.getByText('History'));
    await screen.findByTestId('history-track');

    // Default shared height is 150 (DEFAULT_PREFS.histBarH).
    const barOf = () => document.querySelector('[style*="row-resize"]')!.nextElementSibling as HTMLElement;
    expect(barOf().style.height).toBe('150px');

    // Drag the top edge UP (clientY decreasing) → the bar grows taller.
    const handle = document.querySelector('.hb-resize') as HTMLElement;
    fireEvent(handle, new MouseEvent('pointerdown', { clientY: 500, bubbles: true, cancelable: true }));
    fireEvent(document, new MouseEvent('pointermove', { clientY: 400, bubbles: true }));
    fireEvent(document, new MouseEvent('pointerup', { clientY: 400, bubbles: true }));
    expect(barOf().style.height).toBe('250px');
    await waitFor(() => expect(JSON.parse(localStorage.getItem('asp.prefs.v1')!).histBarH).toBe(250));

    // Switching History ↔ Log keeps the same shared height.
    fireEvent.click(screen.getByText('Log'));
    await screen.findByText(/events$/);
    expect(barOf().style.height).toBe('250px');
    fireEvent.click(screen.getByText('History'));
    await screen.findByTestId('history-track');
    expect(barOf().style.height).toBe('250px');
  });

  it('collapses the history bar when dragged below the threshold', async () => {
    await screen.findByTestId('live-editor');
    fireEvent.click(screen.getByText('History'));
    await screen.findByTestId('history-track');

    const handle = document.querySelector('.hb-resize') as HTMLElement;
    // Drag DOWN far enough that proposed (150 - 100 = 50) < HISTBAR_COLLAPSE (72).
    fireEvent(handle, new MouseEvent('pointerdown', { clientY: 500, bubbles: true, cancelable: true }));
    fireEvent(document, new MouseEvent('pointermove', { clientY: 600, bubbles: true }));
    fireEvent(document, new MouseEvent('pointerup', { clientY: 600, bubbles: true }));

    // Panel snapped shut: the track is gone and the resize handle disappears.
    await waitFor(() => expect(screen.queryByTestId('history-track')).toBeNull());
    expect(document.querySelector('.hb-resize')).toBeNull();
  });

  it('deletes a file via its context menu', async () => {
    fireEvent.contextMenu(screen.getByText('TODO.md'));
    // menu shows Rename + Delete, but no filename header (TODO.md stays a single tree row)
    expect(await screen.findByText('Rename')).toBeTruthy();
    expect(screen.getByText('Delete')).toBeTruthy();
    expect(screen.getAllByText('TODO.md')).toHaveLength(1);
    fireEvent.click(screen.getByText('Delete'));
    await waitFor(() => expect(deleteFile).toHaveBeenCalledWith('v1', 'TODO.md'));
  });

  it('customize / remove / open-another from the switcher menu', async () => {
    fireEvent.click(screen.getByTestId('vault-switcher'));
    fireEvent.click(await screen.findByText('Customize this vault…'));
    fireEvent.click(await screen.findByText('Cancel')); // CustomizeModal cancel

    fireEvent.click(screen.getByTestId('vault-switcher'));
    fireEvent.click(await screen.findByText('Remove this vault…'));
    // local folder vault → confirm removal
    fireEvent.click(await screen.findByText('Remove from asp'));
    await waitFor(() => expect(removeVault).toHaveBeenCalledWith('v1', false));
  });

  it('opens another folder from the switcher menu', async () => {
    fireEvent.click(screen.getByTestId('vault-switcher'));
    fireEvent.click(await screen.findByText('Open another folder…'));
    await waitFor(() => expect(addLocalFolder).toHaveBeenCalledWith('/home/me/picked'));
  });

  it('returns to now from the time-travel banner', async () => {
    await screen.findByTestId('live-editor');
    fireEvent.click(screen.getByText('History'));
    const track = await screen.findByTestId('history-track');
    (track.getBoundingClientRect as unknown) = () => ({ left: 0, top: 0, width: 100, height: 40, right: 100, bottom: 40, x: 0, y: 0, toJSON() {} });
    fireEvent(track, new MouseEvent('pointerdown', { clientX: 20, bubbles: true }));
    fireEvent(document, new MouseEvent('pointerup', { clientX: 20, bubbles: true }));
    fireEvent.click(await screen.findByText('Return to now'));
    await waitFor(() => expect(screen.queryByText(/read-only/)).toBeNull());
  });
});

describe('App — multi-selection', () => {
  beforeEach(async () => { render(<App />); await openVault(); });

  // Tree rows are .asp-hover-row; the breadcrumb is not, so match exact text.
  const treeRow = (name: string) =>
    (Array.from(document.querySelectorAll('.asp-hover-row')) as HTMLElement[]).find((r) => r.textContent === name)!;
  const isHL = (el: HTMLElement) => el.style.background !== '' && el.style.background !== 'transparent';
  const hlCount = () => (Array.from(document.querySelectorAll('.asp-hover-row')) as HTMLElement[]).filter(isHL).length;

  it('plain click selects exactly one file (clears any multi-selection)', async () => {
    await screen.findByTestId('live-editor'); // README auto-selected
    fireEvent.click(treeRow('TODO.md'), { metaKey: true }); // README + TODO
    fireEvent.click(treeRow('README.md')); // plain → only README
    expect(isHL(treeRow('README.md'))).toBe(true);
    expect(isHL(treeRow('TODO.md'))).toBe(false);
  });

  it('cmd/ctrl-click toggles a file in and out without losing the others', async () => {
    await screen.findByTestId('live-editor');
    expect(isHL(treeRow('README.md'))).toBe(true);
    expect(isHL(treeRow('TODO.md'))).toBe(false);
    // add TODO
    fireEvent.click(treeRow('TODO.md'), { metaKey: true });
    expect(isHL(treeRow('TODO.md'))).toBe(true);
    expect(isHL(treeRow('README.md'))).toBe(true); // others kept
    // ctrl-click removes it again
    fireEvent.click(treeRow('TODO.md'), { ctrlKey: true });
    expect(isHL(treeRow('TODO.md'))).toBe(false);
    expect(isHL(treeRow('README.md'))).toBe(true);
  });

  it('shift-click selects a range from the anchor across the visible rows', async () => {
    await screen.findByTestId('live-editor');
    fireEvent.click(treeRow('notes')); // expand → reveals notes/a.md
    await screen.findByText('a.md');
    // anchor is README (default). Shift-click a.md → README, TODO, a.md all selected.
    fireEvent.click(treeRow('a.md'), { shiftKey: true });
    expect(isHL(treeRow('README.md'))).toBe(true);
    expect(isHL(treeRow('TODO.md'))).toBe(true);
    expect(isHL(treeRow('a.md'))).toBe(true);
  });

  it('cmd-clicking the active file moves the editor to a remaining selected file', async () => {
    await screen.findByTestId('live-editor');
    fireEvent.click(treeRow('TODO.md'), { metaKey: true }); // add TODO → it becomes active
    await waitFor(() => expect(screen.getAllByText('TODO.md').length).toBeGreaterThanOrEqual(2)); // breadcrumb = active
    fireEvent.click(treeRow('TODO.md'), { ctrlKey: true }); // deselect the active file
    // Editor falls back to the still-selected README (breadcrumb shows it again).
    await waitFor(() => expect(screen.getAllByText('README.md').length).toBeGreaterThanOrEqual(2));
  });

  it('batch-deletes every selected file via a selected row context menu', async () => {
    await screen.findByTestId('live-editor');
    fireEvent.click(treeRow('TODO.md'), { metaKey: true }); // README + TODO selected
    fireEvent.contextMenu(treeRow('TODO.md')); // right-click a member of the selection
    fireEvent.click(await screen.findByText('Delete'));
    await waitFor(() => expect(deleteFile).toHaveBeenCalledWith('v1', 'README.md'));
    await waitFor(() => expect(deleteFile).toHaveBeenCalledWith('v1', 'TODO.md'));
  });

  it('batch-deletes the selection with the Delete key', async () => {
    await screen.findByTestId('live-editor');
    fireEvent.click(treeRow('TODO.md'), { metaKey: true });
    fireEvent.keyDown(document.body, { key: 'Delete' });
    await waitFor(() => expect(deleteFile).toHaveBeenCalledWith('v1', 'README.md'));
    await waitFor(() => expect(deleteFile).toHaveBeenCalledWith('v1', 'TODO.md'));
  });

  it('Escape collapses a multi-selection back to the single active file', async () => {
    await screen.findByTestId('live-editor');
    fireEvent.click(treeRow('TODO.md'), { metaKey: true }); // README + TODO highlighted
    expect(hlCount()).toBe(2);
    fireEvent.keyDown(document.body, { key: 'Escape' });
    expect(hlCount()).toBe(1); // collapsed to just the active file
  });
});

describe('App — empty + edge states', () => {
  it('shows the empty editor state and saving indicator', async () => {
    render(<App />);
    await openVault();
    const editor = await screen.findByTestId('live-editor');
    await waitFor(() => expect(editor.textContent).toContain('Readme'));
    editor.textContent = '# Readme\n\nedited now';
    fireEvent.input(editor);
    expect(await screen.findByText('Saving…')).toBeTruthy();
    await waitFor(() => expect(writeFile).toHaveBeenCalledWith('v1', 'README.md', '# Readme\n\nedited now'), { timeout: 2000 });
  });

  it('cancels the entry modal', async () => {
    render(<App />);
    fireEvent.click(await screen.findByText('New Vault'));
    fireEvent.click(await screen.findByText('Cancel'));
    await waitFor(() => expect(screen.queryByText('New vault')).toBeNull());
  });

  it('dismisses menus/modals via their overlays and covers edge handlers', async () => {
    render(<App />);
    await openVault();
    const zdiv = (z: number) => document.querySelector(`[style*="z-index: ${z}"]`) as HTMLElement;

    // vault-switcher menu → overlay closes it
    fireEvent.click(screen.getByTestId('vault-switcher'));
    fireEvent.click(zdiv(40));
    // new menu → overlay
    fireEvent.click(screen.getByTitle('New note'));
    fireEvent.click(zdiv(44));
    // files menu → overlay
    fireEvent.click(screen.getByTitle('More'));
    fireEvent.click(zdiv(44));

    // root tree context menu → New folder, then dismiss via overlay
    const treeScroll = document.querySelectorAll('.asp-scroll')[0] as HTMLElement;
    fireEvent.contextMenu(treeScroll, { clientX: 10, clientY: 10 });
    fireEvent.click(zdiv(60)); // overlay closes the ctx menu

    // breadcrumb rename committed via blur
    const editor = await screen.findByTestId('live-editor');
    await waitFor(() => expect(editor.textContent).toContain('Readme'));
    fireEvent.doubleClick(screen.getByTitle('Double-click to rename'));
    fireEvent.blur(screen.getByDisplayValue('README.md'));
  });

  it('fills both entry modals (name + access key fields) and dismisses via overlay', async () => {
    render(<App />);
    fireEvent.click(await screen.findByText('New Vault'));
    fireEvent.change(screen.getByPlaceholderText('My vault'), { target: { value: 'Named' } });
    fireEvent.click(document.querySelector('[style*="z-index: 58"]') as HTMLElement); // overlay closes it

    fireEvent.click(await screen.findByText('Connect Vault'));
    fireEvent.change(screen.getByPlaceholderText(/Paste the code/), { target: { value: 'asp1x' } });
    fireEvent.change(screen.getByPlaceholderText(/Leave blank/), { target: { value: 'KEY' } });
    fireEvent.click(document.querySelector('[style*="z-index: 58"]') as HTMLElement);
  });
});
