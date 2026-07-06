import { mock } from 'bun:test';
import { cleanup, render } from '@testing-library/react';
import { afterEach, beforeAll, describe, expect, it, vi } from '../test-shim';
import type { BranchGraphData, HistEvent, VaultStatus } from '../lib/api';
import { buildEvents } from './history';
import { laneGeometry } from './timeline';
import HistoryBar, { type HistoryBarProps } from './HistoryBar';

const revealPath = vi.fn(async () => {});
mock.module('../lib/api', () => ({ api: { revealPath: (...a: unknown[]) => revealPath(...(a as [])) } }));

afterEach(() => { cleanup(); vi.useRealTimers(); });

const NOW = 1_700_000_000_000;
const status: VaultStatus = { id: 'v1', vault_id: 'vid', rows: 4, files: 2, head: 'h', listening_ticket: 'asp1abc', peers: [], last_ts: 1 };

// A graph with `n` lanes: main (lane 0) plus n-1 feature branches, each with one node.
function manyLaneGraph(n: number): { graph: BranchGraphData; raw: HistEvent[] } {
  const branches = Array.from({ length: n }, (_, i) => ({
    id: i === 0 ? 'main' : `b${i}`,
    name: i === 0 ? 'main' : `feature-${i}`,
    parent: i === 0 ? null : 'main',
    head_commit: null,
    lane: i,
    current: i === 0,
  }));
  const raw: HistEvent[] = branches.map((b, i) => ({
    id: `r${i}`, ts: 1_699_999_000 + i, lamport: i + 1, kind: 'edit', path: `${b.name}.md`, branch_id: b.id,
  }));
  return { graph: { nodes: [], branches, tags: [] }, raw };
}

const props = (n: number): HistoryBarProps => {
  const { graph, raw } = manyLaneGraph(n);
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

describe('laneGeometry lane-axis contract', () => {
  // The frozen fitting case: one lane stays exactly centred, no extra height.
  it('a single lane fits and reports contentH === track height (no scroll)', () => {
    const g = laneGeometry(1, 96);
    expect(g.contentH).toBe(96);
    expect(g.y(0)).toBeCloseTo(48, 0);
  });

  // A handful of lanes still fit the default bar — the common multi-branch case
  // must NOT gain a scrollbar (regression guard for existing users).
  it('a few lanes still fit (contentH === track height)', () => {
    for (const n of [2, 3, 4, 5]) {
      const g = laneGeometry(n, 96);
      expect(g.contentH).toBe(96);
      expect(g.y(n - 1)).toBeLessThanOrEqual(96);
    }
  });

  // Many lanes overflow: contentH grows past the track and is derived from the
  // lane count at the 16px row floor, so the pane can scroll on the lane axis.
  it('many lanes overflow: contentH scales with lane count past the track', () => {
    const g = laneGeometry(60, 96);
    expect(g.contentH).toBeGreaterThan(96);
    expect(g.contentH).toBe(60 * 16 + 12); // 16px row floor * lanes + pad
    // 128-lane clone (the reported bug) needs a much taller surface still.
    expect(laneGeometry(128, 96).contentH).toBeGreaterThan(g.contentH);
    // Monotonic in lane count → scrollbar appears naturally as branches grow.
    expect(laneGeometry(128, 96).contentH).toBe(128 * 16 + 12);
  });
});

// jsdom does no layout, so the track's ResizeObserver would measure a 0px
// height and think every graph overflows. Give the track a real measured size
// (a ~200px history pane) so the fit/overflow decision matches a real browser.
describe('HistoryBar clips and scrolls the lane axis', () => {
  beforeAll(() => {
    Object.defineProperty(HTMLElement.prototype, 'clientHeight', { configurable: true, get() { return 200; } });
    Object.defineProperty(HTMLElement.prototype, 'clientWidth', { configurable: true, get() { return 640; } });
  });

  it('many-lane graph: track clips + scrolls, surface is taller than the track', () => {
    const { getByTestId } = render(<HistoryBar {...props(60)} />);
    const track = getByTestId('history-track');
    const surface = getByTestId('history-surface');
    // Hard clip horizontally always; vertical becomes scrollable when lanes overflow.
    expect(track.style.overflowX).toBe('hidden');
    expect(track.style.overflowY).toBe('auto');
    // Surface height is derived from the lane stack and exceeds the track.
    const h = px(surface.style.height);
    expect(h).toBeGreaterThan(400);
    expect(surface.style.minHeight).toBe('100%');
  });

  it('small-lane graph: no scroll-forcing surface, track stays clipped', () => {
    const { getByTestId } = render(<HistoryBar {...props(2)} />);
    const track = getByTestId('history-track');
    const surface = getByTestId('history-surface');
    // Clip always holds, but no vertical scrollbar for the normal case.
    expect(track.style.overflowX).toBe('hidden');
    expect(track.style.overflowY).toBe('hidden');
    // Surface just fills the track (no lane-count-derived min height).
    expect(px(surface.style.height)).toBeLessThanOrEqual(200);
  });
});

import { afterAll as __aa, mock as __mk } from 'bun:test';
__aa(() => __mk.restore());
