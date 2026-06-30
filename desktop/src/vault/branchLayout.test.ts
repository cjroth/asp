import { describe, expect, it } from '../test-shim';
import type { BranchGraphData } from '../lib/api';
import { DEFAULT_LAYOUT, laneColor, layoutGraph } from './branchLayout';

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

describe('layoutGraph', () => {
  it('places commits left→right by time, one row per lane', () => {
    const g = layoutGraph(data);
    expect(g.placed).toHaveLength(3);
    const byId = Object.fromEntries(g.placed.map((p) => [p.node.commit_id, p]));
    // x increases with lamport (m1 < f1 < m2).
    expect(byId.m1.x).toBeLessThan(byId.f1.x);
    expect(byId.f1.x).toBeLessThan(byId.m2.x);
    // lane → row (y).
    expect(byId.m1.y).toBe(DEFAULT_LAYOUT.padY);
    expect(byId.f1.y).toBe(DEFAULT_LAYOUT.padY + DEFAULT_LAYOUT.rowH);
    expect(g.laneCount).toBe(2);
  });

  it('emits a same-lane chain edge and a cross-lane fork edge', () => {
    const g = layoutGraph(data);
    // m1→m2 same lane (not crossing); m1→f1 crosses lanes (the fork).
    const chain = g.edges.find((e) => !e.cross);
    const fork = g.edges.find((e) => e.cross);
    expect(chain).toBeTruthy();
    expect(fork).toBeTruthy();
    expect(fork!.lane).toBe(1); // coloured by the child (feature) lane
  });

  it('handles an empty graph without throwing', () => {
    const g = layoutGraph({ nodes: [], branches: [{ id: 'main', name: 'main', parent: null, head_commit: null, lane: 0, current: true }] });
    expect(g.placed).toHaveLength(0);
    expect(g.edges).toHaveLength(0);
  });
});

describe('laneColor', () => {
  it('main uses the accent; others are distinct', () => {
    expect(laneColor(0, '#abc')).toBe('#abc');
    expect(laneColor(1, '#abc')).not.toBe('#abc');
    expect(laneColor(1, '#abc')).not.toBe(laneColor(2, '#abc'));
  });
});
