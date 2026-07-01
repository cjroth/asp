import { describe, expect, it } from '../test-shim';
import type { BranchGraphData } from '../lib/api';
import type { TrackEvent } from './history';
import { forkEdges, laneColor, laneForEvent, laneGeometry, laneIndex, lanesFromGraph } from './timeline';

const ev = (id: string, ts: number, branchId: string, kind = 'edit'): TrackEvent => ({ id, ts, kind, path: `${id}.md`, branchId });

const graph = (branches: BranchGraphData['branches']): BranchGraphData => ({ nodes: [], branches, tags: [] });

describe('timeline layout', () => {
  it('falls back to a single main lane when there is no graph', () => {
    const lanes = lanesFromGraph(null);
    expect(lanes).toHaveLength(1);
    expect(lanes[0]).toMatchObject({ branchId: 'main', lane: 0, current: true });
  });

  it('orders lanes by lane index from the graph', () => {
    const g = graph([
      { id: 'feat', name: 'feature', parent: 'main', head_commit: null, lane: 1, current: true },
      { id: 'main', name: 'main', parent: null, head_commit: null, lane: 0, current: false },
    ]);
    const lanes = lanesFromGraph(g);
    expect(lanes.map((l) => l.branchId)).toEqual(['main', 'feat']);
    expect(lanes[1].current).toBe(true);
  });

  it('places an event on its own branch lane, unknown → main (0)', () => {
    const lanes = lanesFromGraph(graph([
      { id: 'main', name: 'main', parent: null, head_commit: null, lane: 0, current: true },
      { id: 'feat', name: 'feature', parent: 'main', head_commit: null, lane: 1, current: false },
    ]));
    const idx = laneIndex(lanes);
    expect(laneForEvent(ev('a', 100, 'feat'), idx)).toBe(1);
    expect(laneForEvent(ev('b', 100, 'main'), idx)).toBe(0);
    expect(laneForEvent(ev('c', 100, 'ghost'), idx)).toBe(0);
  });

  it('draws a fork edge from the parent lane at the branch’s earliest event', () => {
    const lanes = lanesFromGraph(graph([
      { id: 'main', name: 'main', parent: null, head_commit: null, lane: 0, current: false },
      { id: 'feat', name: 'feature', parent: 'main', head_commit: null, lane: 1, current: true },
    ]));
    const events = [ev('m1', 100, 'main'), ev('f2', 300, 'feat'), ev('f1', 200, 'feat')];
    const edges = forkEdges(lanes, events);
    expect(edges).toHaveLength(1);
    expect(edges[0]).toMatchObject({ branchId: 'feat', fromLane: 0, toLane: 1, ts: 200 });
  });

  it('skips fork edges for branches with no events yet, and for main', () => {
    const lanes = lanesFromGraph(graph([
      { id: 'main', name: 'main', parent: null, head_commit: null, lane: 0, current: true },
      { id: 'empty', name: 'empty', parent: 'main', head_commit: null, lane: 1, current: false },
    ]));
    expect(forkEdges(lanes, [ev('m1', 100, 'main')])).toHaveLength(0);
  });

  it('lane geometry centres a single lane and stacks multiple within bounds', () => {
    const one = laneGeometry(1, 80);
    expect(one.y(0)).toBeCloseTo(40, 0); // centred
    const three = laneGeometry(3, 90);
    expect(three.rowH).toBeGreaterThanOrEqual(16);
    expect(three.y(0)).toBeLessThan(three.y(1));
    expect(three.y(1)).toBeLessThan(three.y(2));
    expect(three.y(2)).toBeLessThanOrEqual(90);
  });

  it('main is the accent colour; other lanes cycle a distinct palette', () => {
    expect(laneColor(0, '#abc')).toBe('#abc');
    expect(laneColor(1, '#abc')).not.toBe('#abc');
    expect(laneColor(1, '#abc')).not.toBe(laneColor(2, '#abc'));
  });
});
