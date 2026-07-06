// The bottom bar: a status row (location · fingerprint · live/time-travel pill)
// with History / Log tabs that expand a panel. History is now the unified
// time-travel + branch-network view: events are dots positioned by time (x) on
// their branch's lane (y), fork edges curve where a branch diverged, and tags
// flag named moments. Pan, scrub, zoom and the playhead work exactly as before —
// branching is layered on top. Log shows real sync events. All color is
// theme-driven via CSS variables.
import React, { useEffect, useMemo, useRef, useState } from 'react';
import { api } from '../lib/api';
import type { BranchGraphData, HistEvent, VaultStatus } from '../lib/api';
import {
  axisTicksFor,
  clampView,
  DAY,
  defaultView,
  fmtFull,
  toPct,
  type TrackEvent,
  type View,
  zoomAround,
  zoomKeepingFocus,
} from './history';
import { declutterLabels, FISHEYE_THRESHOLD, fisheyeLaneYs, forkEdges, laneColor, laneCountOf, laneForEvent, laneGeometry, laneIndex, nodesToEvents, packedLanes } from './timeline';
import { lineDiff } from './diffLines';
import * as Icon from './icons';
import { deriveLog, logColor, logText } from './log';

const MAX_TICKS = 240;

export interface HistoryBarProps {
  events: TrackEvent[];
  histRaw: HistEvent[];
  view: View;
  setView: (v: View) => void;
  playhead: number | null;
  setPlayhead: (p: number | null) => void;
  now: number;
  accent: string;
  accentSoft: string;
  timeTravel: boolean;
  location: string;
  locationIsPath: boolean;
  fingerprint: string;
  status?: VaultStatus;
  identity: string;
  histOpen: boolean;
  logOpen: boolean;
  barHeight: number;
  animate: boolean;
  // Branch/tag graph (lanes + fork edges + tags). Null before it loads.
  graph: BranchGraphData | null;
  currentBranch: string;
  onCheckoutBranch: (branchId: string) => void;
  onCreateTag: (name: string, tsMs: number) => void;
  onDeleteTag: (tagId: string) => void;
  // Load the before/after content for a history event, for the diff popup.
  loadDiff: (ev: TrackEvent) => Promise<{ path: string; kind: string; before: string; after: string } | null>;
  onTabHistory: () => void;
  onTabLog: () => void;
  onNow: () => void;
}

const colorForKind = (kind: string, accent: string): string =>
  kind === 'create' ? '#3fa45a' : kind === 'edit' ? accent : kind === 'rename' ? '#d9a93d' : '#d96a6a';

export default function HistoryBar(props: HistoryBarProps) {
  const { histRaw, view, setView, playhead, setPlayhead, now, accent, accentSoft, timeTravel } = props;
  // Per-event history drives the dots; where it's unavailable (web degrades
  // history() to empty), fall back to the graph's coarsened commits so the
  // timeline-as-network-graph is never blank.
  const events = useMemo(
    () => (props.events.length ? props.events : nodesToEvents(props.graph)),
    [props.events, props.graph],
  );
  const { location, locationIsPath, fingerprint, status, identity, histOpen, logOpen, graph, currentBranch } = props;

  const trackRef = useRef<HTMLDivElement | null>(null);
  const viewRef = useRef(view);
  const nowRef = useRef(now);
  const playheadRef = useRef(playhead);
  viewRef.current = view;
  nowRef.current = now;
  playheadRef.current = playhead;

  const [logCopied, setLogCopied] = useState(false);
  const [logCtx, setLogCtx] = useState<{ x: number; y: number; line: string } | null>(null);
  // Inline "name this moment" input; null when not tagging. `ts` is the instant.
  const [tagging, setTagging] = useState<{ ts: number } | null>(null);
  const [tagName, setTagName] = useState('');
  // Measured track pixel size — lane y and SVG edge coords are in px.
  const [trackSize, setTrackSize] = useState({ w: 640, h: 96 });
  // Wave-B vertical fisheye: the cursor's y within the track (px), or null when
  // not hovering. Drives lane magnification only when rows are thin; reads ONLY
  // mouse-Y, never the time (x) axis. rAF-throttled + rounded to 2px steps.
  const [focusY, setFocusY] = useState<number | null>(null);
  // Layout mode: 'time' spaces dots by wall-clock time (default); 'seq' spaces them
  // evenly by edit order (a cleaner, uniform view when edits cluster in time).
  const [seqMode, setSeqMode] = useState(false);
  // The "what changed" diff popup for a clicked dot, and its loading flag.
  const [diff, setDiff] = useState<{ path: string; kind: string; before: string; after: string } | null>(null);
  const [diffBusy, setDiffBusy] = useState(false);
  // Pending tag deletion awaiting confirmation.
  const [confirmTag, setConfirmTag] = useState<{ tag_id: string; name: string } | null>(null);

  // Location (desktop only): the folder ICON opens the OS file manager; a single
  // click on the path TEXT copies the full path (with brief feedback); a
  // right-click anywhere on the location opens a small context menu with both.
  const [pathCopied, setPathCopied] = useState(false);
  const [locCtx, setLocCtx] = useState<{ x: number; y: number } | null>(null);
  const copyPath = () => {
    copy(location);
    setPathCopied(true);
    setTimeout(() => setPathCopied(false), 1200);
  };
  const revealLoc = () => {
    void api.revealPath(location);
  };
  const openLocCtx = (e: React.MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    setLocCtx({ x: Math.min(e.clientX, window.innerWidth - 212), y: Math.min(e.clientY, window.innerHeight - 90) });
  };

  // ---- geometry ----
  const span = view.end - view.start;
  const playT = playhead == null ? now : playhead;
  const filterTs = timeTravel ? playhead : null;
  const axisTicks = useMemo(() => axisTicksFor(view), [view]);

  // Lanes (branches) with client-side PACKED display lanes (the Rust creation-order
  // lane is ignored for layout): stale branches share lanes so a 128-branch clone
  // collapses to a legible handful. Geometry uses the packed lane count, not the
  // branch count.
  const lanes = useMemo(() => packedLanes(graph), [graph]);
  const laneIdx = useMemo(() => laneIndex(lanes), [lanes]);
  const laneCount = useMemo(() => laneCountOf(lanes), [lanes]);
  const edges = useMemo(() => forkEdges(lanes, events), [lanes, events]);
  // Uniform base geometry (surface height, threshold check) + the fisheye seam.
  // The current-branch lane is pinned alongside main so both anchors stay
  // readable under magnification. `geom` is the SINGLE lane→pixel-y seam: guides,
  // polylines, dots and labels all position through geom.y so the whole lane
  // moves coherently. It is an exact identity transform unless rows are thin AND
  // the cursor is over the track (focusY != null) — normal vaults see no change.
  const baseGeom = useMemo(() => laneGeometry(laneCount, trackSize.h), [laneCount, trackSize.h]);
  const curLane = laneIdx.get(currentBranch);
  const geom = useMemo(
    () => fisheyeLaneYs(laneCount, trackSize.h, focusY, { pinned: curLane != null ? [0, 1, curLane] : [0, 1] }),
    [laneCount, trackSize.h, focusY, curLane],
  );
  const fisheyeActive = baseGeom.rowH < FISHEYE_THRESHOLD && laneCount > 1;
  const tags = graph?.tags ?? [];

  // Cap rendered tick nodes: a vault import clusters thousands of events at one
  // instant — rendering them all is a render bomb (they overlap to one pixel).
  const inView = events.filter((e) => e.ts >= view.start - span * 0.03 && e.ts <= view.end + span * 0.03);
  const sampled = inView.length > MAX_TICKS ? inView.filter((_, i) => i % Math.ceil(inView.length / MAX_TICKS) === 0) : inView;
  const visibleRows = filterTs == null ? events.length : events.filter((e) => e.ts <= filterTs).length;

  // Rendered dots + an x-position function, per layout mode. In 'time' mode dots
  // sit at their wall-clock position within the view (pan/zoom apply). In 'seq'
  // mode all (sampled) events are spaced evenly by edit order — a uniform view.
  const rendered = useMemo(() => {
    if (!seqMode) return sampled.map((e) => ({ e, xPct: toPct(e.ts, view) }));
    const list = events.length > MAX_TICKS ? events.filter((_, i) => i % Math.ceil(events.length / MAX_TICKS) === 0) : events;
    const n = Math.max(1, list.length - 1);
    return list.map((e, i) => ({ e, xPct: list.length <= 1 ? 50 : (i / n) * 100 }));
  }, [seqMode, sampled, events, view]);

  // Percent-x for an arbitrary wall-clock instant (playhead / now / tags / edges).
  // In 'seq' mode it snaps to the nearest rendered edit's slot.
  const xAt = (ts: number): number => {
    if (!seqMode) return toPct(ts, view);
    if (rendered.length === 0) return 100;
    if (ts >= rendered[rendered.length - 1].e.ts) return 100;
    let x = rendered[0].xPct;
    for (const r of rendered) {
      if (r.e.ts <= ts) x = r.xPct;
      else break;
    }
    return x;
  };

  // Nearest rendered edit's ts to a percent-x — for click/scrub in 'seq' mode.
  const tsAtPct = (pct: number): number => {
    if (rendered.length === 0) return nowRef.current;
    let best = rendered[0];
    for (const r of rendered) if (Math.abs(r.xPct - pct) < Math.abs(best.xPct - pct)) best = r;
    return best.e.ts;
  };

  const playPct = Math.max(0, Math.min(100, xAt(playT)));
  const nowPct = Math.max(0, Math.min(100, xAt(now)));

  // Measure the track so lane y-positions and fork-edge coordinates are in px.
  useEffect(() => {
    const el = trackRef.current;
    if (!el || !histOpen) return;
    const update = () => setTrackSize({ w: el.clientWidth, h: el.clientHeight });
    update();
    const ro = new ResizeObserver(update);
    ro.observe(el);
    return () => ro.disconnect();
  }, [histOpen, props.barHeight]);

  const seqModeRef = useRef(seqMode);
  seqModeRef.current = seqMode;

  // ---- fisheye focus (vertical hover) ----
  // Read only the cursor's Y, round to 2px, and commit at most once per frame so
  // magnification tracks the cursor without a re-render per pixel. When rows are
  // already comfortable it's a no-op (never sets focusY → zero extra renders).
  const fisheyeRef = useRef(fisheyeActive);
  fisheyeRef.current = fisheyeActive;
  const focusRaf = useRef<number | null>(null);
  const pendingFocus = useRef<number | null>(null);
  useEffect(() => () => { if (focusRaf.current != null) cancelAnimationFrame(focusRaf.current); }, []);
  const onTrackMove = (e: React.MouseEvent) => {
    if (!fisheyeRef.current) return; // identity regime → don't churn state
    const el = trackRef.current;
    if (!el) return;
    const r = el.getBoundingClientRect();
    pendingFocus.current = Math.round((e.clientY - r.top) / 2) * 2;
    if (focusRaf.current == null) {
      focusRaf.current = requestAnimationFrame(() => {
        focusRaf.current = null;
        setFocusY(pendingFocus.current);
      });
    }
  };
  const onTrackLeave = () => {
    if (focusRaf.current != null) { cancelAnimationFrame(focusRaf.current); focusRaf.current = null; }
    // Snap back to the uniform layout (see report: SVG + DOM move together, and
    // easing only the DOM half while SVG geometry snaps reads worse than a clean
    // snap; the rAF-throttled hover already reads smooth).
    setFocusY(null);
  };

  // ---- track interaction ----
  const onTrackDown = (e: React.PointerEvent) => {
    const el = trackRef.current;
    if (!el) return;
    // In 'seq' mode there is no time axis to pan — a click just scrubs to the
    // nearest edit; dragging does the same, live.
    if (seqModeRef.current) {
      const scrub = (ev: PointerEvent | React.PointerEvent) => {
        const r = el.getBoundingClientRect();
        const pct = ((ev.clientX - r.left) / r.width) * 100;
        setPlayhead(Math.min(tsAtPct(pct), nowRef.current));
      };
      scrub(e);
      const move = (ev: PointerEvent) => scrub(ev);
      const up = () => {
        document.removeEventListener('pointermove', move);
        document.removeEventListener('pointerup', up);
      };
      document.addEventListener('pointermove', move);
      document.addEventListener('pointerup', up);
      return;
    }
    const startX = e.clientX;
    const v0 = viewRef.current || defaultView(nowRef.current);
    const span0 = v0.end - v0.start;
    let moved = false;
    const move = (ev: PointerEvent) => {
      const dx = ev.clientX - startX;
      if (Math.abs(dx) > 3) moved = true;
      if (moved) {
        const r = el.getBoundingClientRect();
        const dt = -(dx / r.width) * span0;
        setView(clampView(v0.start + dt, v0.end + dt, nowRef.current));
      }
    };
    const up = (ev: PointerEvent) => {
      document.removeEventListener('pointermove', move);
      document.removeEventListener('pointerup', up);
      if (!moved) {
        const r = el.getBoundingClientRect();
        const t = v0.start + ((ev.clientX - r.left) / r.width) * span0;
        setPlayhead(Math.min(t, nowRef.current));
      }
    };
    document.addEventListener('pointermove', move);
    document.addEventListener('pointerup', up);
  };

  const onHandleDown = (e: React.PointerEvent) => {
    e.stopPropagation();
    const el = trackRef.current;
    if (!el) return;
    const move = (ev: PointerEvent) => {
      const r = el.getBoundingClientRect();
      if (seqModeRef.current) {
        const pct = ((ev.clientX - r.left) / r.width) * 100;
        setPlayhead(Math.min(tsAtPct(pct), nowRef.current));
        return;
      }
      const v = viewRef.current || defaultView(nowRef.current);
      const t = v.start + ((ev.clientX - r.left) / r.width) * (v.end - v.start);
      setPlayhead(Math.max(nowRef.current - 90 * DAY, Math.min(t, nowRef.current)));
    };
    const up = () => {
      document.removeEventListener('pointermove', move);
      document.removeEventListener('pointerup', up);
    };
    document.addEventListener('pointermove', move);
    document.addEventListener('pointerup', up);
  };

  // Click a dot → show the "what changed" diff popup (and scrub to that moment).
  const openDiff = (ev: TrackEvent) => {
    setPlayhead(Math.min(ev.ts, nowRef.current));
    setDiffBusy(true);
    setDiff({ path: ev.path, kind: ev.kind, before: '', after: '' });
    void props
      .loadDiff(ev)
      .then((d) => setDiff(d))
      .catch(() => setDiff(null))
      .finally(() => setDiffBusy(false));
  };

  // Non-passive wheel listener so we can preventDefault and zoom.
  useEffect(() => {
    const el = trackRef.current;
    if (!el) return;
    const handler = (e: WheelEvent) => {
      e.preventDefault();
      const v = viewRef.current || defaultView(nowRef.current);
      const r = el.getBoundingClientRect();
      const f = (e.clientX - r.left) / r.width;
      const factor = e.deltaY > 0 ? 1.2 : 0.82;
      setView(zoomKeepingFocus(v, f, factor, nowRef.current));
    };
    el.addEventListener('wheel', handler, { passive: false });
    return () => el.removeEventListener('wheel', handler);
  }, [histOpen, setView]);

  const zoomBtn = (factor: number) => {
    const v = viewRef.current || defaultView(nowRef.current);
    const c = playheadRef.current != null ? playheadRef.current : nowRef.current;
    setView(zoomAround(v, c, factor, nowRef.current));
  };

  // ---- tags ----
  const beginTag = () => {
    setTagName('');
    setTagging({ ts: playT });
  };
  const commitTag = () => {
    const n = tagName.trim();
    if (n && tagging) props.onCreateTag(n, tagging.ts);
    setTagging(null);
    setTagName('');
  };

  // ---- log ----
  const logLines = useMemo(
    () => (logOpen ? deriveLog(histRaw, status, identity, { now }) : []),
    [logOpen, histRaw, status, identity, now],
  );
  const copy = (text: string) => {
    try {
      void navigator.clipboard?.writeText(text);
    } catch {
      /* ignore */
    }
  };
  const onCopyAll = () => {
    copy(logText(logLines));
    setLogCtx(null);
    setLogCopied(true);
    setTimeout(() => setLogCopied(false), 1400);
  };
  const openLogCtx = (line: string) => (e: React.MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    setLogCtx({ x: Math.min(e.clientX, window.innerWidth - 168), y: Math.min(e.clientY, window.innerHeight - 90), line });
  };

  const tabBase: React.CSSProperties = { display: 'flex', alignItems: 'center', gap: 6, height: 24, padding: '0 11px', border: 'none', background: 'transparent', color: 'var(--text3)', borderRadius: 6, cursor: 'pointer', fontFamily: 'inherit', fontSize: 12, fontWeight: 500 };
  const tabActive: React.CSSProperties = { ...tabBase, background: 'var(--bg)', color: 'var(--text)', boxShadow: '0 1px 2px rgba(0,0,0,0.08)' };
  const barHeight = props.barHeight;

  const pxX = (ts: number) => (xAt(ts) / 100) * trackSize.w;
  const multiLane = lanes.length > 1;

  // Lanes ALWAYS fit the track now (adaptive rowH, no lane-axis scroll — scroll is
  // reserved for time). The surface just fills the track and the track clips BOTH
  // axes so nothing ever paints over the panes above. The surface element is kept
  // as a positioning root for every graph element.
  const surfaceH = geom.contentH; // === trackSize.h

  // Per-BRANCH connecting polylines (each branch's own history reads as one line,
  // even when several stale branches share a display lane) plus the x/ts of each
  // branch's most-recent dot (to anchor + prioritise its label).
  const perBranch = useMemo(() => {
    const byBranch = new Map<string, { xPct: number; ts: number }[]>();
    for (const r of rendered) {
      const b = r.e.branchId;
      if (!byBranch.has(b)) byBranch.set(b, []);
      byBranch.get(b)!.push({ xPct: r.xPct, ts: r.e.ts });
    }
    const polylines = new Map<string, { lane: number; pts: string }>();
    const lastX = new Map<string, number>();
    const lastTs = new Map<string, number>();
    for (const [b, pts] of byBranch) {
      const lane = laneIdx.get(b) ?? 0;
      const y = geom.y(lane);
      const sorted = [...pts].sort((a, c) => a.xPct - c.xPct);
      polylines.set(b, { lane, pts: sorted.map((p) => `${(p.xPct / 100) * trackSize.w},${y}`).join(' ') });
      let mx = -Infinity, mxTs = -Infinity;
      for (const p of pts) { if (p.xPct > mx) mx = p.xPct; if (p.ts > mxTs) mxTs = p.ts; }
      lastX.set(b, (mx / 100) * trackSize.w);
      lastTs.set(b, mxTs);
    }
    return { polylines, lastX, lastTs };
  }, [rendered, laneIdx, geom, trackSize.w]);

  // Pixel-space label declutter: chip per branch tip, prioritised current > tips in
  // the visible time window (recent first) > others, kept only if it doesn't collide
  // with a higher-priority kept chip. Hidden tips fall back to a dot + title hover.
  const labelKeep = useMemo(() => {
    if (!multiLane) return new Set<string>();
    const items = lanes.map((l) => {
      const anchor = perBranch.lastX.get(l.branchId);
      const x = anchor != null ? anchor + 12 : trackSize.w - 4;
      const tip = perBranch.lastTs.get(l.branchId);
      const inWindow = tip != null && tip >= view.start && tip <= view.end;
      // The magnified (hovered) lane's label always wins; the current branch too.
      const focused = geom.active && l.lane === geom.focusLane;
      const priority =
        focused || l.branchId === currentBranch ? Number.MAX_SAFE_INTEGER : inWindow ? 1e15 + (tip ?? 0) : (tip ?? 0);
      const w = Math.min(140, 16 + l.name.length * 6.5);
      return { id: l.branchId, x, y: geom.y(l.lane) - 9, w, h: 18, priority };
    });
    return declutterLabels(items);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [lanes, perBranch, geom, trackSize.w, view.start, view.end, currentBranch, multiLane]);

  return (
    <div style={{ flex: 'none', height: barHeight, background: 'var(--bg-sub)', borderTop: '1px solid var(--line)', display: 'flex', flexDirection: 'column', userSelect: 'none', transition: props.animate ? 'height .16s ease' : 'none' }}>
      <div style={{ display: 'flex', alignItems: 'center', height: 38, padding: '0 9px 0 15px', gap: 10, flex: 'none' }}>
        <span
          onClick={locationIsPath ? revealLoc : undefined}
          onContextMenu={locationIsPath ? openLocCtx : undefined}
          title={locationIsPath ? 'Open in file manager' : undefined}
          style={{ display: 'inline-flex', flex: 'none', color: 'var(--faint2)', cursor: locationIsPath ? 'pointer' : 'default' }}
        >
          {locationIsPath ? <Icon.FolderIcon size={12} stroke="var(--faint2)" /> : <Icon.GlobeIcon size={12} stroke="var(--faint2)" />}
        </span>
        <span
          onClick={locationIsPath ? copyPath : undefined}
          onContextMenu={locationIsPath ? openLocCtx : undefined}
          title={locationIsPath ? 'Click to copy path' : undefined}
          style={{ fontFamily: locationIsPath ? "'JetBrains Mono', monospace" : 'inherit', fontSize: 12, color: pathCopied ? accent : 'var(--text2)', maxWidth: 190, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', cursor: locationIsPath ? 'pointer' : 'default' }}
        >{locationIsPath && pathCopied ? 'Copied path' : location}</span>
        <span style={{ fontFamily: "'JetBrains Mono', monospace", fontSize: 10.5, color: 'var(--faint2)', flex: 'none', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{fingerprint}</span>
        {status && (
          <span data-testid="file-count" style={{ fontSize: 10.5, color: 'var(--faint2)', flex: 'none', whiteSpace: 'nowrap' }}>· {status.files.toLocaleString()} {status.files === 1 ? 'file' : 'files'}</span>
        )}
        {/* Branch pill: shows the checked-out branch (replaces the old dropdown). */}
        {multiLane && (
          <span data-testid="current-branch-pill" style={{ display: 'inline-flex', alignItems: 'center', gap: 5, fontSize: 11, fontFamily: "'JetBrains Mono', monospace", padding: '2px 9px', borderRadius: 20, flex: 'none', background: 'var(--line)', color: laneColor(laneIdx.get(currentBranch) ?? 0, accent), fontWeight: 600 }}>
            <BranchDot color={laneColor(laneIdx.get(currentBranch) ?? 0, accent)} />
            {lanes.find((l) => l.branchId === currentBranch)?.name ?? 'main'}
          </span>
        )}
        {timeTravel && (
          <span style={{ fontSize: 11, fontFamily: "'JetBrains Mono', monospace", padding: '2px 9px', borderRadius: 20, flex: 'none', background: accentSoft, color: accent, fontWeight: 500 }}>{fmtFull(playT)}</span>
        )}
        <span style={{ flex: 1 }} />
        <div style={{ display: 'flex', background: 'var(--line)', borderRadius: 8, padding: 2, flex: 'none' }}>
          <button onClick={props.onTabHistory} style={histOpen ? tabActive : tabBase}>
            <Icon.ClockIcon size={13} stroke="currentColor" style={{ flex: 'none' }} />
            <span>History</span>
          </button>
          <button onClick={props.onTabLog} style={logOpen ? tabActive : tabBase}>
            <Icon.ListIcon size={13} stroke="currentColor" style={{ flex: 'none' }} />
            <span>Log</span>
          </button>
        </div>
      </div>

      {histOpen && (
        <div style={{ display: 'flex', flexDirection: 'column', flex: 1, minHeight: 0, borderTop: '1px solid var(--line)' }}>
          <div style={{ display: 'flex', alignItems: 'center', gap: 8, padding: '6px 14px 2px' }}>
            <span style={{ flex: 1 }} />
            <span style={{ fontSize: 11, color: 'var(--faint2)', fontVariantNumeric: 'tabular-nums', flex: 'none' }}>{filterTs == null ? `${events.length} rows` : `${visibleRows} / ${events.length} rows`}</span>
            {/* Layout toggle: time-proportional vs even-by-edit spacing. */}
            <div style={{ display: 'flex', flex: 'none', border: '1px solid var(--line)', borderRadius: 7, overflow: 'hidden' }}>
              <button data-testid="mode-time" onClick={() => setSeqMode(false)} title="Space dots by time" style={{ height: 24, padding: '0 8px', border: 'none', borderRight: '1px solid var(--line)', background: seqMode ? 'var(--bg)' : 'var(--line)', color: seqMode ? 'var(--text3)' : 'var(--text)', cursor: 'pointer', fontFamily: 'inherit', fontSize: 11, fontWeight: 500 }}>Time</button>
              <button data-testid="mode-seq" onClick={() => setSeqMode(true)} title="Space dots evenly by edit" style={{ height: 24, padding: '0 8px', border: 'none', background: seqMode ? 'var(--line)' : 'var(--bg)', color: seqMode ? 'var(--text)' : 'var(--text3)', cursor: 'pointer', fontFamily: 'inherit', fontSize: 11, fontWeight: 500 }}>Edits</button>
            </div>
            <button data-testid="tag-here" onClick={beginTag} title="Tag this moment" style={{ display: 'flex', alignItems: 'center', gap: 5, fontFamily: 'inherit', fontSize: 12, fontWeight: 500, color: 'var(--text2)', background: 'var(--bg)', border: '1px solid var(--line)', borderRadius: 7, padding: '4px 10px', cursor: 'pointer', flex: 'none' }}>
              <TagIcon color="var(--faint)" />
              <span>Tag</span>
            </button>
            <div style={{ display: 'flex', flex: 'none', border: '1px solid var(--line)', borderRadius: 7, overflow: 'hidden', opacity: seqMode ? 0.4 : 1, pointerEvents: seqMode ? 'none' : 'auto' }}>
              <button className="asp-icon-btn" onClick={() => zoomBtn(1.8)} title="Zoom out" style={{ width: 26, height: 24, border: 'none', borderRight: '1px solid var(--line)', background: 'var(--bg)', color: 'var(--text3)', cursor: 'pointer', display: 'flex', alignItems: 'center', justifyContent: 'center', padding: 0 }}>
                <Icon.MinusIcon />
              </button>
              <button className="asp-icon-btn" onClick={() => zoomBtn(0.55)} title="Zoom in" style={{ width: 26, height: 24, border: 'none', background: 'var(--bg)', color: 'var(--text3)', cursor: 'pointer', display: 'flex', alignItems: 'center', justifyContent: 'center', padding: 0 }}>
                <Icon.PlusIcon size={14} />
              </button>
            </div>
            <button onClick={props.onNow} style={{ fontFamily: 'inherit', fontSize: 12, fontWeight: 500, color: timeTravel ? 'var(--text2)' : 'var(--faint2)', background: 'var(--bg)', border: '1px solid var(--line)', borderRadius: 7, padding: '4px 12px', cursor: 'pointer', flex: 'none' }}>Now</button>
          </div>

          <div ref={trackRef} data-testid="history-track" onPointerDown={onTrackDown} onMouseMove={onTrackMove} onMouseLeave={onTrackLeave} style={{ position: 'relative', flex: 1, margin: '0 16px 11px', cursor: 'crosshair', touchAction: 'none', overflowX: 'hidden', overflowY: 'hidden' }}>
            {/* Surface: a positioning root filling the track. Lanes always fit (no
                scroll — scroll is reserved for the time axis); the track clips BOTH
                axes so no graph element (dot, label, edge, playhead) ever paints
                over the panes above, even at very high packed lane counts. */}
            <div data-testid="history-surface" style={{ position: 'relative', width: '100%', height: surfaceH, minHeight: '100%' }}>
            {/* SVG overlay: one guide per display lane + per-branch connecting line + fork edges. */}
            <svg width={trackSize.w} height={surfaceH} style={{ position: 'absolute', inset: 0, overflow: 'visible', pointerEvents: 'none' }}>
              {Array.from({ length: laneCount }, (_, lane) => (
                <line key={lane} x1={0} y1={geom.y(lane)} x2={trackSize.w} y2={geom.y(lane)} stroke="var(--line)" strokeWidth={1} />
              ))}
              {/* Connecting line through each branch's dots — a branch reads as continuous. */}
              {[...perBranch.polylines.entries()].map(([b, { lane, pts }]) => (
                <polyline key={b} data-testid="lane-line" points={pts} fill="none" stroke={laneColor(lane, accent)} strokeWidth={2} strokeLinecap="round" opacity={0.55} />
              ))}
              {edges.map((e) => {
                const x = pxX(e.ts);
                const y1 = geom.y(e.fromLane);
                const y2 = geom.y(e.toLane);
                const color = laneColor(e.toLane, accent);
                // Curve from the parent lane down to the branch lane at the fork time.
                const d = `M ${x - 14} ${y1} C ${x} ${y1}, ${x} ${y2}, ${x + 6} ${y2}`;
                return <path key={e.branchId} data-testid="fork-edge" d={d} fill="none" stroke={color} strokeWidth={1.6} opacity={0.85} />;
              })}
            </svg>

            {!seqMode &&
              axisTicks.map((a, i) => (
                <React.Fragment key={i}>
                  <div style={{ position: 'absolute', left: a.pct + '%', top: 0, bottom: 0, width: 1, background: 'var(--line)', opacity: 0.5 }} />
                  <div style={{ position: 'absolute', left: a.pct + '%', bottom: -2, transform: 'translateX(4px)', fontSize: 9.5, color: 'var(--faint2)', fontFamily: "'JetBrains Mono', monospace", whiteSpace: 'nowrap' }}>{a.label}</div>
                </React.Fragment>
              ))}

            {rendered.map((r, i) => {
              const e = r.e;
              const past = e.ts <= playT;
              const c = colorForKind(e.kind, laneColor(laneForEvent(e, laneIdx), accent));
              const y = geom.y(laneForEvent(e, laneIdx));
              return (
                <div
                  key={i}
                  onPointerDown={(ev) => { ev.stopPropagation(); openDiff(e); }}
                  title={`${e.kind} · ${e.path} · ${fmtFull(e.ts)} — click to see the change`}
                  style={{ position: 'absolute', left: r.xPct + '%', top: y, width: 18, height: 18, marginLeft: -9, marginTop: -9, borderRadius: '50%', cursor: 'pointer', display: 'flex', alignItems: 'center', justifyContent: 'center', zIndex: 3 }}
                >
                  <span style={{ width: 9, height: 9, borderRadius: '50%', background: past ? c : 'var(--bg)', border: '1.5px solid ' + c, opacity: past ? 1 : 0.5 }} />
                </div>
              );
            })}

            {/* branch labels: on the RIGHT, next to each branch's most recent dot.
                Decluttered by a pixel collision pass — a hidden label collapses to
                just a dot marker whose native `title` reveals the branch name. */}
            {multiLane &&
              lanes.map((l) => {
                const anchor = perBranch.lastX.get(l.branchId);
                const left = anchor != null ? Math.min(anchor + 12, trackSize.w - 4) : trackSize.w - 4;
                const isCur = l.branchId === currentBranch;
                const color = laneColor(l.lane, accent);
                if (!labelKeep.has(l.branchId)) {
                  // Decluttered tip: dot only; hover shows the name (full hover
                  // affordances arrive with fisheye in wave B).
                  return (
                    <div
                      key={l.branchId}
                      data-testid={`lane-tip-${l.name}`}
                      onPointerDown={(e) => { e.stopPropagation(); if (!isCur) props.onCheckoutBranch(l.branchId); }}
                      title={isCur ? `${l.name} · current branch` : `${l.name} — switch`}
                      style={{ position: 'absolute', left, top: geom.y(l.lane) - 4, width: 8, height: 8, marginLeft: -1, borderRadius: '50%', background: color, cursor: isCur ? 'default' : 'pointer', zIndex: 4 }}
                    />
                  );
                }
                return (
                  <div
                    key={l.branchId}
                    data-testid={`lane-label-${l.name}`}
                    onPointerDown={(e) => { e.stopPropagation(); if (!isCur) props.onCheckoutBranch(l.branchId); }}
                    title={isCur ? 'Current branch' : `Switch to ${l.name}`}
                    style={{ position: 'absolute', left, top: geom.y(l.lane) - 9, height: 18, display: 'flex', alignItems: 'center', gap: 4, padding: '0 6px', fontSize: 10.5, fontWeight: isCur ? 700 : 500, color: isCur ? color : 'var(--faint)', background: 'var(--bg-sub)', borderRadius: 5, cursor: isCur ? 'default' : 'pointer', zIndex: 4, maxWidth: 140, whiteSpace: 'nowrap', overflow: 'hidden', textOverflow: 'ellipsis' }}
                  >
                    <BranchDot color={color} />
                    {l.name}
                  </div>
                );
              })}

            {/* tag flags */}
            {tags.map((t) => {
              const tsMs = t.at_ts * 1000;
              const pct = xAt(tsMs);
              if (pct < -2 || pct > 102) return null;
              return (
                <div
                  key={t.tag_id}
                  data-testid={`tag-${t.name}`}
                  className="asp-tag-flag"
                  onPointerDown={(e) => { e.stopPropagation(); setPlayhead(Math.min(tsMs, nowRef.current)); }}
                  title={`${t.name} · ${fmtFull(tsMs)}`}
                  style={{ position: 'absolute', left: pct + '%', top: 0, zIndex: 6, display: 'flex', alignItems: 'center', gap: 3, transform: 'translateX(-1px)', cursor: 'pointer' }}
                >
                  <span style={{ display: 'inline-flex', alignItems: 'center', gap: 4, background: accent, color: '#fff', fontSize: 10, fontWeight: 600, padding: '1px 6px 1px 5px', borderRadius: 5, whiteSpace: 'nowrap', boxShadow: '0 1px 3px rgba(28,25,23,0.25)' }}>
                    <TagIcon color="#fff" size={9} />
                    {t.name}
                    <span
                      data-testid={`tag-delete-${t.name}`}
                      onPointerDown={(e) => { e.stopPropagation(); setConfirmTag({ tag_id: t.tag_id, name: t.name }); }}
                      style={{ marginLeft: 1, opacity: 0.75, fontSize: 11, lineHeight: 1 }}
                    >×</span>
                  </span>
                </div>
              );
            })}

            <div style={{ position: 'absolute', left: nowPct + '%', top: 0, bottom: 0, width: 0, borderLeft: '1px dashed var(--faint2)' }} />
            <div style={{ position: 'absolute', left: playPct + '%', top: 3, bottom: 3, width: 2, marginLeft: -1, background: accent, borderRadius: 1, zIndex: 5 }}>
              <div onPointerDown={onHandleDown} style={{ position: 'absolute', left: -11, top: '50%', width: 24, height: 28, marginTop: -14, borderRadius: 8, background: accent, border: '2px solid var(--bg)', boxShadow: '0 2px 6px rgba(28,25,23,0.22)', cursor: 'ew-resize' }} />
            </div>

            {/* tag-name input, anchored at the playhead. A full-screen backdrop
                closes it on any outside click. */}
            {tagging && (
              <>
                <div
                  data-testid="tag-backdrop"
                  onPointerDown={(e) => { e.stopPropagation(); setTagging(null); setTagName(''); }}
                  style={{ position: 'fixed', inset: 0, zIndex: 7 }}
                />
              <div style={{ position: 'absolute', left: `min(${Math.max(0, xAt(tagging.ts))}%, calc(100% - 190px))`, top: 2, zIndex: 8, display: 'flex', gap: 4, background: 'var(--bg)', border: '1px solid var(--line)', borderRadius: 8, boxShadow: '0 8px 24px rgba(28,25,23,0.18)', padding: 4 }} onPointerDown={(e) => e.stopPropagation()}>
                <input
                  data-testid="tag-name-input"
                  autoFocus
                  value={tagName}
                  placeholder="name this moment"
                  onChange={(e) => setTagName(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter') commitTag();
                    if (e.key === 'Escape') { setTagging(null); setTagName(''); }
                  }}
                  style={{ width: 130, fontSize: 12, padding: '5px 8px', borderRadius: 6, border: '1px solid var(--line)', background: 'var(--bg-sub)', color: 'var(--text)' }}
                />
                <button data-testid="tag-confirm" onPointerDown={(e) => { e.stopPropagation(); commitTag(); }} style={{ fontSize: 12, padding: '0 10px', borderRadius: 6, border: 'none', background: accent, color: '#fff', cursor: 'pointer' }}>Tag</button>
              </div>
              </>
            )}
            </div>
          </div>
        </div>
      )}

      {logOpen && (
        <div style={{ display: 'flex', flexDirection: 'column', flex: 1, minHeight: 0, borderTop: '1px solid var(--line)' }}>
          <div style={{ display: 'flex', alignItems: 'center', gap: 8, padding: '6px 12px 2px 16px' }}>
            <span style={{ fontSize: 11, color: 'var(--faint2)', fontVariantNumeric: 'tabular-nums' }}>{logLines.length} events</span>
            <span style={{ flex: 1 }} />
            <button className="asp-icon-btn" onClick={onCopyAll} title="Copy all" style={{ display: 'flex', alignItems: 'center', justifyContent: 'center', width: 26, height: 24, flex: 'none', border: 'none', background: 'transparent', color: 'var(--text3)', borderRadius: 6, cursor: 'pointer', padding: 0 }}>
              {logCopied ? <Icon.CheckIcon size={15} stroke="#3a9357" /> : <Icon.CopyIcon size={15} stroke="currentColor" />}
            </button>
          </div>
          <div className="asp-scroll" style={{ flex: 1, minHeight: 0, overflowY: 'auto', padding: '2px 14px 10px' }}>
            {logLines.map((ln, i) => (
              <div key={i} onContextMenu={openLogCtx(ln.raw)} className="asp-hover-soft" style={{ display: 'flex', gap: 11, padding: '1.5px 6px', borderRadius: 4, fontFamily: "'JetBrains Mono', monospace", fontSize: 11.5, lineHeight: 1.7, whiteSpace: 'nowrap' }}>
                <span style={{ color: 'var(--faint2)', flex: 'none' }}>{ln.time}</span>
                <span style={{ color: logColor(ln.level, accent), flex: 'none', width: 40, fontWeight: 500 }}>{ln.level}</span>
                <span style={{ color: 'var(--text2)', overflow: 'hidden', textOverflow: 'ellipsis' }}>{ln.msg}</span>
              </div>
            ))}
          </div>
        </div>
      )}

      {/* Diff popup: what changed at the clicked history dot. */}
      {diff && (
        <>
          <div onPointerDown={() => setDiff(null)} style={{ position: 'fixed', inset: 0, zIndex: 70, background: 'rgba(28,25,23,0.28)' }} />
          <div data-testid="diff-popup" style={{ position: 'fixed', zIndex: 71, top: '16vh', left: '50%', transform: 'translateX(-50%)', width: 'min(720px, 92vw)', maxHeight: '64vh', background: 'var(--bg)', border: '1px solid var(--line)', borderRadius: 14, boxShadow: '0 24px 64px rgba(28,25,23,0.22)', display: 'flex', flexDirection: 'column', overflow: 'hidden' }}>
            <div style={{ display: 'flex', alignItems: 'center', gap: 8, padding: '11px 15px', borderBottom: '1px solid var(--line)' }}>
              <span style={{ width: 7, height: 7, borderRadius: '50%', background: colorForKind(diff.kind, accent), flex: 'none' }} />
              <span style={{ fontSize: 13, fontWeight: 600, flex: 1, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{diff.path}</span>
              <span style={{ fontSize: 11, color: 'var(--faint)', textTransform: 'capitalize' }}>{diff.kind}</span>
              <span onPointerDown={() => setDiff(null)} style={{ cursor: 'pointer', fontSize: 18, lineHeight: 1, color: 'var(--faint)', marginLeft: 4 }}>×</span>
            </div>
            <div className="asp-scroll" style={{ flex: 1, minHeight: 0, overflow: 'auto', padding: '10px 14px', fontFamily: "'JetBrains Mono', monospace", fontSize: 12, lineHeight: 1.55 }}>
              {diffBusy ? (
                <div style={{ color: 'var(--faint)' }}>Loading…</div>
              ) : (
                (() => {
                  const d = lineDiff(diff.before, diff.after);
                  if (d.unchanged) return <div style={{ color: 'var(--faint)' }}>No textual change at this point (or the change is between edits within the same second).</div>;
                  return (
                    <>
                      {d.context.map((l, i) => (
                        <div key={`c${i}`} style={{ color: 'var(--faint2)', whiteSpace: 'pre-wrap' }}> {l}</div>
                      ))}
                      {d.removed.map((l, i) => (
                        <div key={`r${i}`} data-testid="diff-removed" style={{ color: '#c0392b', background: '#c0392b14', whiteSpace: 'pre-wrap' }}>- {l}</div>
                      ))}
                      {d.added.map((l, i) => (
                        <div key={`a${i}`} data-testid="diff-added" style={{ color: '#2f8f4e', background: '#2f8f4e14', whiteSpace: 'pre-wrap' }}>+ {l}</div>
                      ))}
                    </>
                  );
                })()
              )}
            </div>
          </div>
        </>
      )}

      {/* Confirm before deleting a tag. */}
      {confirmTag && (
        <>
          <div onPointerDown={() => setConfirmTag(null)} style={{ position: 'fixed', inset: 0, zIndex: 72, background: 'rgba(28,25,23,0.28)' }} />
          <div data-testid="tag-delete-confirm" style={{ position: 'fixed', zIndex: 73, top: '38vh', left: '50%', transform: 'translateX(-50%)', width: 'min(320px, 90vw)', background: 'var(--bg)', border: '1px solid var(--line)', borderRadius: 12, boxShadow: '0 24px 64px rgba(28,25,23,0.22)', padding: 16 }}>
            <div style={{ fontSize: 13.5, color: 'var(--text)', marginBottom: 14 }}>Delete the tag <b>{confirmTag.name}</b>? This can't be undone.</div>
            <div style={{ display: 'flex', justifyContent: 'flex-end', gap: 8 }}>
              <button onPointerDown={() => setConfirmTag(null)} style={{ fontSize: 12.5, padding: '6px 12px', borderRadius: 7, border: '1px solid var(--line)', background: 'var(--bg)', color: 'var(--text2)', cursor: 'pointer' }}>Cancel</button>
              <button data-testid="tag-delete-confirm-btn" onPointerDown={() => { props.onDeleteTag(confirmTag.tag_id); setConfirmTag(null); }} style={{ fontSize: 12.5, padding: '6px 12px', borderRadius: 7, border: 'none', background: '#d96a6a', color: '#fff', cursor: 'pointer' }}>Delete tag</button>
            </div>
          </div>
        </>
      )}

      {locCtx && (
        <>
          <div onClick={() => setLocCtx(null)} onContextMenu={(e) => { e.preventDefault(); setLocCtx(null); }} style={{ position: 'fixed', inset: 0, zIndex: 64 }} />
          <div style={{ position: 'fixed', left: locCtx.x, top: locCtx.y, zIndex: 65, width: 200, background: 'var(--bg)', border: '1px solid var(--line)', borderRadius: 10, boxShadow: '0 10px 28px rgba(28,25,23,0.15)', padding: 4 }}>
            <div className="asp-hover-soft" onClick={() => { copyPath(); setLocCtx(null); }} style={{ display: 'flex', alignItems: 'center', padding: '7px 11px', borderRadius: 7, cursor: 'pointer', fontSize: 13, color: 'var(--text)' }}>
              <span>Copy path</span>
            </div>
            <div className="asp-hover-soft" onClick={() => { revealLoc(); setLocCtx(null); }} style={{ display: 'flex', alignItems: 'center', padding: '7px 11px', borderRadius: 7, cursor: 'pointer', fontSize: 13, color: 'var(--text)' }}>
              <span>Open in file manager</span>
            </div>
          </div>
        </>
      )}

      {logCtx && (
        <>
          <div onClick={() => setLogCtx(null)} onContextMenu={(e) => { e.preventDefault(); setLogCtx(null); }} style={{ position: 'fixed', inset: 0, zIndex: 64 }} />
          <div style={{ position: 'fixed', left: logCtx.x, top: logCtx.y, zIndex: 65, width: 156, background: 'var(--bg)', border: '1px solid var(--line)', borderRadius: 10, boxShadow: '0 10px 28px rgba(28,25,23,0.15)', padding: 4 }}>
            <div className="asp-hover-soft" onClick={() => { copy(logCtx.line); setLogCtx(null); }} style={{ display: 'flex', alignItems: 'center', padding: '7px 11px', borderRadius: 7, cursor: 'pointer', fontSize: 13, color: 'var(--text)' }}>
              <span>Copy line</span>
            </div>
            <div className="asp-hover-soft" onClick={onCopyAll} style={{ display: 'flex', alignItems: 'center', padding: '7px 11px', borderRadius: 7, cursor: 'pointer', fontSize: 13, color: 'var(--text)' }}>
              <span>Copy all</span>
            </div>
          </div>
        </>
      )}
    </div>
  );
}

function BranchDot({ color }: { color: string }) {
  return <span style={{ width: 6, height: 6, borderRadius: '50%', background: color, flex: 'none' }} />;
}

function TagIcon({ color = 'currentColor', size = 11 }: { color?: string; size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" fill="none" style={{ flex: 'none' }}>
      <path d="M2.5 2.5h5.2c.3 0 .6.12.8.34l4.7 4.7a1.1 1.1 0 0 1 0 1.56l-4.06 4.06a1.1 1.1 0 0 1-1.56 0l-4.7-4.7a1.1 1.1 0 0 1-.34-.8V2.5Z" stroke={color} strokeWidth="1.1" strokeLinejoin="round" />
      <circle cx="5.6" cy="5.6" r="1" fill={color} />
    </svg>
  );
}
