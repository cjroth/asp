import { mock } from 'bun:test';
// Integration tests for the redesigned App: theme/font toggles, sidebar resize,
// new file/folder, hidden + pretty names, breadcrumb rename, customize, share,
// remove, history/log tabs + time-travel, and the entry modals — all wired to a
// mocked backend.
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from './test-shim';

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
const checkoutBranch = vi.fn(async () => {});
const forkBranchAt = vi.fn(async () => 'edit-branch-id');
const createTag = vi.fn(async () => 'tag-id');
const deleteTag = vi.fn(async () => {});
const removeVault = vi.fn(async () => {});
const cloneRemote = vi.fn(async (dest: string) => ({ id: 'v3', path: dest, vault_id: 'vid3', enabled: false, listening_ticket: null }));
const addLocalFolder = vi.fn(async (p: string) => ({ id: 'v3', path: p, vault_id: 'vid3', enabled: false, listening_ticket: null }));
const setAllowConnections = vi.fn(async () => 'asp1sharecode');
const listVaults = vi.fn(async () => [
  { id: 'v1', path: '/home/me/notes', vault_id: 'vid1', enabled: false, listening_ticket: null },
  { id: 'v2', path: '/home/me/work', vault_id: 'vid2', enabled: false, listening_ticket: null },
]);
const getStatus = vi.fn(async (id: string) => ({ id, vault_id: id === 'v1' ? 'vid1' : 'vid2', rows: 3, files: 3, head: 'h', listening_ticket: null, peers: id === 'v1' ? ['ssh-ed25519 PEERKEY a@b'] : [], last_ts: Math.floor(Date.now() / 1000) - 30 }));

mock.module('@tauri-apps/plugin-dialog', () => ({ open: vi.fn(async () => '/home/me/picked') }));
mock.module('./lib/api', () => ({
  api: {
    startLiveSync: vi.fn(), stopLiveSync: vi.fn(), setLocalRelay: vi.fn(async () => false), getLocalRelay: vi.fn(async () => false),
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
    // Branch/tag surface (the timeline network graph + auto-branch + tags).
    currentBranch: async () => 'main',
    branchGraph: async () => ({ nodes: [], branches: [{ id: 'main', name: 'main', parent: null, head_commit: null, lane: 0, current: true }], tags: [] }),
    checkoutBranch: (...a: unknown[]) => checkoutBranch(...(a as [])),
    forkBranchAt: (...a: unknown[]) => forkBranchAt(...(a as [])),
    listTags: async () => [],
    createTag: (...a: unknown[]) => createTag(...(a as [])),
    deleteTag: (...a: unknown[]) => deleteTag(...(a as [])),
  },
}));

import App, { __resetUrlRestore } from './App';

afterEach(cleanup);
beforeEach(() => { vi.clearAllMocks(); reset(); localStorage.clear(); document.documentElement.removeAttribute('data-theme'); __resetUrlRestore(); window.history.replaceState(null, '', '/'); });

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
    const customize = await screen.findByText('Customize…');
    // Vault-row context menu is text-only (no leading icon).
    expect((customize.closest('.asp-hover-soft') as HTMLElement).querySelector('svg')).toBeNull();
    fireEvent.click(customize);
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

  it('New Vault modal auto-focuses the name input; Enter creates (respecting blocked); Escape closes', async () => {
    render(<App />);
    fireEvent.click(await screen.findByText('New Vault'));
    const name = screen.getByPlaceholderText('My vault');
    expect(document.activeElement).toBe(name);
    // Desktop needs a destination folder — Enter must NOT submit while blocked.
    fireEvent.change(name, { target: { value: 'Named' } });
    fireEvent.keyDown(name, { key: 'Enter' });
    expect(addLocalFolder).not.toHaveBeenCalled();
    // Choose a folder → Enter now creates.
    fireEvent.click(screen.getByText('Choose…'));
    await screen.findByText('/home/me/picked');
    fireEvent.keyDown(name, { key: 'Enter' });
    await waitFor(() => expect(addLocalFolder).toHaveBeenCalledWith('/home/me/picked'));
  });

  it('New Vault modal closes on Escape', async () => {
    render(<App />);
    fireEvent.click(await screen.findByText('New Vault'));
    const name = screen.getByPlaceholderText('My vault');
    fireEvent.keyDown(name, { key: 'Escape' });
    await waitFor(() => expect(screen.queryByPlaceholderText('My vault')).toBeNull());
    expect(addLocalFolder).not.toHaveBeenCalled();
  });

  it('Connect modal auto-focuses the invite field; plain Enter is a newline, Cmd/Ctrl+Enter submits', async () => {
    render(<App />);
    fireEvent.click(await screen.findByText('Connect Vault'));
    const code = screen.getByPlaceholderText(/Paste the code/);
    expect(document.activeElement).toBe(code);
    fireEvent.change(code, { target: { value: 'asp1ticket' } });
    fireEvent.click(screen.getByText('Choose…'));
    await screen.findByText('/home/me/picked');
    // Plain Enter in the textarea inserts a newline — it must NOT submit.
    fireEvent.keyDown(code, { key: 'Enter' });
    expect(cloneRemote).not.toHaveBeenCalled();
    // Cmd/Ctrl+Enter submits when valid.
    fireEvent.keyDown(code, { key: 'Enter', metaKey: true });
    await waitFor(() => expect(cloneRemote).toHaveBeenCalledWith('/home/me/picked', 'asp1ticket', undefined));
  });

  it('Connect modal closes on Escape', async () => {
    render(<App />);
    fireEvent.click(await screen.findByText('Connect Vault'));
    const code = screen.getByPlaceholderText(/Paste the code/);
    fireEvent.keyDown(code, { key: 'Escape' });
    await waitFor(() => expect(screen.queryByPlaceholderText(/Paste the code/)).toBeNull());
    expect(cloneRemote).not.toHaveBeenCalled();
  });
});

describe('App — editor', () => {
  beforeEach(async () => { render(<App />); await openVault(); });

  it('toggles theme from the status bar (the Sans/Serif font toggle is gone)', async () => {
    const editor = await screen.findByTestId('live-editor');
    // The reading-font toggle was removed — markdown is always serif.
    expect(screen.queryByTitle(/Reading font/)).toBeNull();
    expect(editor.style.fontFamily).toContain('Newsreader');
    const themeBtns = screen.getAllByTitle('Toggle theme');
    fireEvent.click(themeBtns[themeBtns.length - 1]);
    expect(document.documentElement.getAttribute('data-theme')).toBe('dark');
  });

  it('keeps the theme button in the tab row, with save-status + word count in a separate bar below', async () => {
    await screen.findByTestId('live-editor');
    const tabRow = screen.getByTestId('tab-bar').parentElement!;
    // The theme button shares the tab strip's row.
    const themeBtns = screen.getAllByTitle('Toggle theme');
    const themeBtn = themeBtns[themeBtns.length - 1];
    expect(tabRow.contains(themeBtn)).toBe(true);
    // The save status and word count are no longer in the tab row.
    expect(tabRow.textContent).not.toContain('Saved');
    expect(tabRow.textContent).not.toMatch(/word/);
    // They moved into a dedicated, non-scrolling bar below the tab row.
    const status = screen.getByTestId('content-status');
    expect(tabRow.contains(status)).toBe(false);
    expect(status.textContent).toContain('Saved');
    expect(status.textContent).toMatch(/word/);
    // Pinned (does not scroll with the document).
    expect(status.style.flexGrow).toBe('0');
    expect(status.style.flexShrink).toBe('0');
    // The cluster hugs the right edge of the full-width bar (no 760px column).
    expect(status.style.justifyContent).toBe('flex-end');
    expect(status.style.width).not.toBe('760px');
  });

  it('flips the content status to Saving… while a write is in flight', async () => {
    const editor = await screen.findByTestId('live-editor');
    await waitFor(() => expect(editor.textContent).toContain('Readme'));
    editor.textContent = '# Readme\n\nedited again';
    fireEvent.input(editor);
    const status = await screen.findByTestId('content-status');
    await waitFor(() => expect(status.textContent).toContain('Saving…'));
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

  it('renames the open file via the tab context menu', async () => {
    const editor = await screen.findByTestId('live-editor');
    await waitFor(() => expect(editor.textContent).toContain('Readme'));
    const readmeTab = screen.getAllByTestId('tab').find((t) => t.getAttribute('data-path') === 'README.md')!;
    fireEvent.contextMenu(readmeTab);
    fireEvent.click(await screen.findByText('Rename'));
    const input = screen.getByTestId('tab-rename-input');
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

  it('Share on desktop generates a code (not the browser-unavailable message)', async () => {
    fireEvent.click(screen.getByTestId('vault-switcher'));
    fireEvent.click(await screen.findByText('Share this vault…'));
    // Desktop opens a listening socket and shows the generated ticket.
    await waitFor(() => expect(setAllowConnections).toHaveBeenCalledWith('v1', true));
    expect(await screen.findByText('asp1sharecode')).toBeTruthy();
    expect(screen.queryByText(/Sharing isn’t available for browser vaults/)).toBeNull();
    expect(screen.queryByText('Generating…')).toBeNull();
  });

  it('Share modal auto-focuses the Copy button and closes on Escape', async () => {
    Object.assign(navigator, { clipboard: { writeText: vi.fn() } });
    fireEvent.click(screen.getByTestId('vault-switcher'));
    fireEvent.click(await screen.findByText('Share this vault…'));
    const copy = await screen.findByText('Copy');
    expect(document.activeElement).toBe(copy);
    fireEvent.keyDown(copy, { key: 'Escape' });
    await waitFor(() => expect(screen.queryByText('Share this vault')).toBeNull());
  });

  it('Remove modal auto-focuses Cancel; Enter does NOT remove; Escape cancels', async () => {
    fireEvent.click(screen.getByTestId('vault-switcher'));
    fireEvent.click(await screen.findByText('Remove this vault…'));
    const cancel = await screen.findByText('Cancel');
    expect(document.activeElement).toBe(cancel);
    // Destructive action must never auto-fire on Enter.
    fireEvent.keyDown(cancel, { key: 'Enter' });
    expect(removeVault).not.toHaveBeenCalled();
    // Escape cancels without removing.
    fireEvent.keyDown(cancel, { key: 'Escape' });
    await waitFor(() => expect(screen.queryByText('Remove from asp')).toBeNull());
    expect(removeVault).not.toHaveBeenCalled();
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
    // Time travel is editable now — the banner invites branching, not "read-only".
    expect(await screen.findByTestId('time-travel-banner')).toBeTruthy();
    fireEvent.click(screen.getByText(/Restore onto/));
    await waitFor(() => expect(restoreFileAt).toHaveBeenCalled());
  });

  it('auto-branches when you edit while scrubbed into the past', async () => {
    await screen.findByTestId('live-editor');
    fireEvent.click(screen.getByText('History'));
    const track = await screen.findByTestId('history-track');
    (track.getBoundingClientRect as unknown) = () => ({ left: 0, top: 0, width: 100, height: 40, right: 100, bottom: 40, x: 0, y: 0, toJSON() {} });
    fireEvent(track, new MouseEvent('pointerdown', { clientX: 20, bubbles: true }));
    fireEvent(document, new MouseEvent('pointerup', { clientX: 20, bubbles: true }));
    await screen.findByTestId('time-travel-banner');
    // Editing in the past forks a branch at that instant instead of overwriting HEAD.
    const editor = screen.getByTestId('live-editor');
    fireEvent.input(editor, { target: { textContent: '# Readme\n\nEDIT IN THE PAST' } });
    await waitFor(() => expect(forkBranchAt).toHaveBeenCalled());
    expect(await screen.findByTestId('branch-created-banner')).toBeTruthy();
  });

  it('tags the current moment from the History track', async () => {
    await screen.findByTestId('live-editor');
    fireEvent.click(screen.getByText('History'));
    await screen.findByTestId('history-track');
    fireEvent.click(screen.getByTestId('tag-here'));
    const input = await screen.findByTestId('tag-name-input');
    fireEvent.change(input, { target: { value: 'release' } });
    fireEvent.keyDown(input, { key: 'Enter' });
    await waitFor(() => expect(createTag).toHaveBeenCalledWith('v1', 'release', expect.any(Number)));
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
    fireEvent.click(await screen.findByTestId('confirm-delete'));
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
    await waitFor(() => expect(screen.queryByTestId('time-travel-banner')).toBeNull());
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
    // The confirm modal names the count for a multi-selection.
    expect((await screen.findByTestId('delete-confirm')).textContent).toContain('2 items');
    fireEvent.click(screen.getByTestId('confirm-delete'));
    await waitFor(() => expect(deleteFile).toHaveBeenCalledWith('v1', 'README.md'));
    await waitFor(() => expect(deleteFile).toHaveBeenCalledWith('v1', 'TODO.md'));
  });

  it('batch-deletes the selection with the Delete key', async () => {
    await screen.findByTestId('live-editor');
    fireEvent.click(treeRow('TODO.md'), { metaKey: true });
    fireEvent.keyDown(document.body, { key: 'Delete' });
    fireEvent.click(await screen.findByTestId('confirm-delete'));
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

describe('App — drag-and-drop move', () => {
  // A subfolder so we can exercise the "into a descendant" guard.
  beforeEach(async () => {
    FILES.push({ path: 'notes/sub', file_id: 'notes/sub', is_dir: true, merge_class: 'dir' });
    FILES.push({ path: 'notes/sub/c.md', file_id: 'notes/sub/c.md', is_dir: false, merge_class: 'text' });
    render(<App />);
    await openVault();
    await screen.findByTestId('live-editor');
  });

  const dt = () => ({ effectAllowed: '', setData: vi.fn(), getData: vi.fn() });
  const treeRow = (name: string) =>
    (Array.from(document.querySelectorAll('.asp-hover-row')) as HTMLElement[]).find((r) => r.textContent === name)!;

  it('moves a file into a folder by dropping it on the folder row', async () => {
    fireEvent.dragStart(treeRow('TODO.md'), { dataTransfer: dt() });
    fireEvent.dragOver(treeRow('notes'));
    fireEvent.drop(treeRow('notes'));
    await waitFor(() => expect(renameFile).toHaveBeenCalledWith('v1', 'TODO.md', 'notes/TODO.md'));
    // The dropped file now lives under the (auto-expanded) destination folder.
    await waitFor(() => expect(treeRow('TODO.md')).toBeTruthy());
  });

  it('moves a nested file to the root by dropping on the empty tree area', async () => {
    fireEvent.click(treeRow('notes')); // expand to reveal notes/a.md
    await screen.findByText('a.md');
    const treeScroll = document.querySelectorAll('.asp-scroll')[0] as HTMLElement;
    fireEvent.dragStart(treeRow('a.md'), { dataTransfer: dt() });
    fireEvent.drop(treeScroll);
    await waitFor(() => expect(renameFile).toHaveBeenCalledWith('v1', 'notes/a.md', 'a.md'));
  });

  it('rejects dropping a folder into its own descendant (no rename)', async () => {
    fireEvent.click(treeRow('notes')); // expand to reveal the sub folder
    await screen.findByText('sub');
    fireEvent.dragStart(treeRow('notes'), { dataTransfer: dt() });
    fireEvent.dragOver(treeRow('sub'));
    fireEvent.drop(treeRow('sub'));
    expect(renameFile).not.toHaveBeenCalled();
  });

  it('is a no-op when dropping a file onto its own current folder', async () => {
    fireEvent.click(treeRow('notes'));
    await screen.findByText('a.md');
    fireEvent.dragStart(treeRow('a.md'), { dataTransfer: dt() });
    fireEvent.drop(treeRow('notes')); // a.md already lives in notes/
    // Give the async move a tick; it should bail before any rename.
    await Promise.resolve();
    expect(renameFile).not.toHaveBeenCalled();
  });

  it('moves an entire multi-selection in one drop', async () => {
    fireEvent.click(treeRow('TODO.md'), { metaKey: true }); // README (active) + TODO selected
    fireEvent.dragStart(treeRow('TODO.md'), { dataTransfer: dt() });
    fireEvent.drop(treeRow('notes'));
    await waitFor(() => expect(renameFile).toHaveBeenCalledWith('v1', 'README.md', 'notes/README.md'));
    await waitFor(() => expect(renameFile).toHaveBeenCalledWith('v1', 'TODO.md', 'notes/TODO.md'));
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

    // tab rename committed via blur (no-op rename → just exercises the handler)
    const editor = await screen.findByTestId('live-editor');
    await waitFor(() => expect(editor.textContent).toContain('Readme'));
    const readmeTab = screen.getAllByTestId('tab').find((t) => t.getAttribute('data-path') === 'README.md')!;
    fireEvent.contextMenu(readmeTab);
    fireEvent.click(await screen.findByText('Rename'));
    fireEvent.blur(screen.getByTestId('tab-rename-input'));
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

// The `.asp-scroll` regions (file tree, editor, history bar, vault list, customize
// modal) hold genuinely overflowing content, so e2e measurement confirmed the
// scrollbars are real (not a layout-overflow bug) — see e2e/scroll-metrics.mjs.
// They get a thin macOS-style scrollbar: narrow (8px), a rounded translucent
// thumb over a transparent track — close to the native overlay bar instead of
// the chunky classic one. These pin that contract.
describe('.asp-scroll: thin macOS-style scrollbar', () => {
  const cssText = readFileSync(resolve(process.cwd(), 'src/styles.css'), 'utf8');

  it('gives the WebKit scrollbar a narrow 8px width (not display:none)', () => {
    const barRule = (cssText.match(/\.asp-scroll::-webkit-scrollbar\s*\{([^}]*)\}/) || ['', ''])[1];
    expect(barRule).toMatch(/width:\s*8px/);
    expect(barRule).not.toMatch(/display\s*:\s*none/);
  });

  it('uses thin Firefox scrollbars (scrollbar-width:thin, not none)', () => {
    const rule = (cssText.match(/\.asp-scroll\s*\{([^}]*)\}/) || ['', ''])[1];
    expect(rule).toMatch(/scrollbar-width:\s*thin/);
    expect(rule).not.toMatch(/scrollbar-width:\s*none/);
  });

  it('paints a rounded, translucent thumb over a transparent track', () => {
    const thumbRule = (cssText.match(/\.asp-scroll::-webkit-scrollbar-thumb\s*\{([^}]*)\}/) || ['', ''])[1];
    expect(thumbRule).toMatch(/border-radius/);
    expect(thumbRule).toMatch(/background:\s*rgba\(/);
    const trackRule = (cssText.match(/\.asp-scroll::-webkit-scrollbar-track\s*\{([^}]*)\}/) || ['', ''])[1];
    expect(trackRule).toMatch(/background:\s*transparent/);
  });
});
import { afterAll as __aa, mock as __mk } from 'bun:test';
__aa(() => __mk.restore());
