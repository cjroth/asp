// The GitHub-network-tab-style branch/commit graph. Renders the engine's coarsened
// DAG as an SVG: one horizontal lane per branch, commits left→right by time, fork
// and merge edges curving between lanes. Clicking a lane label checks that branch
// out; hovering a node shows its label. All geometry comes from `layoutGraph`, so
// this file is just the SVG + interactions.

import { useMemo, useState } from 'react';
import type { BranchGraphData } from '../lib/api';
import { DEFAULT_LAYOUT, laneColor, layoutGraph } from './branchLayout';

export interface BranchGraphProps {
  data: BranchGraphData;
  accent: string;
  /** Check out a branch (lane click). */
  onCheckout: (branchId: string) => void;
  /** Currently checked-out branch id (highlighted). */
  current: string;
}

export default function BranchGraph({ data, accent, onCheckout, current }: BranchGraphProps) {
  const layout = useMemo(() => layoutGraph(data), [data]);
  const [hover, setHover] = useState<string | null>(null);

  const LABEL_W = 120;
  const svgW = LABEL_W + layout.width;
  const branchesByLane = useMemo(() => [...data.branches].sort((a, b) => a.lane - b.lane), [data.branches]);

  if (data.nodes.length === 0) {
    return (
      <div data-testid="branch-graph-empty" style={{ padding: 24, color: 'var(--faint)', fontSize: 13 }}>
        No commits yet — edit a file to start the history, then branch from any point.
      </div>
    );
  }

  return (
    <div data-testid="branch-graph" className="asp-scroll" style={{ overflow: 'auto', maxHeight: '100%', padding: '6px 0' }}>
      <svg width={svgW} height={layout.height} style={{ display: 'block', fontFamily: 'inherit' }}>
        {/* lane guide lines + clickable branch labels */}
        {branchesByLane.map((b) => {
          const y = DEFAULT_LAYOUT.padY + b.lane * DEFAULT_LAYOUT.rowH;
          const color = laneColor(b.lane, accent);
          return (
            <g key={b.id}>
              <line x1={LABEL_W} y1={y} x2={svgW} y2={y} stroke="var(--line)" strokeWidth={1} />
              <text
                data-testid={`branch-lane-${b.name}`}
                x={8}
                y={y + 4}
                onClick={() => onCheckout(b.id)}
                style={{ cursor: 'pointer', fontSize: 12, fontWeight: b.id === current ? 700 : 500, fill: b.id === current ? color : 'var(--text)' }}
              >
                {b.id === current ? '● ' : ''}
                {b.name}
              </text>
            </g>
          );
        })}

        {/* edges (fork/merge edges curve across lanes) */}
        {layout.edges.map((e, i) => {
          const x1 = LABEL_W + e.x1;
          const x2 = LABEL_W + e.x2;
          const color = laneColor(e.lane, accent);
          const d = e.cross
            ? `M ${x1} ${e.y1} C ${(x1 + x2) / 2} ${e.y1}, ${(x1 + x2) / 2} ${e.y2}, ${x2} ${e.y2}`
            : `M ${x1} ${e.y1} L ${x2} ${e.y2}`;
          return <path key={i} d={d} fill="none" stroke={color} strokeWidth={1.6} opacity={0.85} />;
        })}

        {/* commit nodes */}
        {layout.placed.map((p) => {
          const cx = LABEL_W + p.x;
          const color = laneColor(p.node.lane, accent);
          const isHover = hover === p.node.commit_id;
          return (
            <g key={p.node.commit_id} onMouseEnter={() => setHover(p.node.commit_id)} onMouseLeave={() => setHover(null)}>
              <circle data-testid="branch-commit" cx={cx} cy={p.y} r={isHover ? 6 : 4.5} fill={color} stroke="var(--bg)" strokeWidth={1.5} />
              {isHover && (
                <text x={cx + 9} y={p.y - 8} style={{ fontSize: 11, fill: 'var(--text)' }}>
                  {p.node.label}
                </text>
              )}
            </g>
          );
        })}
      </svg>
    </div>
  );
}
