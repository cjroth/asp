import { describe, expect, it } from '../test-shim';
import type { BranchGraphData, GraphNode } from '../lib/api';
import type { TrackEvent } from './history';
import {
  branchSpans,
  declutterLabels,
  forkEdges,
  type LaneSpan,
  laneColor,
  laneCountOf,
  laneForEvent,
  laneGeometry,
  laneIndex,
  lanesFromGraph,
  packLanes,
  packedLanes,
} from './timeline';

const ev = (id: string, ts: number, branchId: string, kind = 'edit'): TrackEvent => ({ id, ts, kind, path: `${id}.md`, branchId });

const graph = (branches: BranchGraphData['branches']): BranchGraphData => ({ nodes: [], branches, tags: [] });

const span = (branchId: string, start: number, end: number, pinned = false): LaneSpan => ({ branchId, start, end, pinned });
// max lane index + 1 in a packed map (the number of display lanes it uses).
const laneCount = (m: Map<string, number>) => (m.size === 0 ? 0 : Math.max(...m.values()) + 1);

// Ground-truth minimum lanes for a set of spans under a given epsilon: the peak
// number of intervals overlapping at any instant, where each interval's tail is
// inflated by epsilon (half-open [start, end+eps)) so touching intervals may share.
const maxOverlap = (spans: LaneSpan[], eps: number): number => {
  const pts: { x: number; d: number }[] = [];
  for (const s of spans) { pts.push({ x: s.start, d: 1 }); pts.push({ x: s.end + eps, d: -1 }); }
  // At equal coordinate, process ends (-1) before starts (+1): a lane freed at t
  // is reusable by a span starting at t.
  pts.sort((a, b) => a.x - b.x || a.d - b.d);
  let cur = 0, mx = 0;
  for (const p of pts) { cur += p.d; if (cur > mx) mx = cur; }
  return mx;
};

const epsOf = (spans: LaneSpan[]) => {
  let mn = Infinity, mx = -Infinity;
  for (const s of spans) { if (s.start < mn) mn = s.start; if (s.end > mx) mx = s.end; }
  const range = mx - mn;
  return range > 0 ? range * 0.01 : 0;
};

const node = (commit_id: string, branch_id: string, ts: number): GraphNode =>
  ({ commit_id, branch_id, parents: [], ts, lamport: 1, label: commit_id, lane: 0 });

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

describe('packLanes — interval lane packing', () => {
  it('is empty for no spans', () => {
    expect(packLanes([]).size).toBe(0);
  });

  it('pins main to lane 0 and the current (pinned) branch to lane 1', () => {
    const m = packLanes([
      span('x', 0, 10),
      span('feat', 0, 10, true), // current
      span('main', 0, 10),
    ]);
    expect(m.get('main')).toBe(0);
    expect(m.get('feat')).toBe(1);
    // x overlaps both reserved lanes → it must open a third lane.
    expect(m.get('x')).toBe(2);
  });

  it('keeps main on lane 0 even when its span sorts last and it never shares', () => {
    const m = packLanes([
      span('a', 0, 5),
      span('b', 20, 25), // disjoint from a (gap well beyond epsilon)
      span('main', 100, 101), // far-future, would sort last
    ]);
    expect(m.get('main')).toBe(0);
    // a and b are disjoint non-main branches → they SHARE a single lane (1),
    // and never fall onto main's lane 0.
    expect(m.get('a')).toBe(1);
    expect(m.get('b')).toBe(1);
  });

  it('uses the minimal number of lanes == max concurrent overlap', () => {
    // a,b overlap (2 concurrent); c is disjoint and reuses a freed lane.
    const spans = [span('a', 0, 5), span('b', 1, 6), span('c', 100, 105)];
    const m = packLanes(spans);
    expect(laneCount(m)).toBe(maxOverlap(spans, epsOf(spans)));
    expect(laneCount(m)).toBe(2);
    // c reused the earliest-freed lane (a's).
    expect(m.get('c')).toBe(m.get('a'));
    expect(m.get('a')).not.toBe(m.get('b'));
  });

  it('never places overlapping (within epsilon) spans on the same lane', () => {
    const spans = [span('a', 0, 4), span('b', 2, 6), span('c', 3, 9), span('d', 100, 110), span('e', 101, 105)];
    const m = packLanes(spans);
    const eps = epsOf(spans);
    const byLane = new Map<number, LaneSpan[]>();
    for (const s of spans) {
      const l = m.get(s.branchId)!;
      (byLane.get(l) ?? byLane.set(l, []).get(l)!).push(s);
    }
    for (const list of byLane.values()) {
      list.sort((x, y) => x.start - y.start);
      for (let i = 1; i < list.length; i++) {
        expect(list[i - 1].end + eps).toBeLessThanOrEqual(list[i].start);
      }
    }
  });

  it('is deterministic regardless of input order', () => {
    const spans = [span('a', 0, 5), span('b', 1, 6), span('c', 7, 9), span('main', 2, 3), span('d', 8, 12, true)];
    const forward = packLanes(spans);
    const reversed = packLanes([...spans].reverse());
    const shuffled = packLanes([spans[2], spans[0], spans[4], spans[1], spans[3]]);
    expect([...reversed.entries()].sort()).toEqual([...forward.entries()].sort());
    expect([...shuffled.entries()].sort()).toEqual([...forward.entries()].sort());
  });

  it('handles zero-length spans: same instant → separate lanes, later → reuse', () => {
    // Range > 0 (so epsilon > 0): two zero-length spans at the same instant cannot
    // share (epsilon separates them); a later one reuses a freed lane.
    const spans = [span('a', 0, 0), span('b', 0, 0), span('c', 100, 100)];
    const m = packLanes(spans);
    expect(m.get('a')).not.toBe(m.get('b'));
    expect([m.get('a'), m.get('b')]).toContain(m.get('c'));
  });

  it('respects an explicit epsilon that forces spacing', () => {
    // Without epsilon these back-to-back spans would share; a large epsilon forbids it.
    const spans = [span('a', 0, 5), span('b', 5, 10)];
    expect(laneCount(packLanes(spans, { epsilon: 0 }))).toBe(1);
    expect(laneCount(packLanes(spans, { epsilon: 1 }))).toBe(2);
  });

  it('fuzz (seeded LCG): invariants + determinism over random span sets', () => {
    let seed = 0x1234_5678;
    const rnd = () => ((seed = (seed * 1_103_515_245 + 12_345) & 0x7fff_ffff) / 0x7fff_ffff);
    for (let iter = 0; iter < 200; iter++) {
      const n = 1 + Math.floor(rnd() * 24);
      // Non-main, non-pinned spans so the greedy result is exactly max-overlap.
      const spans: LaneSpan[] = Array.from({ length: n }, (_, i) => {
        const start = Math.floor(rnd() * 1000);
        return span(`s${i}`, start, start + Math.floor(rnd() * 200));
      });
      const eps = epsOf(spans);
      const m = packLanes(spans);
      // (1) exact lower bound: lane count == peak overlap (and ≤ naive one-per-branch).
      expect(laneCount(m)).toBe(maxOverlap(spans, eps));
      expect(laneCount(m)).toBeLessThanOrEqual(n);
      // (2) no lane holds two epsilon-overlapping spans.
      const byLane = new Map<number, LaneSpan[]>();
      for (const s of spans) (byLane.get(m.get(s.branchId)!) ?? byLane.set(m.get(s.branchId)!, []).get(m.get(s.branchId)!)!).push(s);
      for (const list of byLane.values()) {
        list.sort((x, y) => x.start - y.start);
        for (let i = 1; i < list.length; i++) expect(list[i - 1].end + eps).toBeLessThanOrEqual(list[i].start);
      }
      // (3) determinism: identical output when the same input is shuffled.
      const shuffled = [...spans].sort(() => rnd() - 0.5);
      expect([...packLanes(shuffled).entries()].sort()).toEqual([...m.entries()].sort());
    }
  });
});

describe('branchSpans — activity windows from graph nodes', () => {
  it('an open branch ends at its LAST COMMIT (never "now")', () => {
    const g: BranchGraphData = {
      nodes: [node('m1', 'main', 100), node('m2', 'main', 200), node('f1', 'feat', 150), node('f2', 'feat', 180)],
      branches: [
        { id: 'main', name: 'main', parent: null, head_commit: 'm2', lane: 0, current: false },
        { id: 'feat', name: 'feat', parent: 'main', head_commit: 'f2', lane: 1, current: true },
      ],
      tags: [],
    };
    const spans = branchSpans(g);
    const feat = spans.find((s) => s.branchId === 'feat')!;
    // ms conversion, ends at last commit (180s → 180000ms), well below Date.now().
    expect(feat.start).toBe(150_000);
    expect(feat.end).toBe(180_000);
    expect(feat.end).toBeLessThan(Date.now());
    expect(feat.pinned).toBe(true); // current → pinned
  });

  it('a branch with no nodes in the capped graph gets a zero-length span (head time, else floor)', () => {
    const g: BranchGraphData = {
      nodes: [node('m1', 'main', 100), node('m2', 'main', 200)],
      branches: [
        { id: 'main', name: 'main', parent: null, head_commit: 'm2', lane: 0, current: true },
        { id: 'atHead', name: 'atHead', parent: 'main', head_commit: 'm1', lane: 1, current: false }, // head resolves to a node
        { id: 'ghost', name: 'ghost', parent: 'main', head_commit: null, lane: 2, current: false }, // nothing to anchor
      ],
      tags: [],
    };
    const spans = branchSpans(g);
    const atHead = spans.find((s) => s.branchId === 'atHead')!;
    expect(atHead.start).toBe(atHead.end); // zero-length
    expect(atHead.start).toBe(100_000); // m1's time
    const ghost = spans.find((s) => s.branchId === 'ghost')!;
    expect(ghost.start).toBe(ghost.end);
    expect(ghost.start).toBe(100_000); // graph floor (earliest node), not distorting the range
  });

  it('packedLanes ignores the Rust creation-order lane and packs disjoint branches together', () => {
    // Two feature branches active in disjoint windows → they SHARE one display lane,
    // even though Rust handed them lanes 1 and 2.
    const g: BranchGraphData = {
      nodes: [
        node('m1', 'main', 0), node('m2', 'main', 5),
        node('a1', 'a', 10), node('a2', 'a', 20),
        node('b1', 'b', 100), node('b2', 'b', 110),
      ],
      branches: [
        { id: 'main', name: 'main', parent: null, head_commit: 'm2', lane: 0, current: true },
        { id: 'a', name: 'a', parent: 'main', head_commit: 'a2', lane: 1, current: false },
        { id: 'b', name: 'b', parent: 'main', head_commit: 'b2', lane: 2, current: false },
      ],
      tags: [],
    };
    const lanes = packedLanes(g);
    const byId = new Map(lanes.map((l) => [l.branchId, l.lane]));
    expect(byId.get('main')).toBe(0);
    expect(byId.get('a')).toBe(byId.get('b')); // packed onto the same display lane
    expect(laneCountOf(lanes)).toBe(2); // main + one shared feature lane (was 3)
  });
});

describe('declutterLabels — pixel-space collision pass', () => {
  const box = (id: string, x: number, priority: number) => ({ id, x, y: 0, w: 30, h: 18, priority });

  it('keeps every label when none overlap', () => {
    const keep = declutterLabels([box('a', 0, 1), box('b', 100, 1), box('c', 200, 1)]);
    expect(keep).toEqual(new Set(['a', 'b', 'c']));
  });

  it('higher priority wins a collision', () => {
    // Two boxes at the same spot: the higher-priority one is kept, the other dropped.
    const keep = declutterLabels([box('lo', 10, 1), box('hi', 12, 5)]);
    expect(keep.has('hi')).toBe(true);
    expect(keep.has('lo')).toBe(false);
  });

  it('thins a dense overlapping chain to a non-overlapping subset', () => {
    // 10 chips 8px apart (each 30px wide) → heavily overlapping; only a few survive.
    const items = Array.from({ length: 10 }, (_, i) => box(`c${i}`, i * 8, 10 - i));
    const keep = declutterLabels(items);
    expect(keep.size).toBeGreaterThan(0);
    expect(keep.size).toBeLessThan(10);
    // Kept boxes must be pairwise non-overlapping (with the 2px pad).
    const kept = items.filter((b) => keep.has(b.id)).sort((a, b) => a.x - b.x);
    for (let i = 1; i < kept.length; i++) {
      // non-overlap with the 2px pad on both sides.
      expect(kept[i - 1].x + kept[i - 1].w + 2).toBeLessThanOrEqual(kept[i].x - 2);
    }
  });
});
