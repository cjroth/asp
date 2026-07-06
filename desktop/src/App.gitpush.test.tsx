import { mock } from 'bun:test';
// Git-bridge §7.2 "Commit & push to git" desktop surface. The dialog's decision +
// action logic is factored into pure helpers (`canPushGit`, `gitPushSummary`,
// `runGitPush`) so this test exercises the real code paths WITHOUT the heavy full
// App render (jsdom + 5.8MB wasm) — the VM OOMs on that. We assert: the push
// affordance is gated to a desktop git vault (absent on web / non-git); the commit
// message is pre-filled from the pending diff; and confirming calls
// `gitPush(activeId, message)` then refreshes the status, with typed failures
// (frozen, nothing-to-commit) mapped to friendly copy.
import { describe, expect, it, vi } from './test-shim';

// App.tsx pulls `open` from the Tauri dialog plugin at import; stub it so importing
// the module (for its helpers) never touches the real plugin.
mock.module('@tauri-apps/plugin-dialog', () => ({ open: vi.fn(async () => null) }));

const { canPushGit, gitPushSummary, runGitPush } = await import('./App');

const STATUS = { remoteUrl: 'https://example.com/r.git', atSha: 'deadbeefcafe', frozen: false, ahead: 0, behind: 0, policy: 'manual' };

describe('canPushGit — the push affordance gate', () => {
  it('shows for a git vault in the desktop shell', () => {
    expect(canPushGit(STATUS, true)).toBe(true);
  });
  it('is hidden on a non-git vault (no git status → button absent)', () => {
    expect(canPushGit(null, true)).toBe(false);
  });
  it('is hidden on web (browser cannot push — spec non-goal)', () => {
    expect(canPushGit(STATUS, false)).toBe(false);
  });
});

describe('gitPushSummary — pre-filled commit message', () => {
  it('summarizes the changed count + first paths', () => {
    const msg = gitPushSummary({ filesChanged: 2, paths: ['a.md', 'b.md'] });
    expect(msg).toBe('asp: 2 file(s) changed (a.md, b.md)');
  });
  it('elides after three paths', () => {
    const msg = gitPushSummary({ filesChanged: 4, paths: ['a', 'b', 'c', 'd'] });
    expect(msg).toBe('asp: 4 file(s) changed (a, b, c…)');
  });
  it('is empty for a clean tree (nothing to commit)', () => {
    expect(gitPushSummary({ filesChanged: 0, paths: [] })).toBe('');
  });
});

describe('runGitPush — confirm action', () => {
  it('calls gitPush(id, message), then refreshes the status', async () => {
    const gitPush = vi.fn(async () => ({ pushedSha: 'abc1234feedface', commits: 1 }));
    const gitStatus = vi.fn(async () => ({ ...STATUS, atSha: 'abc1234feedface' }));
    const out = await runGitPush({ gitPush, gitStatus }, 'vault-1', 'my message');
    expect(gitPush).toHaveBeenCalledWith('vault-1', 'my message');
    expect(gitStatus).toHaveBeenCalledWith('vault-1');
    expect(out.ok).toBe(true);
    expect(out.pushedSha).toBe('abc1234feedface');
    expect(out.commits).toBe(1);
    expect(out.status?.atSha).toBe('abc1234feedface');
  });

  it('reports a friendly "nothing to commit" when the push sends 0 commits', async () => {
    const gitPush = vi.fn(async () => ({ pushedSha: null, commits: 0 }));
    const gitStatus = vi.fn(async () => STATUS);
    const out = await runGitPush({ gitPush, gitStatus }, 'vault-1', 'noop');
    expect(out.ok).toBe(false);
    expect(out.error).toMatch(/nothing to commit/i);
    expect(gitStatus).not.toHaveBeenCalled();
  });

  it('maps a frozen-remote error to the rebaseline hint', async () => {
    const gitPush = vi.fn(async () => { throw new Error('git push: remote is frozen (upstream history was rewritten) — run `asp git rebaseline` first'); });
    const gitStatus = vi.fn(async () => STATUS);
    const out = await runGitPush({ gitPush, gitStatus }, 'vault-1', 'msg');
    expect(out.ok).toBe(false);
    expect(out.error).toBe('History was rewritten upstream — run rebaseline.');
  });
});

import { afterAll as __aa, mock as __mk } from 'bun:test';
__aa(() => __mk.restore());
