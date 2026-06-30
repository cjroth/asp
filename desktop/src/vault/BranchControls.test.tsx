import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, mock, vi } from '../test-shim';
import type { BranchInfo } from '../lib/api';

// A small in-memory backend so the switcher exercises real create/switch/delete.
let branches: BranchInfo[] = [];
const calls: Record<string, unknown[][]> = { create: [], checkout: [], del: [], graph: [] };
function reset() {
  branches = [{ branch_id: 'main', name: 'main', parent: null, current: true }];
  for (const k of Object.keys(calls)) calls[k] = [];
}
reset();

const apiMock = {
  listBranches: vi.fn(async () => branches.map((b) => ({ ...b }))),
  createBranch: vi.fn(async (_id: string, name: string) => {
    calls.create.push([name]);
    const branch_id = `id-${name}`;
    branches.push({ branch_id, name, parent: 'main', current: false });
    return branch_id;
  }),
  checkoutBranch: vi.fn(async (_id: string, bid: string) => {
    calls.checkout.push([bid]);
    branches = branches.map((b) => ({ ...b, current: b.branch_id === bid }));
  }),
  deleteBranch: vi.fn(async (_id: string, bid: string) => {
    calls.del.push([bid]);
    branches = branches.filter((b) => b.branch_id !== bid).map((b) => ({ ...b, current: b.branch_id === 'main' || b.current }));
  }),
  branchGraph: vi.fn(async () => {
    calls.graph.push([]);
    return { nodes: [{ commit_id: 'c1', branch_id: 'main', parents: [], ts: 1, lamport: 1, label: 'a.md', lane: 0 }], branches: [{ id: 'main', name: 'main', parent: null, head_commit: 'c1', lane: 0, current: true }] };
  }),
};
mock.module('../lib/api', () => ({ api: apiMock }));

// Imported AFTER the mock is registered.
import BranchControls from './BranchControls';

afterEach(() => { cleanup(); reset(); });

const onChanged = vi.fn();
const renderCtl = () => render(<BranchControls vaultId="v1" accent="#3d63dd" onChanged={onChanged} />);

describe('BranchControls', () => {
  it('shows the current branch and lists branches in the menu', async () => {
    branches.push({ branch_id: 'id-x', name: 'feature-x', parent: 'main', current: false });
    renderCtl();
    await waitFor(() => expect(screen.getByTestId('branch-switcher').textContent).toContain('main'));
    fireEvent.click(screen.getByTestId('branch-switcher'));
    expect(screen.getByTestId('branch-row-main')).toBeTruthy();
    expect(screen.getByTestId('branch-row-feature-x')).toBeTruthy();
  });

  it('creates a branch from here and switches to it', async () => {
    renderCtl();
    await waitFor(() => screen.getByTestId('branch-switcher'));
    fireEvent.click(screen.getByTestId('branch-switcher'));
    fireEvent.click(screen.getByTestId('branch-new'));
    fireEvent.change(screen.getByTestId('branch-name-input'), { target: { value: 'topic' } });
    fireEvent.click(screen.getByTestId('branch-create-confirm'));
    await waitFor(() => expect(calls.create).toEqual([['topic']]));
    // create-and-switch: checkout was called with the new id, and files reloaded.
    await waitFor(() => expect(calls.checkout.at(-1)).toEqual(['id-topic']));
    expect(onChanged).toHaveBeenCalled();
  });

  it('switches branches on row click', async () => {
    branches.push({ branch_id: 'id-y', name: 'feat-y', parent: 'main', current: false });
    renderCtl();
    await waitFor(() => screen.getByTestId('branch-switcher'));
    fireEvent.click(screen.getByTestId('branch-switcher'));
    fireEvent.click(screen.getByTestId('branch-row-feat-y'));
    await waitFor(() => expect(calls.checkout).toEqual([['id-y']]));
    expect(onChanged).toHaveBeenCalled();
  });

  it('deletes a non-main branch (and main has no delete affordance)', async () => {
    branches.push({ branch_id: 'id-z', name: 'feat-z', parent: 'main', current: false });
    renderCtl();
    await waitFor(() => screen.getByTestId('branch-switcher'));
    fireEvent.click(screen.getByTestId('branch-switcher'));
    expect(screen.queryByTestId('branch-delete-main')).toBeNull();
    fireEvent.click(screen.getByTestId('branch-delete-feat-z'));
    await waitFor(() => expect(calls.del).toEqual([['id-z']]));
  });

  it('opens the network graph modal', async () => {
    renderCtl();
    await waitFor(() => screen.getByTestId('branch-switcher'));
    fireEvent.click(screen.getByTestId('branch-switcher'));
    fireEvent.click(screen.getByTestId('branch-graph-open'));
    await waitFor(() => expect(screen.getByTestId('branch-graph-modal')).toBeTruthy());
    expect(screen.getByTestId('branch-graph')).toBeTruthy();
    expect(calls.graph.length).toBe(1);
  });
});
