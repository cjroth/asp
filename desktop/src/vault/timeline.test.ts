import { describe, expect, it } from '../test-shim';
import type { BranchGraphData, GraphNode } from '../lib/api';
import type { TrackEvent } from './history';
import {
  branchSpans,
  type BranchGroup,
  declutterLabels,
  FISHEYE_PIN_MIN,
  FISHEYE_THRESHOLD,
  fisheyeLaneYs,
  forkEdges,
  groupBranches,
  groupSpanId,
  IDEAL_ROW,
  type LaneSpan,
  laneColor,
  laneCountOf,
  laneForEvent,
  laneGeometry,
  laneIndex,
  lanesFromGraph,
  packLanes,
  packedLanes,
  packedLanesGrouped,
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

describe('groupBranches — prefix accordion (wave C)', () => {
  const gb = (id: string, name: string, current = false) =>
    ({ id, name, parent: id === 'main' ? null : 'main', head_commit: `${id}-h`, lane: 0, current });
  // A graph where each branch has ONE node (a zero-length span at its ts seconds).
  const farm = (branches: BranchGraphData['branches'], ts: Record<string, number>): BranchGraphData => ({
    branches,
    nodes: Object.entries(ts).map(([bid, t]) => node(`${bid}-n`, bid, t)),
    tags: [],
  });

  it('groups branches sharing a first path segment when >= minGroupSize (default 3)', () => {
    const g = farm(
      [gb('main', 'main', true), gb('da', 'dependabot/npm/a'), gb('db', 'dependabot/npm/b'), gb('dc', 'dependabot/pip/c')],
      { main: 0, da: 10, db: 20, dc: 30 },
    );
    const groups = groupBranches(g);
    expect(groups).toHaveLength(1);
    expect(groups[0].prefix).toBe('dependabot');
    expect(groups[0].branchIds).toEqual(['da', 'db', 'dc']); // members sorted by id
    // Union span = min start … max end over members (seconds → ms), keyed by group id.
    expect(groups[0].span).toMatchObject({ branchId: groupSpanId('dependabot'), start: 10_000, end: 30_000, pinned: false });
  });

  it('honours minGroupSize: a below-threshold prefix stays individual', () => {
    const g = farm([gb('main', 'main', true), gb('da', 'cursor/a'), gb('db', 'cursor/b')], { main: 0, da: 1, db: 2 });
    expect(groupBranches(g)).toHaveLength(0); // 2 members < 3
    expect(groupBranches(g, { minGroupSize: 2 })).toHaveLength(1); // opt-in lowers the bar
  });

  it('never groups main or the current branch (a pin removed can drop a group below threshold)', () => {
    const g = farm(
      [gb('main', 'main', false), gb('da', 'dependabot/a'), gb('db', 'dependabot/b'), gb('dc', 'dependabot/c', true)],
      { main: 0, da: 10, db: 20, dc: 30 },
    );
    // dc is current → excluded → only da,db remain (2 < 3) → no group.
    expect(groupBranches(g)).toHaveLength(0);
    // main itself, even with a "main/…"-shaped id, is never a member.
    const g2 = farm(
      [gb('main', 'main', true), gb('da', 'dependabot/a'), gb('db', 'dependabot/b'), gb('dc', 'dependabot/c')],
      { main: 0, da: 10, db: 20, dc: 30 },
    );
    expect(groupBranches(g2)[0].branchIds).toEqual(['da', 'db', 'dc']); // main absent
  });

  it('leaves slash-less branches as ungrouped singletons', () => {
    const g = farm(
      [gb('main', 'main', true), gb('a', 'cursor/a'), gb('b', 'cursor/b'), gb('c', 'cursor/c'), gb('r', 'readme')],
      { main: 0, a: 1, b: 2, c: 3, r: 4 },
    );
    const groups = groupBranches(g);
    expect(groups).toHaveLength(1);
    expect(groups[0].prefix).toBe('cursor');
    expect(groups[0].branchIds).not.toContain('r'); // no '/' → singleton
  });

  it('is deterministic: groups sorted by prefix', () => {
    const g = farm(
      [
        gb('main', 'main', true),
        gb('c1', 'cursor/1'), gb('c2', 'cursor/2'), gb('c3', 'cursor/3'),
        gb('d1', 'dependabot/1'), gb('d2', 'dependabot/2'), gb('d3', 'dependabot/3'),
      ],
      { main: 0, c1: 1, c2: 2, c3: 3, d1: 4, d2: 5, d3: 6 },
    );
    expect(groupBranches(g).map((x) => x.prefix)).toEqual(['cursor', 'dependabot']);
  });
});

describe('packedLanesGrouped — accordion re-pack composition (wave C)', () => {
  const gb = (id: string, name: string, current = false) =>
    ({ id, name, parent: id === 'main' ? null : 'main', head_commit: `${id}-h`, lane: 0, current });
  // Every branch active over the SAME window → nothing packs away (worst case).
  const overlapGraph = (extra: { id: string; name: string }[]): BranchGraphData => {
    const branches = [gb('main', 'main', true), ...extra.map((e) => gb(e.id, e.name))];
    const nodes = branches.flatMap((b) => [node(`${b.id}-s`, b.id, 100), node(`${b.id}-e`, b.id, 200)]);
    return { branches, nodes, tags: [] };
  };
  const g3 = () => overlapGraph([
    { id: 'd1', name: 'dep/1' }, { id: 'd2', name: 'dep/2' }, { id: 'd3', name: 'dep/3' },
    { id: 's1', name: 'solo1' }, { id: 's2', name: 'solo2' },
  ]);

  it('collapsed group costs ONE lane; members fold onto it (fewer lanes than ungrouped)', () => {
    const g = g3();
    const ungrouped = laneCountOf(packedLanes(g)); // main + 5 overlapping = 6
    const groups = groupBranches(g);
    const layout = packedLanesGrouped(g, groups, new Set()); // dep collapsed
    // main + dep(1) + solo1 + solo2 = 4 lanes < 6.
    expect(layout.laneCount).toBeLessThan(ungrouped);
    // All three dep members share one display lane (the group's).
    const dl = new Set(['d1', 'd2', 'd3'].map((id) => layout.laneOfBranch.get(id)));
    expect(dl.size).toBe(1);
    // They're folded out of the individually-rendered `lanes`, and marked as members.
    expect(layout.lanes.map((l) => l.branchId)).not.toContain('d1');
    expect(layout.memberGroup.get('d1')).toBe('dep');
    expect(layout.groupLanes.map((gl) => gl.prefix)).toEqual(['dep']);
  });

  it('expanding a group restores its members as individual overlapping lanes', () => {
    const g = g3();
    const groups = groupBranches(g);
    const expanded = packedLanesGrouped(g, groups, new Set(['dep']));
    // dep expanded → its 3 members overlap → 3 distinct lanes again.
    const dl = new Set(['d1', 'd2', 'd3'].map((id) => expanded.laneOfBranch.get(id)));
    expect(dl.size).toBe(3);
    expect(expanded.memberGroup.size).toBe(0); // nothing folded
    expect(expanded.lanes.map((l) => l.branchId)).toEqual(
      expect.arrayContaining(['d1', 'd2', 'd3']),
    );
  });

  it('all-expanded (or no-group) layout is identical to packedLanes — regression guard', () => {
    const g = g3();
    const groups = groupBranches(g);
    const allExpanded = packedLanesGrouped(g, groups, new Set(groups.map((x) => x.prefix)));
    const base = packedLanes(g);
    // Same lane per branch, same count, same individual lane list.
    for (const l of base) expect(allExpanded.laneOfBranch.get(l.branchId)).toBe(l.lane);
    expect(allExpanded.laneCount).toBe(laneCountOf(base));
    expect(allExpanded.groupLanes).toHaveLength(0);
    expect([...allExpanded.lanes].map((l) => [l.branchId, l.lane]).sort())
      .toEqual(base.map((l) => [l.branchId, l.lane]).sort());
  });

  it('main and current keep their pins regardless of grouping', () => {
    const branches = [
      gb('main', 'main'),
      gb('cur', 'feature/x', true),
      gb('d1', 'dep/1'), gb('d2', 'dep/2'), gb('d3', 'dep/3'),
    ];
    const nodes = branches.flatMap((b) => [node(`${b.id}-s`, b.id, 100), node(`${b.id}-e`, b.id, 200)]);
    const g: BranchGraphData = { branches, nodes, tags: [] };
    const groups = groupBranches(g);
    const layout = packedLanesGrouped(g, groups, new Set());
    expect(layout.laneOfBranch.get('main')).toBe(0);
    expect(layout.laneOfBranch.get('cur')).toBe(1);
  });

  it('fuzz (seeded LCG): random farms + random expansion never overlap on a lane; deterministic', () => {
    let seed = 0x0c0f_fee1;
    const rnd = () => ((seed = (seed * 1_103_515_245 + 12_345) & 0x7fff_ffff) / 0x7fff_ffff);
    const ser = (m: Map<string, number>) => [...m.entries()].sort().map((e) => e.join(':')).join(',');
    for (let iter = 0; iter < 150; iter++) {
      const n = 2 + Math.floor(rnd() * 22);
      const branches: BranchGraphData['branches'] = [gb('main', 'main', true)];
      const nodes: GraphNode[] = [node('main-s', 'main', Math.floor(rnd() * 100)), node('main-e', 'main', 100 + Math.floor(rnd() * 100))];
      for (let i = 0; i < n; i++) {
        const id = `b${i}`;
        // ~60% of branches join one of a few prefix farms; the rest are singletons.
        const name = rnd() < 0.6 ? `farm${Math.floor(rnd() * 3)}/${i}` : `solo${i}`;
        branches.push(gb(id, name));
        const s = Math.floor(rnd() * 900);
        nodes.push(node(`${id}-s`, id, s), node(`${id}-e`, id, s + Math.floor(rnd() * 150)));
      }
      const g: BranchGraphData = { branches, nodes, tags: [] };
      const groups = groupBranches(g);
      const expanded = new Set(groups.filter(() => rnd() < 0.5).map((x) => x.prefix));
      const layout = packedLanesGrouped(g, groups, expanded);

      // Reconstruct the effective span set the layout packed (collapsed union spans +
      // every non-folded branch) and assert no two share a lane within epsilon.
      const collapsed: BranchGroup[] = groups.filter((x) => !expanded.has(x.prefix));
      const folded = new Set(collapsed.flatMap((x) => x.branchIds));
      const effective: LaneSpan[] = [
        ...collapsed.map((x) => x.span),
        ...branchSpans(g).filter((s) => !folded.has(s.branchId)),
      ];
      const laneOf = (s: LaneSpan): number =>
        s.branchId.startsWith('group:')
          ? layout.laneOfBranch.get(collapsed.find((c) => c.span.branchId === s.branchId)!.branchIds[0])!
          : layout.laneOfBranch.get(s.branchId)!;
      const eps = epsOf(effective);
      const byLane = new Map<number, LaneSpan[]>();
      for (const s of effective) (byLane.get(laneOf(s)) ?? byLane.set(laneOf(s), []).get(laneOf(s))!).push(s);
      for (const list of byLane.values()) {
        list.sort((a, b) => a.start - b.start);
        for (let i = 1; i < list.length; i++) {
          expect(list[i - 1].end + eps).toBeLessThanOrEqual(list[i].start);
        }
      }
      // Determinism: identical layout on a second call.
      expect(ser(packedLanesGrouped(g, groups, expanded).laneOfBranch)).toBe(ser(layout.laneOfBranch));
    }
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

describe('fisheyeLaneYs — 1-D vertical lane magnification (wave B)', () => {
  // A "thin" configuration where fisheye actually engages: 40 lanes in a 200px
  // track → uniform rowH = 5px (< threshold). Focus near the middle.
  const N = 40, H = 200;
  const uniform = () => laneGeometry(N, H);

  it('is EXACT identity when focusY is null (byte-for-byte the uniform seam)', () => {
    const g = fisheyeLaneYs(N, H, null);
    const u = uniform();
    expect(g.active).toBe(false);
    expect(g.focusLane).toBeNull();
    for (let l = 0; l < N; l++) {
      expect(g.y(l)).toBe(u.y(l)); // exact equality, not close-to
      expect(g.rowH(l)).toBe(u.rowH);
    }
    expect(g.contentH).toBe(H);
  });

  it('is EXACT identity when rows are already comfortable (rowH >= threshold)', () => {
    // 4 lanes in 200px → uniform rowH = 16 (>= 10). Even with a live focus, no-op.
    const u = laneGeometry(4, 200);
    expect(u.rowH).toBeGreaterThanOrEqual(FISHEYE_THRESHOLD);
    const g = fisheyeLaneYs(4, 200, 100); // focus present but ignored
    expect(g.active).toBe(false);
    for (let l = 0; l < 4; l++) expect(g.y(l)).toBe(u.y(l));
  });

  it('a single lane is always identity (nothing to magnify)', () => {
    const g = fisheyeLaneYs(1, 40, 20);
    expect(g.active).toBe(false);
    expect(g.y(0)).toBe(laneGeometry(1, 40).y(0));
  });

  it('when active: total height conserved — stack fits [0, trackH], strictly monotonic', () => {
    for (const focusY of [10, 50, 100, 150, 199]) {
      const g = fisheyeLaneYs(N, H, focusY);
      expect(g.active).toBe(true);
      let prev = -Infinity;
      for (let l = 0; l < N; l++) {
        const y = g.y(l);
        expect(Number.isFinite(y)).toBe(true);
        expect(y).toBeGreaterThan(prev); // strictly below the lane above
        prev = y;
      }
      // First centre >= 0, last centre + half its row <= trackH (fits, no clip growth).
      expect(g.y(0) - g.rowH(0) / 2).toBeGreaterThanOrEqual(-1e-6);
      expect(g.y(N - 1) + g.rowH(N - 1) / 2).toBeLessThanOrEqual(H + 1e-6);
    }
  });

  it('the focused (nearest) lane swells to >= IDEAL_ROW*0.9 at any focus position', () => {
    // Sweep every 1px; the lane nearest the cursor must always be near-full height.
    for (let focusY = 0; focusY <= H; focusY++) {
      const g = fisheyeLaneYs(N, H, focusY);
      expect(g.rowH(g.focusLane!)).toBeGreaterThanOrEqual(IDEAL_ROW * 0.9);
    }
  });

  it('pinned anchor lanes keep >= PIN_MIN px even when the focus is far away', () => {
    // Focus at the very bottom → lanes 0/1 (pinned) are as far from focus as possible.
    const g = fisheyeLaneYs(N, H, H, { pinned: [0, 1] });
    expect(g.rowH(0)).toBeGreaterThanOrEqual(FISHEYE_PIN_MIN - 1e-6);
    expect(g.rowH(1)).toBeGreaterThanOrEqual(FISHEYE_PIN_MIN - 1e-6);
    // And a custom pin (a selected branch) is honoured too.
    const g2 = fisheyeLaneYs(N, H, 0, { pinned: [0, 1, 30] });
    expect(g2.rowH(30)).toBeGreaterThanOrEqual(FISHEYE_PIN_MIN - 1e-6);
  });

  it('is continuous in focusY — no jumps as the cursor moves (Lipschitz-ish)', () => {
    for (let focusY = 5; focusY < H - 5; focusY += 1) {
      const a = fisheyeLaneYs(N, H, focusY);
      const b = fisheyeLaneYs(N, H, focusY + 1);
      for (let l = 0; l < N; l++) {
        // A 1px focus step moves any lane centre by only a few px (no discontinuity).
        expect(Math.abs(a.y(l) - b.y(l))).toBeLessThan(8);
      }
    }
  });

  it('is deterministic — identical output across two independent calls', () => {
    const a = fisheyeLaneYs(N, H, 73);
    const b = fisheyeLaneYs(N, H, 73);
    for (let l = 0; l < N; l++) {
      expect(a.y(l)).toBe(b.y(l));
      expect(a.rowH(l)).toBe(b.rowH(l));
    }
  });

  it('sum of row heights fills the track exactly (conservation)', () => {
    const g = fisheyeLaneYs(N, H, 90);
    let sum = 0;
    for (let l = 0; l < N; l++) sum += g.rowH(l);
    expect(sum).toBeCloseTo(H, 4);
  });

  it('fuzz (seeded LCG): random laneCount/trackH/focusY hold every invariant, no NaN, deterministic', () => {
    let seed = 0x0bad_c0de;
    const rnd = () => ((seed = (seed * 1_103_515_245 + 12_345) & 0x7fff_ffff) / 0x7fff_ffff);
    for (let iter = 0; iter < 400; iter++) {
      const laneCount = 2 + Math.floor(rnd() * 200); // 2..201
      const trackH = 90 + Math.floor(rnd() * 320); // 90..409 (feasible pane sizes)
      const focusY = rnd() * trackH;
      const g = fisheyeLaneYs(laneCount, trackH, focusY);
      const u = laneGeometry(laneCount, trackH);
      if (u.rowH >= FISHEYE_THRESHOLD) {
        // Identity regime: exact match, regardless of focus.
        expect(g.active).toBe(false);
        for (let l = 0; l < laneCount; l++) expect(g.y(l)).toBe(u.y(l));
        continue;
      }
      expect(g.active).toBe(true);
      let prev = -Infinity, sum = 0;
      for (let l = 0; l < laneCount; l++) {
        const y = g.y(l), rh = g.rowH(l);
        expect(Number.isNaN(y)).toBe(false);
        expect(Number.isNaN(rh)).toBe(false);
        expect(rh).toBeGreaterThan(0);
        expect(y).toBeGreaterThan(prev);
        prev = y;
        sum += rh;
      }
      expect(sum).toBeCloseTo(trackH, 3); // conservation
      expect(g.y(laneCount - 1) + g.rowH(laneCount - 1) / 2).toBeLessThanOrEqual(trackH + 1e-6);
      // Focused + pinned guarantees.
      expect(g.rowH(g.focusLane!)).toBeGreaterThanOrEqual(IDEAL_ROW * 0.9 - 1e-6);
      expect(g.rowH(0)).toBeGreaterThanOrEqual(FISHEYE_PIN_MIN - 1e-6);
      expect(g.rowH(1)).toBeGreaterThanOrEqual(FISHEYE_PIN_MIN - 1e-6);
      // Determinism: a second identical call matches byte-for-byte.
      const g2 = fisheyeLaneYs(laneCount, trackH, focusY);
      for (let l = 0; l < laneCount; l++) expect(g2.y(l)).toBe(g.y(l));
    }
  });
});
