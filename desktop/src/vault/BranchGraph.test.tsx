import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from '../test-shim';
import type { BranchGraphData } from '../lib/api';
import BranchGraph from './BranchGraph';

afterEach(cleanup);

const data: BranchGraphData = {
  branches: [
    { id: 'main', name: 'main', parent: null, head_commit: 'm2', lane: 0, current: true },
    { id: 'feat', name: 'feature', parent: 'main', head_commit: 'f1', lane: 1, current: false },
  ],
  nodes: [
    { commit_id: 'm1', branch_id: 'main', parents: [], ts: 1, lamport: 1, label: 'a.md', lane: 0 },
    { commit_id: 'm2', branch_id: 'main', parents: ['m1'], ts: 3, lamport: 10, label: 'a.md', lane: 0 },
    { commit_id: 'f1', branch_id: 'feat', parents: ['m1'], ts: 2, lamport: 5, label: 'b.md', lane: 1 },
  ],
};

describe('BranchGraph', () => {
  it('renders a lane per branch and a node per commit', () => {
    render(<BranchGraph data={data} accent="#3d63dd" current="main" onCheckout={vi.fn()} />);
    expect(screen.getByTestId('branch-lane-main')).toBeTruthy();
    expect(screen.getByTestId('branch-lane-feature')).toBeTruthy();
    expect(screen.getAllByTestId('branch-commit')).toHaveLength(3);
  });

  it('checks out a branch when its lane label is clicked', () => {
    const onCheckout = vi.fn();
    render(<BranchGraph data={data} accent="#3d63dd" current="main" onCheckout={onCheckout} />);
    fireEvent.click(screen.getByTestId('branch-lane-feature'));
    expect(onCheckout).toHaveBeenCalledWith('feat');
  });

  it('shows an empty state when there are no commits', () => {
    render(<BranchGraph data={{ nodes: [], branches: data.branches }} accent="#3d63dd" current="main" onCheckout={vi.fn()} />);
    expect(screen.getByTestId('branch-graph-empty')).toBeTruthy();
  });
});
