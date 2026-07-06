import { mock } from 'bun:test';
// Git-bridge §7.2 connect-modal wiring: pasting a git URL swaps the Access-key
// field for a Token field and routes submit to api.cloneGit; an ordinary ASP
// ticket keeps the access key and routes to api.cloneRemote. Runs in web mode
// (no Tauri) so the modal needs no destination folder.
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from './test-shim';

const cloneGit = vi.fn(async () => ({ id: 'g1', path: '', vault_id: 'gv1', enabled: true, listening_ticket: null }));
const cloneRemote = vi.fn(async () => ({ id: 'r1', path: '', vault_id: 'rv1', enabled: true, listening_ticket: null }));

mock.module('@tauri-apps/plugin-dialog', () => ({ open: vi.fn(async () => null) }));
mock.module('./lib/api', () => ({
  api: {
    startLiveSync: vi.fn(), stopLiveSync: vi.fn(),
    listVaults: vi.fn(async () => []),
    getIdentity: vi.fn(async () => 'ssh-ed25519 WEBKEY me@browser'),
    getStatus: vi.fn(async (id: string) => ({ id, vault_id: 'gv1', rows: 0, files: 0, head: '', listening_ticket: null, peers: [], last_ts: null })),
    gitStatus: vi.fn(async () => null),
    listFiles: vi.fn(async () => []),
    history: vi.fn(async () => []),
    listBranches: vi.fn(async () => [{ branch_id: 'main', name: 'main', parent: null, current: true }]),
    currentBranch: vi.fn(async () => 'main'),
    branchGraph: vi.fn(async () => ({ nodes: [], branches: [], tags: [] })),
    readFile: vi.fn(async () => ''),
    writeFile: vi.fn(),
    removeVault: vi.fn(),
    createVault: vi.fn(),
    addLocalFolder: vi.fn(),
    cloneGit: (...a: unknown[]) => (cloneGit as (...x: unknown[]) => unknown)(...a),
    cloneRemote: (...a: unknown[]) => (cloneRemote as (...x: unknown[]) => unknown)(...a),
  },
}));

import App from './App';

const w = window as unknown as Record<string, unknown>;
beforeEach(() => {
  vi.clearAllMocks();
  delete w.__TAURI_INTERNALS__;
  delete w.__TAURI__;
  localStorage.clear();
});
afterEach(() => { cleanup(); w.__TAURI_INTERNALS__ = {}; });

async function openConnect() {
  render(<App />);
  fireEvent.click(await screen.findByText('Connect Vault'));
  return screen.getByPlaceholderText(/Paste an invite code/);
}

const GIT_URL = 'https://github.com/octo/repo.git';
// A representative ASP peer ticket / node id (no scheme, no scp colon, no .git).
const TICKET = 'a'.repeat(64);

describe('connect modal — git URL detection (git-bridge §7.2)', () => {
  it('swaps the Access-key field for a Token field when a git URL is typed', async () => {
    const box = await openConnect();
    // A plain box shows the Access-key field, no Token field.
    expect(screen.getByText(/Access key/)).toBeTruthy();
    expect(screen.queryByTestId('git-token-field')).toBeNull();

    fireEvent.change(box, { target: { value: GIT_URL } });

    await waitFor(() => expect(screen.getByTestId('git-token-field')).toBeTruthy());
    expect(screen.queryByText(/Access key/)).toBeNull();
    expect(screen.getByText('Token')).toBeTruthy();
  });

  it('routes a git URL (with token + depth) to api.cloneGit, not cloneRemote', async () => {
    const box = await openConnect();
    fireEvent.change(box, { target: { value: GIT_URL } });
    await screen.findByTestId('git-token-field');

    fireEvent.change(screen.getByPlaceholderText(/Personal access token/), { target: { value: 'ghp_secret' } });
    // Reveal the Advanced disclosure and set a shallow depth.
    fireEvent.click(screen.getByText('Advanced'));
    fireEvent.change(await screen.findByPlaceholderText('e.g. 50'), { target: { value: '25' } });

    fireEvent.click(screen.getByRole('button', { name: 'Connect' }));

    await waitFor(() => expect(cloneGit).toHaveBeenCalled());
    expect(cloneGit).toHaveBeenCalledWith('', GIT_URL, 'ghp_secret', 25, expect.any(Function));
    expect(cloneRemote).not.toHaveBeenCalled();
  });

  it('routes an ordinary ASP ticket to api.cloneRemote, not cloneGit', async () => {
    const box = await openConnect();
    fireEvent.change(box, { target: { value: TICKET } });
    // No Token field for a non-git input.
    expect(screen.queryByTestId('git-token-field')).toBeNull();

    fireEvent.click(screen.getByRole('button', { name: 'Connect' }));

    await waitFor(() => expect(cloneRemote).toHaveBeenCalled());
    expect(cloneRemote).toHaveBeenCalledWith('', TICKET, undefined, expect.any(Function));
    expect(cloneGit).not.toHaveBeenCalled();
  });

  it('shows the SSH-agent note (no token field) for an ssh git URL', async () => {
    const box = await openConnect();
    fireEvent.change(box, { target: { value: 'git@github.com:octo/repo.git' } });
    // On web an ssh URL is unsupported — the note says so and submit is blocked.
    const note = await screen.findByTestId('git-ssh-note');
    expect(note.textContent).toMatch(/SSH clone isn’t supported in the browser/);
    expect(screen.queryByTestId('git-token-field')).toBeNull();
  });
});

import { afterAll as __aa, mock as __mk } from 'bun:test';
__aa(() => __mk.restore());
