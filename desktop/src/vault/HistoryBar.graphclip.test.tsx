import { mock } from 'bun:test';
import { cleanup, render } from '@testing-library/react';
import { afterEach, beforeAll, describe, expect, it, vi } from '../test-shim';
import type { BranchGraphData, GraphNode, HistEvent, VaultStatus } from '../lib/api';
import { buildEvents } from './history';
import { laneGeometry } from './timeline';
import HistoryBar, { type HistoryBarProps } from './HistoryBar';

const revealPath = vi.fn(async () => {});
mock.module('../lib/api', () => ({ api: { revealPath: (...a: unknown[]) => revealPath(...(a as [])) } }));

afterEach(() => { cleanup(); vi.useRealTimers(); });

const NOW = 1_700_000_000_000;
const NOW_S = NOW / 1000;
const status: VaultStatus = { id: 'v1', vault_id: 'vid', rows: 4, files: 2, head: 'h', listening_ticket: 'asp1abc', peers: [], last_ts: 1 };

// A branch's two-node span [startS, endS] (seconds) so packing has a real interval
// to work with (a single node would be a zero-length span). main is lane 0.
function branchWithSpan(id: string, name: string, current: boolean, startS: number, endS: number) {
  const branch = { id, name, parent: id === 'main' ? null : 'main', head_commit: `${id}-h`, lane: 0, current };
  const nodes: GraphNode[] = [
    { commit_id: `${id}-a`, branch_id: id, parents: [], ts: startS, lamport: 1, label: `${name} start`, lane: 0 },
    { commit_id: `${id}-h`, branch_id: id, parents: [`${id}-a`], ts: endS, lamport: 2, label: `${name} end`, lane: 0 },
  ];
  return { branch, nodes };
}

// n branches all ACTIVE across the same window → every span overlaps → packing
// cannot collapse them: packed lane count == n (stress the fit/clip contract).
function overlappingGraph(n: number): { graph: BranchGraphData; raw: HistEvent[] } {
  const startS = NOW_S - 3 * 86400;
  const endS = NOW_S - 2 * 86400;
  const parts = Array.from({ length: n }, (_, i) =>
    branchWithSpan(i === 0 ? 'main' : `b${i}`, i === 0 ? 'main' : `feature-${i}`, i === 0, startS, endS),
  );
  const raw: HistEvent[] = parts.map((p, i) => ({
    id: `r${i}`, ts: startS + i, lamport: i + 1, kind: 'edit', path: `${p.branch.name}.md`, branch_id: p.branch.id,
  }));
  return { graph: { nodes: parts.flatMap((p) => p.nodes), branches: parts.map((p) => p.branch), tags: [] }, raw };
}

// n branches with DISJOINT sequential spans → stale branches free their lanes and
// pack onto a handful → packed lane count << n (the whole point of the redesign).
function staggeredGraph(n: number): { graph: BranchGraphData; raw: HistEvent[] } {
  const parts = Array.from({ length: n }, (_, i) => {
    if (i === 0) return branchWithSpan('main', 'main', true, NOW_S - (n + 1) * 3600, NOW_S - n * 3600);
    const startS = NOW_S - (n - i) * 3600; // each an hour apart, 30-min long → disjoint
    return branchWithSpan(`b${i}`, `feature-${i}`, false, startS, startS + 1800);
  });
  const raw: HistEvent[] = parts.map((p, i) => ({
    id: `r${i}`, ts: NOW_S - (n - i) * 3600, lamport: i + 1, kind: 'edit', path: `${p.branch.name}.md`, branch_id: p.branch.id,
  }));
  return { graph: { nodes: parts.flatMap((p) => p.nodes), branches: parts.map((p) => p.branch), tags: [] }, raw };
}

const props = (build: (n: number) => { graph: BranchGraphData; raw: HistEvent[] }, n: number): HistoryBarProps => {
  const { graph, raw } = build(n);
  return {
    events: buildEvents(raw),
    histRaw: raw,
    view: { start: NOW - 7 * 86400000, end: NOW + 0.4 * 86400000 },
    setView: vi.fn(),
    playhead: null,
    setPlayhead: vi.fn(),
    now: NOW,
    accent: '#3d63dd',
    accentSoft: '#3d63dd22',
    timeTravel: false,
    location: '/home/me/vault',
    locationIsPath: true,
    fingerprint: 'AB12',
    status,
    identity: 'ssh-ed25519 DEVICEKEY me@host',
    histOpen: true,
    logOpen: false,
    barHeight: 200,
    animate: false,
    graph,
    currentBranch: 'main',
    onCheckoutBranch: vi.fn(),
    onCreateTag: vi.fn(),
    onDeleteTag: vi.fn(),
    loadDiff: vi.fn(async () => null),
    onTabHistory: vi.fn(),
    onTabLog: vi.fn(),
    onNow: vi.fn(),
  };
};

const px = (v: string) => parseFloat(v.replace('px', ''));

describe('laneGeometry adaptive-fit contract (no lane-axis scroll)', () => {
  // Frozen: one lane stays exactly centred, surface fills the track.
  it('a single lane fits and reports contentH === track height', () => {
    const g = laneGeometry(1, 96);
    expect(g.contentH).toBe(96);
    expect(g.y(0)).toBeCloseTo(48, 0);
  });

  // A handful of lanes still sit at the comfortable row height, still no scroll.
  it('a few lanes fit at IDEAL_ROW and never exceed the track (contentH === height)', () => {
    for (const n of [2, 3, 4, 5]) {
      const g = laneGeometry(n, 96);
      expect(g.contentH).toBe(96); // never grows → no scroll surface
      expect(g.rowH).toBe(16); // 96/n >= 16 for n<=6 → clamped to IDEAL_ROW
      expect(g.y(n - 1)).toBeLessThanOrEqual(96);
    }
  });

  // Many lanes: rowH bottoms out at the MIN_ROW floor, but contentH still equals
  // the track — lanes become thin threads that FIT rather than scroll.
  it('many lanes shrink to the MIN_ROW floor with contentH pinned to the track', () => {
    const g = laneGeometry(60, 96);
    expect(g.contentH).toBe(96); // no growth, no scroll
    expect(g.rowH).toBe(3); // clamp(96/60, 3, 16) → 3 (floor)
    // 128-lane clone (the reported bug): still floored, still fills the track.
    const big = laneGeometry(128, 96);
    expect(big.rowH).toBe(3);
    expect(big.contentH).toBe(96);
  });
});

// jsdom does no layout, so the track's ResizeObserver would measure 0px. Give the
// track a real measured size (a ~200px history pane) so geometry matches a browser.
describe('HistoryBar fits the lane axis inside the pane (no scroll, hard clip)', () => {
  beforeAll(() => {
    Object.defineProperty(HTMLElement.prototype, 'clientHeight', { configurable: true, get() { return 200; } });
    Object.defineProperty(HTMLElement.prototype, 'clientWidth', { configurable: true, get() { return 640; } });
  });

  it('many overlapping branches: BOTH axes clipped, surface fills the track (no scroll)', () => {
    const { getByTestId } = render(<HistoryBar {...props(overlappingGraph, 40)} />);
    const track = getByTestId('history-track');
    const surface = getByTestId('history-surface');
    // Hard clip on BOTH axes now — scroll is reserved for the time axis.
    expect(track.style.overflowX).toBe('hidden');
    expect(track.style.overflowY).toBe('hidden');
    // Surface fills the track exactly; it never grows a scrollable overflow.
    expect(px(surface.style.height)).toBe(200);
    expect(surface.style.minHeight).toBe('100%');
  });

  it('overlapping spans cannot be packed: one guide line per branch (lane count == n)', () => {
    // All 40 spans overlap → packing must keep 40 distinct lanes.
    const { container } = render(<HistoryBar {...props(overlappingGraph, 40)} />);
    const guides = container.querySelectorAll('svg line');
    expect(guides.length).toBe(40);
  });

  it('disjoint (stale) spans PACK: far fewer guide lines than branches', () => {
    // 40 branches, each active for a disjoint 30-min window → they collapse onto a
    // tiny number of shared lanes (main + a couple), proving the reduction.
    const { container } = render(<HistoryBar {...props(staggeredGraph, 40)} />);
    const guides = container.querySelectorAll('svg line');
    expect(guides.length).toBeLessThan(40);
    expect(guides.length).toBeLessThanOrEqual(4);
  });

  it('small-lane graph: still clipped both axes, surface never exceeds the track', () => {
    const { getByTestId } = render(<HistoryBar {...props(overlappingGraph, 2)} />);
    const track = getByTestId('history-track');
    const surface = getByTestId('history-surface');
    expect(track.style.overflowX).toBe('hidden');
    expect(track.style.overflowY).toBe('hidden');
    expect(px(surface.style.height)).toBeLessThanOrEqual(200);
  });
});

import { afterAll as __aa, mock as __mk } from 'bun:test';
__aa(() => __mk.restore());
