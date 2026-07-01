// Pure geometry for the unified history-timeline / branch-network view. The
// bottom timeline IS the network graph now: events are dots positioned by time
// (x) on their branch's lane (y), with curved fork edges where a branch diverged
// from its parent, and tag flags at named moments. Kept DOM-free so the lane
// assignment, fork edges and tag placement are unit-testable; the React
// component (HistoryBar) does only SVG/DOM from these results. The time math
// (toPct, zoom, clampView, axisTicks) stays in history.ts and is reused as-is.

import type { BranchGraphData, GraphBranch } from '../lib/api';
import type { TrackEvent } from './history';

export const MAIN_BRANCH_ID = 'main';

export interface Lane {
  branchId: string;
  name: string;
  lane: number; // 0 = main, top lane
  current: boolean;
  parent: string | null;
}

export interface ForkEdge {
  branchId: string;
  fromLane: number; // parent lane
  toLane: number; // child (branch) lane
  ts: number; // epoch ms where the branch's history starts (the divergence)
}

/** A stable, distinct colour per lane (main = the accent, others cycle a palette).
 *  Kept identical to the old network graph so the visual language is unchanged. */
export function laneColor(lane: number, accent: string): string {
  if (lane === 0) return accent;
  const palette = ['#e5484d', '#f76b15', '#46a758', '#8e4ec6', '#0091ff', '#e93d82', '#12a594', '#f5d90a'];
  return palette[(lane - 1) % palette.length];
}

/** Ordered lanes from the graph's branch list (main always lane 0, then by lane
 *  index). A graph is optional — with none, a single `main` lane is returned so the
 *  timeline still renders on a pre-branching / empty vault. */
export function lanesFromGraph(graph: BranchGraphData | null): Lane[] {
  const branches: GraphBranch[] = graph?.branches ?? [];
  if (branches.length === 0) {
    return [{ branchId: MAIN_BRANCH_ID, name: 'main', lane: 0, current: true, parent: null }];
  }
  return [...branches]
    .sort((a, b) => a.lane - b.lane)
    .map((b) => ({ branchId: b.id, name: b.name, lane: b.lane, current: b.current, parent: b.parent }));
}

/** branchId -> lane index, for placing an event/tag on its lane (unknown → 0). */
export function laneIndex(lanes: Lane[]): Map<string, number> {
  const m = new Map<string, number>();
  for (const l of lanes) m.set(l.branchId, l.lane);
  return m;
}

/** The lane an event sits on: its own branch's lane, else main (0). */
export function laneForEvent(e: TrackEvent, idx: Map<string, number>): number {
  return idx.get(e.branchId) ?? 0;
}

/** Fork edges: for every non-main lane with a parent, an edge from the parent lane
 *  to the branch lane at the branch's earliest event time (its divergence point on
 *  the timeline). Branches with no events yet are skipped (nothing to point at).
 *  Deterministic and order-independent. */
export function forkEdges(lanes: Lane[], events: TrackEvent[]): ForkEdge[] {
  const idx = laneIndex(lanes);
  // Earliest event ts per branch.
  const earliest = new Map<string, number>();
  for (const e of events) {
    const cur = earliest.get(e.branchId);
    if (cur == null || e.ts < cur) earliest.set(e.branchId, e.ts);
  }
  const out: ForkEdge[] = [];
  for (const l of lanes) {
    if (l.branchId === MAIN_BRANCH_ID || l.parent == null) continue;
    const ts = earliest.get(l.branchId);
    if (ts == null) continue; // no commits on this branch yet
    const fromLane = idx.get(l.parent) ?? 0;
    out.push({ branchId: l.branchId, fromLane, toLane: l.lane, ts });
  }
  return out;
}

/** Fallback dots from the coarsened graph commits, for surfaces where the full
 *  per-event `history()` isn't available (web degrades it to empty). Each commit
 *  becomes a track event on its branch lane so the timeline is never blank. */
export function nodesToEvents(graph: BranchGraphData | null): TrackEvent[] {
  const nodes = graph?.nodes ?? [];
  return nodes
    .map((n) => ({ id: n.commit_id, ts: n.ts * 1000, kind: 'edit', path: n.label, branchId: n.branch_id }))
    .sort((a, b) => a.ts - b.ts);
}

/** Vertical geometry for `laneCount` lanes inside a track of `height` px: the y of
 *  each lane and a per-lane row height, kept within [minRow, maxRow] and centred.
 *  One lane → the single centred row the timeline has always used. */
export function laneGeometry(laneCount: number, height: number): { rowH: number; y: (lane: number) => number; top: number } {
  const n = Math.max(1, laneCount);
  const rowH = Math.max(16, Math.min(30, (height - 12) / n));
  const used = rowH * n;
  const top = (height - used) / 2 + rowH / 2;
  return { rowH, top, y: (lane: number) => top + lane * rowH };
}
