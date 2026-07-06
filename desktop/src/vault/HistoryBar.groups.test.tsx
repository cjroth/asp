import { mock } from 'bun:test';
import { act, cleanup, fireEvent, render } from '@testing-library/react';
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from '../test-shim';
import type { BranchGraphData, GraphNode, HistEvent, VaultStatus } from '../lib/api';
import { buildEvents } from './history';
import { laneCountOf, laneGeometry, packedLanes } from './timeline';
import HistoryBar, { type HistoryBarProps } from './HistoryBar';

const revealPath = vi.fn(async () => {});
mock.module('../lib/api', () => ({ api: { revealPath: (...a: unknown[]) => revealPath(...(a as [])) } }));

// rAF/cAF sync so a mousemove commits the fisheye focus within one act().
beforeAll(() => {
  (globalThis as unknown as { requestAnimationFrame: (cb: FrameRequestCallback) => number }).requestAnimationFrame = (
    cb,
  ) => { cb(0); return 1; };
  (globalThis as unknown as { cancelAnimationFrame: (h: number) => void }).cancelAnimationFrame = () => {};
});

// A fresh localStorage per test so a persisted expand/collapse choice never leaks
// into the next test's size-based default.
beforeEach(() => { try { localStorage.clear(); } catch { /* ignore */ } });
afterEach(() => { cleanup(); vi.useRealTimers(); });

const NOW = 1_700_000_000_000;
const NOW_S = NOW / 1000;
const status: VaultStatus = { id: 'v1', vault_id: 'vid', rows: 4, files: 2, head: 'h', listening_ticket: 'asp1abc', peers: [], last_ts: 1 };

// A branch active over one shared [startS,endS] window (overlaps everything → no
// packing collapse; every span needs its own lane).
function branchWithSpan(id: string, name: string, current: boolean, startS: number, endS: number) {
  const branch = { id, name, parent: id === 'main' ? null : 'main', head_commit: `${id}-h`, lane: 0, current };
  const nodes: GraphNode[] = [
    { commit_id: `${id}-a`, branch_id: id, parents: [], ts: startS, lamport: 1, label: `${name} start`, lane: 0 },
    { commit_id: `${id}-h`, branch_id: id, parents: [`${id}-a`], ts: endS, lamport: 2, label: `${name} end`, lane: 0 },
  ];
  return { branch, nodes };
}

function build(parts: { branch: BranchGraphData['branches'][number]; nodes: GraphNode[] }[]): { graph: BranchGraphData; raw: HistEvent[] } {
  const startS = NOW_S - 3 * 86400;
  const raw: HistEvent[] = parts.map((p, i) => ({
    id: `r${i}`, ts: startS + i, lamport: i + 1, kind: 'edit', path: `${p.branch.name}.md`, branch_id: p.branch.id,
  }));
  return { graph: { nodes: parts.flatMap((p) => p.nodes), branches: parts.map((p) => p.branch), tags: [] }, raw };
}

// main + 12 dependabot/* + 9 cursor/* + 19 slash-less singletons, ALL overlapping
// one window → 41 packed lanes ungrouped (a farm drowning the pane).
function farmGraph(): { graph: BranchGraphData; raw: HistEvent[] } {
  const startS = NOW_S - 3 * 86400, endS = NOW_S - 2 * 86400;
  const parts = [branchWithSpan('main', 'main', true, startS, endS)];
  for (let i = 0; i < 12; i++) parts.push(branchWithSpan(`d${i}`, `dependabot/npm/${i}`, false, startS, endS));
  for (let i = 0; i < 9; i++) parts.push(branchWithSpan(`c${i}`, `cursor/${i}`, false, startS, endS));
  for (let i = 0; i < 19; i++) parts.push(branchWithSpan(`s${i}`, `solo-${i}`, false, startS, endS));
  return build(parts);
}

// main + 3 foo/* + 2 slash-less → 6 overlapping lanes (<= threshold) → all-expanded
// default; the render must match the ungrouped wave-B baseline byte-for-byte.
function smallGroupGraph(): { graph: BranchGraphData; raw: HistEvent[] } {
  const startS = NOW_S - 3 * 86400, endS = NOW_S - 2 * 86400;
  const parts = [
    branchWithSpan('main', 'main', true, startS, endS),
    branchWithSpan('f1', 'foo/1', false, startS, endS),
    branchWithSpan('f2', 'foo/2', false, startS, endS),
    branchWithSpan('f3', 'foo/3', false, startS, endS),
    branchWithSpan('b1', 'bar', false, startS, endS),
    branchWithSpan('b2', 'baz', false, startS, endS),
  ];
  return build(parts);
}

const mkProps = (g: { graph: BranchGraphData; raw: HistEvent[] }): HistoryBarProps => ({
  events: buildEvents(g.raw),
  histRaw: g.raw,
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
  graph: g.graph,
  currentBranch: 'main',
  onCheckoutBranch: vi.fn(),
  onCreateTag: vi.fn(),
  onDeleteTag: vi.fn(),
  loadDiff: vi.fn(async () => null),
  onTabHistory: vi.fn(),
  onTabLog: vi.fn(),
  onNow: vi.fn(),
});

const guideCount = (c: HTMLElement) => c.querySelectorAll('svg line').length;
const guideYs = (c: HTMLElement): number[] =>
  Array.from(c.querySelectorAll('svg line')).map((l) => parseFloat(l.getAttribute('y1') || '0'));

// jsdom does no layout — give the track a real 640x200 measured size, and a
// bounding rect anchored at (0,0) so fisheye focusY = clientY.
beforeAll(() => {
  Object.defineProperty(HTMLElement.prototype, 'clientHeight', { configurable: true, get() { return 200; } });
  Object.defineProperty(HTMLElement.prototype, 'clientWidth', { configurable: true, get() { return 640; } });
  Object.defineProperty(HTMLElement.prototype, 'getBoundingClientRect', {
    configurable: true,
    value() { return { left: 0, top: 0, right: 640, bottom: 200, width: 640, height: 200, x: 0, y: 0, toJSON() {} }; },
  });
});

describe('HistoryBar prefix groups (wave C accordion)', () => {
  it('big two-farm graph defaults COLLAPSED: lane count drops, group chips show counts', () => {
    const g = farmGraph();
    expect(laneCountOf(packedLanes(g.graph))).toBe(41); // ungrouped: main + 40 overlapping
    const { container, getByTestId } = render(<HistoryBar {...mkProps(g)} />);
    // Collapsed default: main + 19 singletons + dependabot(1) + cursor(1) = 22 lanes.
    expect(guideCount(container)).toBe(22);
    // A single chip per farm, with its member count.
    const dep = getByTestId('group-chip-dependabot');
    const cur = getByTestId('group-chip-cursor');
    expect(dep.textContent).toContain('dependabot/');
    expect(dep.textContent).toContain('(12)');
    expect(cur.textContent).toContain('(9)');
    // Members render as micro-dots (not full dots/labels) while collapsed.
    expect(container.querySelectorAll('[data-testid="group-microdot"]').length).toBeGreaterThan(0);
    expect(container.querySelector('[data-testid="lane-label-dependabot/npm/0"]')).toBeNull();
  });

  it('clicking a group chip expands it (lane count rises), clicking again collapses', () => {
    const { container, getByTestId } = render(<HistoryBar {...mkProps(farmGraph())} />);
    expect(guideCount(container)).toBe(22);

    // Expand dependabot: its 12 members overlap → +12 lanes, −1 group lane = 33.
    act(() => { fireEvent.pointerDown(getByTestId('group-chip-dependabot')); });
    expect(guideCount(container)).toBe(33);
    // The expanded chip is a collapse affordance (▾) and members are now individual.
    expect(getByTestId('group-chip-dependabot').textContent).toContain('▾');
    expect(container.querySelectorAll('[data-testid="group-microdot"]').length).toBeGreaterThan(0); // cursor still collapsed

    // Collapse it again → back to 22.
    act(() => { fireEvent.pointerDown(getByTestId('group-chip-dependabot')); });
    expect(guideCount(container)).toBe(22);
    expect(getByTestId('group-chip-dependabot').textContent).toContain('▸');
  });

  it('a collapsed micro-dot expands its group (it is not a checkout)', () => {
    const p = mkProps(farmGraph());
    const { container } = render(<HistoryBar {...p} />);
    const micro = container.querySelector('[data-testid="group-microdot"]')!;
    expect(micro).not.toBeNull();
    act(() => { fireEvent.pointerDown(micro); });
    // No checkout fired; a group expanded → total lanes grew past the collapsed 22.
    expect(p.onCheckoutBranch).not.toHaveBeenCalled();
    expect(guideCount(container)).toBeGreaterThan(22);
  });

  it('small graph with one 3-member group defaults ALL-EXPANDED: byte-identical to the ungrouped baseline', () => {
    const g = smallGroupGraph();
    const baseCount = laneCountOf(packedLanes(g.graph)); // 6, <= threshold
    expect(baseCount).toBe(6);
    const expectedYs = Array.from({ length: baseCount }, (_, l) => laneGeometry(baseCount, 200).y(l));
    const { container, getByTestId } = render(<HistoryBar {...mkProps(g)} />);
    // Same lane count and the exact same guide y-seam as a no-group render.
    expect(guideCount(container)).toBe(6);
    expect(guideYs(container)).toEqual(expectedYs);
    // Nothing collapsed → no micro-dots; the chip is present as a collapse affordance.
    expect(container.querySelectorAll('[data-testid="group-microdot"]').length).toBe(0);
    expect(getByTestId('group-chip-foo').textContent).toContain('▾');
  });

  it('fisheye composes: collapsed thin lanes still magnify on hover', () => {
    const { container, getByTestId } = render(<HistoryBar {...mkProps(farmGraph())} />);
    const track = getByTestId('history-track');
    // 22 lanes / 200px → ~9px rows (< 10 threshold) → fisheye engages.
    const before = guideYs(container);
    const uniformGap = before[1] - before[0];
    act(() => { fireEvent.mouseMove(track, { clientY: 100 }); });
    const after = guideYs(container);
    const maxGap = Math.max(...after.slice(1).map((y, i) => y - after[i]));
    expect(maxGap).toBeGreaterThan(uniformGap * 2); // a lane clearly magnified
  });
});

import { afterAll as __aa, mock as __mk } from 'bun:test';
__aa(() => __mk.restore());
