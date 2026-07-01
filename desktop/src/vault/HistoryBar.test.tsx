import { mock } from 'bun:test';
import { act, cleanup, fireEvent, render } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from '../test-shim';
import type { HistEvent, VaultStatus } from '../lib/api';
import { buildEvents } from './history';
import HistoryBar, { type HistoryBarProps } from './HistoryBar';

const revealPath = vi.fn(async () => {});
mock.module('../lib/api', () => ({ api: { revealPath: (...a: unknown[]) => revealPath(...(a as [])) } }));

afterEach(() => { cleanup(); vi.useRealTimers(); });

const NOW = 1_700_000_000_000;
const rawEvents: HistEvent[] = [
  { id: 'r1', ts: 1_699_999_000, lamport: 1, kind: 'create', path: 'README.md', branch_id: 'main' },
  { id: 'r2', ts: 1_699_999_500, lamport: 2, kind: 'edit', path: 'README.md', branch_id: 'main' },
  { id: 'r3', ts: 1_699_999_800, lamport: 3, kind: 'rename', path: 'a.md', branch_id: 'main' },
  { id: 'r4', ts: 1_699_999_900, lamport: 4, kind: 'delete', path: 'b.md', branch_id: 'main' },
];
const status: VaultStatus = { id: 'v1', vault_id: 'vid', rows: 4, files: 2, head: 'h', listening_ticket: 'asp1abc', peers: ['ssh-ed25519 PEER x'], last_ts: 1 };

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

const rectTrack = (el: HTMLElement) => {
  (el.getBoundingClientRect as unknown) = () => ({ left: 0, top: 0, width: 100, height: 40, right: 100, bottom: 40, x: 0, y: 0, toJSON() {} });
};

describe('HistoryBar', () => {
  it('sizes the bar from barHeight and reflects animate in the transition', () => {
    const { container, rerender } = render(<HistoryBar {...props({ barHeight: 220, animate: true })} />);
    const bar = container.firstChild as HTMLElement;
    expect(bar.style.height).toBe('220px');
    expect(bar.style.transition).toBe('height .16s ease');

    // While dragging (animate=false) the height jumps with no transition lag.
    rerender(<HistoryBar {...props({ barHeight: 300, animate: false })} />);
    expect(bar.style.height).toBe('300px');
    expect(bar.style.transition).toBe('none');
  });

  it('switches tabs and reports rows', () => {
    const onTabHistory = vi.fn();
    const onTabLog = vi.fn();
    const { getByText } = render(<HistoryBar {...props({ onTabHistory, onTabLog })} />);
    fireEvent.click(getByText('History'));
    fireEvent.click(getByText('Log'));
    expect(onTabHistory).toHaveBeenCalled();
    expect(onTabLog).toHaveBeenCalled();
    expect(getByText('4 rows')).toBeTruthy();
  });

  it('zoom buttons and Now call their handlers', () => {
    const setView = vi.fn();
    const onNow = vi.fn();
    const { getByTitle, getByText } = render(<HistoryBar {...props({ setView, onNow })} />);
    fireEvent.click(getByTitle('Zoom out'));
    fireEvent.click(getByTitle('Zoom in'));
    fireEvent.click(getByText('Now'));
    expect(setView).toHaveBeenCalledTimes(2);
    expect(onNow).toHaveBeenCalled();
  });

  it('clicking the track (no drag) sets the playhead; dragging pans the view', () => {
    const setPlayhead = vi.fn();
    const setView = vi.fn();
    const { getByTestId } = render(<HistoryBar {...props({ setPlayhead, setView })} />);
    const track = getByTestId('history-track');
    rectTrack(track);

    // No-move click → playhead.
    fireEvent(track, new MouseEvent('pointerdown', { clientX: 30, bubbles: true }));
    fireEvent(document, new MouseEvent('pointerup', { clientX: 30, bubbles: true }));
    expect(setPlayhead).toHaveBeenCalled();

    // Drag → pan (setView).
    fireEvent(track, new MouseEvent('pointerdown', { clientX: 30, bubbles: true }));
    fireEvent(document, new MouseEvent('pointermove', { clientX: 80, bubbles: true }));
    fireEvent(document, new MouseEvent('pointerup', { clientX: 80, bubbles: true }));
    expect(setView).toHaveBeenCalled();
  });

  it('wheel zooms, the handle drags, and a tick jumps the playhead', () => {
    const setView = vi.fn();
    const setPlayhead = vi.fn();
    const { getByTestId, container } = render(<HistoryBar {...props({ setView, setPlayhead })} />);
    const track = getByTestId('history-track');
    rectTrack(track);

    track.dispatchEvent(new WheelEvent('wheel', { deltaY: 10, clientX: 50, bubbles: true, cancelable: true }));
    expect(setView).toHaveBeenCalled();

    const handle = container.querySelector('[style*="ew-resize"]') as HTMLElement;
    fireEvent(handle, new MouseEvent('pointerdown', { clientX: 40, bubbles: true }));
    fireEvent(document, new MouseEvent('pointermove', { clientX: 60, bubbles: true }));
    fireEvent(document, new MouseEvent('pointerup', { clientX: 60, bubbles: true }));

    const tick = container.querySelector('div[title]') as HTMLElement;
    fireEvent(tick, new MouseEvent('pointerdown', { clientX: 10, bubbles: true }));
    expect(setPlayhead).toHaveBeenCalled();
  });

  it('renders the time-travel pill and visible-row ratio when scrubbing', () => {
    const { getByText } = render(<HistoryBar {...props({ timeTravel: true, playhead: NOW - 600000 })} />);
    expect(getByText(/\/ 4 rows/)).toBeTruthy();
  });

  it('single-clicks the location path to copy it (with feedback), without revealing', () => {
    vi.useFakeTimers();
    revealPath.mockClear();
    const writeText = vi.fn();
    Object.assign(navigator, { clipboard: { writeText } });
    const { getByText, queryByText } = render(<HistoryBar {...props({ location: '/home/me/vault' })} />);

    fireEvent.click(getByText('/home/me/vault'));
    expect(writeText).toHaveBeenCalledWith('/home/me/vault');
    expect(revealPath).not.toHaveBeenCalled();
    expect(getByText('Copied path')).toBeTruthy();

    act(() => vi.advanceTimersByTime(1200)); // feedback reverts
    expect(queryByText('Copied path')).toBeNull();
    expect(getByText('/home/me/vault')).toBeTruthy();
  });

  it('clicking the folder icon opens the file manager (no copy)', () => {
    revealPath.mockClear();
    const writeText = vi.fn();
    Object.assign(navigator, { clipboard: { writeText } });
    const { getByTitle } = render(<HistoryBar {...props({ location: '/home/me/vault' })} />);

    fireEvent.click(getByTitle('Open in file manager'));
    expect(revealPath).toHaveBeenCalledWith('/home/me/vault');
    expect(writeText).not.toHaveBeenCalled();
  });

  it('right-clicks the location to open a menu whose items copy and reveal', () => {
    revealPath.mockClear();
    const writeText = vi.fn();
    Object.assign(navigator, { clipboard: { writeText } });
    const { getByText, getByTitle, queryByText } = render(<HistoryBar {...props({ location: '/home/me/vault' })} />);

    // Right-click the folder icon → context menu appears with both actions.
    fireEvent.contextMenu(getByTitle('Open in file manager'), { clientX: 20, clientY: 20 });
    // The context menu rows are text-only (no leading icons).
    const reveal = getByText('Open in file manager').closest('.asp-hover-soft') as HTMLElement;
    expect(reveal.querySelector('svg')).toBeNull();
    fireEvent.click(getByText('Open in file manager'));
    expect(revealPath).toHaveBeenCalledWith('/home/me/vault');
    expect(queryByText('Open in file manager')).toBeNull(); // menu closes after a choice

    // Reopen via the path text and copy.
    fireEvent.contextMenu(getByText('/home/me/vault'), { clientX: 20, clientY: 20 });
    fireEvent.click(getByText('Copy path'));
    expect(writeText).toHaveBeenCalledWith('/home/me/vault');
  });

  it('does not attach copy/reveal behavior in web mode (locationIsPath=false)', () => {
    revealPath.mockClear();
    const writeText = vi.fn();
    Object.assign(navigator, { clipboard: { writeText } });
    const { getByText, queryByTitle } = render(<HistoryBar {...props({ locationIsPath: false, location: 'web vault' })} />);

    const span = getByText('web vault');
    fireEvent.click(span);
    fireEvent.contextMenu(span, { clientX: 20, clientY: 20 });
    expect(queryByTitle('Open in file manager')).toBeNull();
    expect(queryByTitle('Click to copy path')).toBeNull();
    expect(getByText('web vault')).toBeTruthy(); // no context menu opened
    expect(writeText).not.toHaveBeenCalled();
    expect(revealPath).not.toHaveBeenCalled();
  });

  it('swallows clipboard errors when copying the log', () => {
    Object.assign(navigator, { clipboard: { writeText: () => { throw new Error('denied'); } } });
    const { getByTitle } = render(<HistoryBar {...props({ histOpen: false, logOpen: true })} />);
    expect(() => fireEvent.click(getByTitle('Copy all'))).not.toThrow();
  });

  it('shows the log panel, copies all, and copies a single line via context menu', () => {
    vi.useFakeTimers();
    const writeText = vi.fn();
    Object.assign(navigator, { clipboard: { writeText } });
    const { getByTitle, getByText, container } = render(<HistoryBar {...props({ histOpen: false, logOpen: true })} />);
    expect(getByText(/events$/)).toBeTruthy();
    fireEvent.click(getByTitle('Copy all'));
    expect(writeText).toHaveBeenCalled();
    vi.advanceTimersByTime(1400); // "copied" resets

    // right-click a log line → context menu → Copy line / Copy all
    const line = container.querySelector('.asp-hover-soft') as HTMLElement;
    fireEvent.contextMenu(line, { clientX: 10, clientY: 10 });
    fireEvent.click(getByText('Copy line'));
    fireEvent.contextMenu(line, { clientX: 10, clientY: 10 });
    fireEvent.click(getByText('Copy all'));
    expect(writeText).toHaveBeenCalledTimes(3);
  });
});
import { afterAll as __aa, mock as __mk } from 'bun:test';
__aa(() => __mk.restore());
