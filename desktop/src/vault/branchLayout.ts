// Pure layout for the branch/commit network graph (GitHub-network-style). Kept
// separate from the React component so the geometry — lanes, node positions, fork
// and merge edges — is unit-testable without a DOM. The engine already coarsens +
// caps commits per branch; this only positions them.

import type { BranchGraphData, GraphNode } from '../lib/api';

export interface PlacedNode {
  node: GraphNode;
  x: number;
  y: number;
}
export interface Edge {
  x1: number;
  y1: number;
  x2: number;
  y2: number;
  /** Lane of the CHILD node — used to colour the edge by its branch. */
  lane: number;
  /** True when the edge crosses lanes (a fork or a merge), drawn as a curve. */
  cross: boolean;
}
export interface GraphLayout {
  placed: PlacedNode[];
  edges: Edge[];
  width: number;
  height: number;
  laneCount: number;
}

export interface LayoutOpts {
  colW: number; // horizontal spacing between successive commits (time axis →)
  rowH: number; // vertical spacing between lanes
  padX: number;
  padY: number;
}

export const DEFAULT_LAYOUT: LayoutOpts = { colW: 26, rowH: 34, padX: 22, padY: 22 };

/**
 * Place every commit on a left→right time axis (older left), one row per branch
 * lane, and connect each commit to its parents — a same-lane edge for the chain,
 * a curved cross-lane edge for a fork (a branch's first commit) or a merge.
 */
export function layoutGraph(data: BranchGraphData, opts: LayoutOpts = DEFAULT_LAYOUT): GraphLayout {
  const { colW, rowH, padX, padY } = opts;

  // Time order: parents (lower lamport) come first, so x increases with the DAG.
  const sorted = [...data.nodes].sort((a, b) => a.lamport - b.lamport || (a.commit_id < b.commit_id ? -1 : 1));

  const pos = new Map<string, PlacedNode>();
  const placed: PlacedNode[] = sorted.map((node, i) => {
    const p: PlacedNode = { node, x: padX + i * colW, y: padY + node.lane * rowH };
    pos.set(node.commit_id, p);
    return p;
  });

  const edges: Edge[] = [];
  for (const p of placed) {
    for (const parentId of p.node.parents) {
      const from = pos.get(parentId);
      if (!from) continue;
      edges.push({ x1: from.x, y1: from.y, x2: p.x, y2: p.y, lane: p.node.lane, cross: from.y !== p.y });
    }
  }

  const laneCount = data.branches.reduce((m, b) => Math.max(m, b.lane + 1), 1);
  const width = padX * 2 + Math.max(0, sorted.length - 1) * colW + 8;
  const height = padY * 2 + Math.max(0, laneCount - 1) * rowH + 8;
  return { placed, edges, width, height, laneCount };
}

/** A stable, distinct colour per lane (main = the accent, others cycle a palette). */
export function laneColor(lane: number, accent: string): string {
  if (lane === 0) return accent;
  const palette = ['#e5484d', '#f76b15', '#46a758', '#8e4ec6', '#0091ff', '#e93d82', '#12a594', '#f5d90a'];
  return palette[(lane - 1) % palette.length];
}
