import { mock } from 'bun:test';
import { act, cleanup, fireEvent, render } from '@testing-library/react';
import { afterEach, beforeAll, describe, expect, it, vi } from '../test-shim';
import type { BranchGraphData, GraphNode, HistEvent, VaultStatus } from '../lib/api';
import { buildEvents } from './history';
import HistoryBar, { type HistoryBarProps } from './HistoryBar';

const revealPath = vi.fn(async () => {});
mock.module('../lib/api', () => ({ api: { revealPath: (...a: unknown[]) => revealPath(...(a as [])) } }));

// rAF/cAF are used to throttle the fisheye focus commit. jsdom lacks them; run
// the callback synchronously so a mousemove commits within one act().
beforeAll(() => {
  (globalThis as unknown as { requestAnimationFrame: (cb: FrameRequestCallback) => number }).requestAnimationFrame = (
    cb,
  ) => { cb(0); return 1; };
  (globalThis as unknown as { cancelAnimationFrame: (h: number) => void }).cancelAnimationFrame = () => {};
});

afterEach(() => { cleanup(); vi.useRealTimers(); });

const NOW = 1_700_000_000_000;
const NOW_S = NOW / 1000;
const status: VaultStatus = { id: 'v1', vault_id: 'vid', rows: 4, files: 2, head: 'h', listening_ticket: 'asp1abc', peers: [], last_ts: 1 };

function branchWithSpan(id: string, name: string, current: boolean, startS: number, endS: number) {
  const branch = { id, name, parent: id === 'main' ? null : 'main', head_commit: `${id}-h`, lane: 0, current };
  const nodes: GraphNode[] = [
    { commit_id: `${id}-a`, branch_id: id, parents: [], ts: startS, lamport: 1, label: `${name} start`, lane: 0 },
    { commit_id: `${id}-h`, branch_id: id, parents: [`${id}-a`], ts: endS, lamport: 2, label: `${name} end`, lane: 0 },
  ];
  return { branch, nodes };
}

// n branches all active over the same window → 40 distinct packed lanes (thin
// rows → fisheye engages). Each on its own lane so guide count == n.
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

const props = (n: number): HistoryBarProps => {
  const { graph, raw } = overlappingGraph(n);
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
    barHeight: 240,
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
// The guide <line>s are one per lane, in lane order: their y1 IS geom.y(lane).
const guideYs = (container: HTMLElement): number[] =>
  Array.from(container.querySelectorAll('svg line')).map((l) => parseFloat(l.getAttribute('y1') || '0'));

// jsdom does no layout; give the track a real measured size (a ~200px pane).
beforeAll(() => {
  Object.defineProperty(HTMLElement.prototype, 'clientHeight', { configurable: true, get() { return 200; } });
  Object.defineProperty(HTMLElement.prototype, 'clientWidth', { configurable: true, get() { return 640; } });
  // getBoundingClientRect drives focusY = clientY - rect.top; anchor at 0.
  Object.defineProperty(HTMLElement.prototype, 'getBoundingClientRect', {
    configurable: true,
    value() { return { left: 0, top: 0, right: 640, bottom: 200, width: 640, height: 200, x: 0, y: 0, toJSON() {} }; },
  });
});

describe('HistoryBar vertical fisheye (wave B)', () => {
  it('40-lane thin graph: hovering magnifies the lane under the cursor and restores on leave', () => {
    const { getByTestId, container } = render(<HistoryBar {...props(40)} />);
    const track = getByTestId('history-track');

    // Uniform baseline: 40 lanes / 200px → evenly spaced ~5px guides.
    const before = guideYs(container);
    expect(before.length).toBe(40);
    const uniformGaps = before.slice(1).map((y, i) => y - before[i]);
    // All gaps ~equal (uniform) before any hover.
    for (const g of uniformGaps) expect(g).toBeCloseTo(uniformGaps[0], 3);

    // Hover near the vertical middle (~lane 20).
    act(() => { fireEvent.mouseMove(track, { clientY: 100 }); });
    const after = guideYs(container);
    expect(after.length).toBe(40);

    // The gap around the hovered lane widened toward IDEAL_ROW spacing…
    const focusGaps = after.slice(1).map((y, i) => y - after[i]);
    const maxFocusGap = Math.max(...focusGaps);
    expect(maxFocusGap).toBeGreaterThan(uniformGaps[0] * 2); // clearly magnified
    expect(maxFocusGap).toBeGreaterThanOrEqual(16 * 0.9); // ~IDEAL_ROW row
    // …the widest gap sits near the cursor (lane ~20), not at the edges.
    const widestAt = focusGaps.indexOf(maxFocusGap);
    expect(widestAt).toBeGreaterThan(12);
    expect(widestAt).toBeLessThan(28);

    // Still strictly monotonic and still inside the track (no clip/scroll change).
    for (let i = 1; i < after.length; i++) expect(after[i]).toBeGreaterThan(after[i - 1]);
    expect(after[after.length - 1]).toBeLessThanOrEqual(200);

    // Mouse leave → back to the uniform layout, exactly.
    act(() => { fireEvent.mouseLeave(track); });
    const restored = guideYs(container);
    for (let i = 0; i < restored.length; i++) expect(restored[i]).toBeCloseTo(before[i], 5);
  });

  it('reveals a full label chip for the exact lane under the cursor', () => {
    // Focus geometry is deterministic: 40 lanes / 200px → uniform top 2.5, rowH 5.
    // Cursor y=100 → fLane=(100-2.5)/5=19.5 → focus lane 20. Packed lanes are
    // string-sorted, so display lane 20 is branch b27 (name "feature-27").
    const { getByTestId, container } = render(<HistoryBar {...props(40)} />);
    const track = getByTestId('history-track');
    const isLabel = () => container.querySelector('[data-testid="lane-label-feature-27"]') != null;
    const isTip = () => container.querySelector('[data-testid="lane-tip-feature-27"]') != null;

    // At uniform 5px density the hovered lane is a decluttered dot, not a chip.
    expect(isLabel()).toBe(false);
    expect(isTip()).toBe(true);

    // Hover at y=100 → the hovered lane wins highest label priority → full chip.
    act(() => { fireEvent.mouseMove(track, { clientY: 100 }); });
    expect(isLabel()).toBe(true);

    // Leaving restores the decluttered dot.
    act(() => { fireEvent.mouseLeave(track); });
    expect(isLabel()).toBe(false);
    expect(isTip()).toBe(true);
  });

  it('click-to-checkout still hits the right branch under magnification', () => {
    const p = props(40);
    const { getByTestId, container } = render(<HistoryBar {...p} />);
    const track = getByTestId('history-track');
    act(() => { fireEvent.mouseMove(track, { clientY: 100 }); });
    // A revealed magnified-lane label/tip fires onCheckoutBranch with its branchId.
    const tip = container.querySelector('[data-testid^="lane-label-feature-"], [data-testid^="lane-tip-feature-"]');
    expect(tip).not.toBeNull();
    fireEvent.pointerDown(tip!);
    expect(p.onCheckoutBranch).toHaveBeenCalledTimes(1);
    const arg = (p.onCheckoutBranch as unknown as { mock: { calls: unknown[][] } }).mock.calls[0][0] as string;
    expect(arg).toMatch(/^b\d+$/); // a real feature branch id, not main
  });

  it('4-lane comfortable graph: mousemove changes NOTHING (identity guard)', () => {
    const { getByTestId, container } = render(<HistoryBar {...props(4)} />);
    const track = getByTestId('history-track');
    const before = guideYs(container);
    expect(before.length).toBe(4);
    act(() => { fireEvent.mouseMove(track, { clientY: 100 }); });
    const after = guideYs(container);
    // rowH = 16 >= threshold → fisheye is a no-op; guides are byte-for-byte identical.
    expect(after).toEqual(before);
  });
});

import { afterAll as __aa, mock as __mk } from 'bun:test';
__aa(() => __mk.restore());
