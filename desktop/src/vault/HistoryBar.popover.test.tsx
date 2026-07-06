import { mock } from 'bun:test';
import { act, cleanup, fireEvent, render } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from '../test-shim';
import type { BranchGraphData, GraphNode, HistEvent, VaultStatus } from '../lib/api';
import { buildEvents } from './history';
import HistoryBar, { type HistoryBarProps } from './HistoryBar';

const revealPath = vi.fn(async () => {});
mock.module('../lib/api', () => ({ api: { revealPath: (...a: unknown[]) => revealPath(...(a as [])) } }));

// Fake timers throughout: the hover intent delay + close grace are real
// setTimeouts in the component, so we drive them deterministically (and stay
// immune to ambient timer state other files leave behind in the shared process).
beforeEach(() => vi.useFakeTimers());
afterEach(() => { cleanup(); vi.useRealTimers(); });

// Advance fake timers by `ms` and drain the microtasks the fired callbacks kick
// off (loadDiff promise resolutions), all inside act so React flushes.
const tick = async (ms: number) => { await act(async () => { await vi.advanceTimersByTimeAsync(ms); }); };

const NOW = 1_700_000_000_000;
const rawEvents: HistEvent[] = [
  { id: 'r1', ts: 1_699_999_000, lamport: 1, kind: 'create', path: 'README.md', branch_id: 'main' },
  { id: 'r2', ts: 1_699_999_500, lamport: 2, kind: 'edit', path: 'notes.md', branch_id: 'main' },
];
const status: VaultStatus = { id: 'v1', vault_id: 'vid', rows: 2, files: 2, head: 'h', listening_ticket: 'asp1abc', peers: [], last_ts: 1 };

const props = (over: Partial<HistoryBarProps> = {}): HistoryBarProps => ({
  events: buildEvents(rawEvents),
  histRaw: rawEvents,
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
  barHeight: 150,
  animate: true,
  graph: { nodes: [], branches: [{ id: 'main', name: 'main', parent: null, head_commit: null, lane: 0, current: true }], tags: [] },
  currentBranch: 'main',
  onCheckoutBranch: vi.fn(),
  onCreateTag: vi.fn(),
  onDeleteTag: vi.fn(),
  loadDiff: vi.fn(async () => ({ path: 'README.md', kind: 'edit', before: 'a\n', after: 'a\nb\n' })),
  onTabHistory: vi.fn(),
  onTabLog: vi.fn(),
  onNow: vi.fn(),
  ...over,
});

// jsdom getBoundingClientRect is all-zero; that's fine — popoverPosition itself
// is exercised in timeline.test.ts. Here we only verify open/close/swap wiring.
const dots = (c: HTMLElement) => Array.from(c.querySelectorAll('[data-testid="commit-dot"]')) as HTMLElement[];

describe('HistoryBar diff popover', () => {
  it('opens on hover after the intent delay, streaming the diff in, and closes on leave', async () => {
    const loadDiff = vi.fn(async () => ({ path: 'README.md', kind: 'edit', before: 'a\n', after: 'a\nb\n' }));
    const { container, queryByTestId, getByTestId } = render(<HistoryBar {...props({ loadDiff })} />);
    const dot = dots(container)[0];

    fireEvent.mouseEnter(dot);
    await tick(100); // still within the 250ms intent delay
    expect(queryByTestId('diff-popover')).toBeNull();
    expect(loadDiff).not.toHaveBeenCalled();

    await tick(200); // now past the delay → opens + fetch resolves
    expect(getByTestId('diff-popover')).toBeTruthy();
    expect(loadDiff).toHaveBeenCalledTimes(1);
    expect(getByTestId('diff-added').textContent).toContain('b'); // the added line

    fireEvent.mouseLeave(dot);
    await tick(200); // past the 150ms close grace
    expect(queryByTestId('diff-popover')).toBeNull();
  });

  it('cancels a drive-by hover (leave before the intent delay = no popover, no fetch)', async () => {
    const loadDiff = vi.fn(async () => ({ path: 'README.md', kind: 'edit', before: 'a\n', after: 'a\nb\n' }));
    const { container, queryByTestId } = render(<HistoryBar {...props({ loadDiff })} />);
    const dot = dots(container)[0];
    fireEvent.mouseEnter(dot);
    fireEvent.mouseLeave(dot); // before HOVER_DELAY
    await tick(400);
    expect(queryByTestId('diff-popover')).toBeNull();
    expect(loadDiff).not.toHaveBeenCalled();
  });

  it('stays open when the pointer moves into the popover, and closes on leaving it', async () => {
    const { container, getByTestId, queryByTestId } = render(<HistoryBar {...props()} />);
    const dot = dots(container)[0];
    fireEvent.mouseEnter(dot);
    await tick(300);
    const popover = getByTestId('diff-popover');

    // Leave the dot but immediately enter the popover → the grace close is cancelled.
    fireEvent.mouseLeave(dot);
    fireEvent.mouseEnter(popover);
    await tick(300);
    expect(getByTestId('diff-popover')).toBeTruthy(); // hover-persist

    fireEvent.mouseLeave(popover);
    await tick(300);
    expect(queryByTestId('diff-popover')).toBeNull();
  });

  it('swapping dots re-anchors + re-fetches, and a LATE stale resolution cannot clobber the new one', async () => {
    // Dot 1's loadDiff resolves LATE; dot 2's resolves normally. The stale-guard
    // (fetchSeq) must drop the late one so it never overwrites dot 2's content.
    let releaseFirst: (v: { path: string; kind: string; before: string; after: string }) => void = () => {};
    const loadDiff = vi.fn((ev: { path: string }) => {
      if (ev.path === 'README.md') return new Promise<{ path: string; kind: string; before: string; after: string }>((res) => { releaseFirst = res; });
      return Promise.resolve({ path: 'notes.md', kind: 'edit', before: 'x\n', after: 'x\nSECOND\n' });
    });
    const { container, getByTestId } = render(<HistoryBar {...props({ loadDiff })} />);
    const [d1, d2] = dots(container);

    fireEvent.mouseEnter(d1);
    await tick(300); // opens on dot 1, first fetch in-flight (unresolved)
    expect(getByTestId('diff-loading')).toBeTruthy();

    fireEvent.mouseLeave(d1);
    fireEvent.mouseEnter(d2);
    await tick(300); // swaps to dot 2, its fetch resolves with SECOND
    expect(getByTestId('diff-added').textContent).toContain('SECOND');

    // The stale dot-1 fetch resolves LATE — must NOT clobber dot 2.
    await act(async () => { releaseFirst({ path: 'README.md', kind: 'edit', before: 'a\n', after: 'a\nSTALE\n' }); await Promise.resolve(); });
    expect(getByTestId('diff-added').textContent).toContain('SECOND');
    expect(getByTestId('diff-added').textContent).not.toContain('STALE');
    expect(loadDiff).toHaveBeenCalledTimes(2); // one fetch per dot, no storm
  });

  it('click pins the popover (survives mouse-leave) and scrubs the playhead; Escape unpins', async () => {
    const setPlayhead = vi.fn();
    const { container, getByTestId, queryByTestId } = render(<HistoryBar {...props({ setPlayhead })} />);
    const dot = dots(container)[0];

    await act(async () => { fireEvent(dot, new MouseEvent('pointerdown', { clientX: 10, bubbles: true })); await Promise.resolve(); });
    expect(setPlayhead).toHaveBeenCalled(); // click still scrubs
    expect(getByTestId('diff-popover')).toBeTruthy();

    // Pinned: leaving the dot does NOT close it.
    fireEvent.mouseLeave(dot);
    await tick(400);
    expect(getByTestId('diff-popover')).toBeTruthy();

    await act(async () => { fireEvent.keyDown(document, { key: 'Escape' }); });
    expect(queryByTestId('diff-popover')).toBeNull();
  });

  it('a pinned popover closes on an outside pointer-down', async () => {
    const { container, getByTestId, queryByTestId } = render(<HistoryBar {...props()} />);
    const dot = dots(container)[0];
    await act(async () => { fireEvent(dot, new MouseEvent('pointerdown', { clientX: 10, bubbles: true })); await Promise.resolve(); });
    expect(getByTestId('diff-popover')).toBeTruthy();

    await act(async () => { fireEvent(document.body, new MouseEvent('pointerdown', { bubbles: true })); });
    expect(queryByTestId('diff-popover')).toBeNull();
  });

  it('the old top-middle modal is gone: no diff-popup testid after a dot click', async () => {
    const { container, queryByTestId } = render(<HistoryBar {...props()} />);
    const dot = dots(container)[0];
    await act(async () => { fireEvent(dot, new MouseEvent('pointerdown', { clientX: 10, bubbles: true })); await Promise.resolve(); });
    expect(queryByTestId('diff-popup')).toBeNull();
  });

  it('collapsed-group micro-dots get NO diff popover (they expand the group instead)', async () => {
    // A dependabot/* farm whose members' spans all OVERLAP one window → many
    // packed lanes → the graph defaults collapsed and members render as
    // micro-dots. Hovering a micro-dot must not open a popover.
    const startS = 1_699_990_000;
    const endS = 1_699_999_000;
    const branches: BranchGraphData['branches'] = [{ id: 'main', name: 'main', parent: null, head_commit: null, lane: 0, current: true }];
    const nodes: GraphNode[] = [];
    const farmRaw: HistEvent[] = [{ id: 'r0', ts: startS, lamport: 1, kind: 'create', path: 'README.md', branch_id: 'main' }];
    for (let i = 0; i < 12; i++) {
      branches.push({ id: `dep${i}`, name: `dependabot/npm/${i}`, parent: 'main', head_commit: null, lane: i + 1, current: false });
      nodes.push({ commit_id: `dep${i}-a`, branch_id: `dep${i}`, parents: [], ts: startS, lamport: 1, label: 's', lane: 0 });
      nodes.push({ commit_id: `dep${i}-h`, branch_id: `dep${i}`, parents: [`dep${i}-a`], ts: endS, lamport: 2, label: 'e', lane: 0 });
      farmRaw.push({ id: `da${i}`, ts: startS + i, lamport: 2 + i, kind: 'edit', path: `dependabot/npm/${i}.md`, branch_id: `dep${i}` });
      farmRaw.push({ id: `db${i}`, ts: endS - i, lamport: 20 + i, kind: 'edit', path: `dependabot/npm/${i}.md`, branch_id: `dep${i}` });
    }
    const graph: BranchGraphData = { nodes, branches, tags: [] };
    const loadDiff = vi.fn(async () => ({ path: 'x', kind: 'edit', before: '', after: '' }));
    const { container, queryByTestId } = render(<HistoryBar {...props({ events: buildEvents(farmRaw), histRaw: farmRaw, graph, loadDiff })} />);

    const micro = container.querySelector('[data-testid="group-microdot"]') as HTMLElement | null;
    expect(micro).toBeTruthy();
    // Micro-dots have no mouseEnter popover handler; hovering does nothing.
    fireEvent.mouseEnter(micro!);
    await tick(400);
    expect(queryByTestId('diff-popover')).toBeNull();
    expect(loadDiff).not.toHaveBeenCalled();
  });
});
import { afterAll as __aa, mock as __mk } from 'bun:test';
__aa(() => __mk.restore());
