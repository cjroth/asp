// The bottom bar: a status row (location · fingerprint · live/time-travel pill)
// with History / Log tabs that expand a panel. History shows the time-travel
// track (pan, scrub, zoom, jump-to-event); Log shows real sync events derived
// from history() + live status. All color is theme-driven via CSS variables.
import React, { useEffect, useMemo, useRef, useState } from 'react';
import { api } from '../lib/api';
import type { HistEvent, VaultStatus } from '../lib/api';
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
  onTabHistory: () => void;
  onTabLog: () => void;
  onNow: () => void;
}

const colorForKind = (kind: string, accent: string): string =>
  kind === 'create' ? '#3fa45a' : kind === 'edit' ? accent : kind === 'rename' ? '#d9a93d' : '#d96a6a';

export default function HistoryBar(props: HistoryBarProps) {
  const { events, histRaw, view, setView, playhead, setPlayhead, now, accent, accentSoft, timeTravel } = props;
  const { location, locationIsPath, fingerprint, status, identity, histOpen, logOpen } = props;

  const trackRef = useRef<HTMLDivElement | null>(null);
  const viewRef = useRef(view);
  const nowRef = useRef(now);
  const playheadRef = useRef(playhead);
  viewRef.current = view;
  nowRef.current = now;
  playheadRef.current = playhead;

  const [logCopied, setLogCopied] = useState(false);
  const [logCtx, setLogCtx] = useState<{ x: number; y: number; line: string } | null>(null);

  // Location path: single click copies the full path (with brief feedback),
  // double click reveals it in the OS file manager. A short timer on the single
  // click lets a following dblclick cancel it so a reveal never also copies.
  const [pathCopied, setPathCopied] = useState(false);
  const clickTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const onPathClick = () => {
    if (clickTimer.current) clearTimeout(clickTimer.current);
    clickTimer.current = setTimeout(() => {
      clickTimer.current = null;
      copy(location);
      setPathCopied(true);
      setTimeout(() => setPathCopied(false), 1200);
    }, 250);
  };
  const onPathDouble = () => {
    if (clickTimer.current) {
      clearTimeout(clickTimer.current);
      clickTimer.current = null;
    }
    void api.revealPath(location);
  };

  // ---- geometry ----
  const span = view.end - view.start;
  const playT = playhead == null ? now : playhead;
  const filterTs = timeTravel ? playhead : null;
  const axisTicks = useMemo(() => axisTicksFor(view), [view]);
  const playPct = Math.max(0, Math.min(100, toPct(playT, view)));
  const nowPct = Math.max(0, Math.min(100, toPct(now, view)));

  // Cap rendered tick nodes: a vault import clusters thousands of events at one
  // instant — rendering them all is a render bomb (they overlap to one pixel).
  const inView = events.filter((e) => e.ts >= view.start - span * 0.03 && e.ts <= view.end + span * 0.03);
  const sampled = inView.length > MAX_TICKS ? inView.filter((_, i) => i % Math.ceil(inView.length / MAX_TICKS) === 0) : inView;
  const visibleRows = filterTs == null ? events.length : events.filter((e) => e.ts <= filterTs).length;

  // ---- track interaction ----
  const onTrackDown = (e: React.PointerEvent) => {
    const el = trackRef.current;
    if (!el) return;
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
      const v = viewRef.current || defaultView(nowRef.current);
      const r = el.getBoundingClientRect();
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

  const onJump = (ts: number) => (e: React.PointerEvent) => {
    e.stopPropagation();
    setPlayhead(Math.min(ts, nowRef.current));
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
  const barHeight = logOpen ? 196 : histOpen ? 108 : 38;

  return (
    <div style={{ flex: 'none', height: barHeight, background: 'var(--bg-sub)', borderTop: '1px solid var(--line)', display: 'flex', flexDirection: 'column', userSelect: 'none', transition: 'height .16s ease' }}>
      <div style={{ display: 'flex', alignItems: 'center', height: 38, padding: '0 9px 0 15px', gap: 10, flex: 'none' }}>
        <span style={{ display: 'inline-flex', flex: 'none', color: 'var(--faint2)' }}>
          {locationIsPath ? <Icon.FolderIcon size={12} stroke="var(--faint2)" /> : <Icon.GlobeIcon size={12} stroke="var(--faint2)" />}
        </span>
        <span
          onClick={locationIsPath ? onPathClick : undefined}
          onDoubleClick={locationIsPath ? onPathDouble : undefined}
          title={locationIsPath ? 'Click to copy path · double-click to reveal in file manager' : undefined}
          style={{ fontFamily: locationIsPath ? "'JetBrains Mono', monospace" : 'inherit', fontSize: 12, color: pathCopied ? accent : 'var(--text2)', maxWidth: 190, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', cursor: locationIsPath ? 'pointer' : 'default' }}
        >{locationIsPath && pathCopied ? 'Copied path' : location}</span>
        <span style={{ fontFamily: "'JetBrains Mono', monospace", fontSize: 10.5, color: 'var(--faint2)', flex: 'none', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{fingerprint}</span>
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
            <div style={{ display: 'flex', flex: 'none', border: '1px solid var(--line)', borderRadius: 7, overflow: 'hidden' }}>
              <button className="asp-icon-btn" onClick={() => zoomBtn(1.8)} title="Zoom out" style={{ width: 26, height: 24, border: 'none', borderRight: '1px solid var(--line)', background: 'var(--bg)', color: 'var(--text3)', cursor: 'pointer', display: 'flex', alignItems: 'center', justifyContent: 'center', padding: 0 }}>
                <Icon.MinusIcon />
              </button>
              <button className="asp-icon-btn" onClick={() => zoomBtn(0.55)} title="Zoom in" style={{ width: 26, height: 24, border: 'none', background: 'var(--bg)', color: 'var(--text3)', cursor: 'pointer', display: 'flex', alignItems: 'center', justifyContent: 'center', padding: 0 }}>
                <Icon.PlusIcon size={14} />
              </button>
            </div>
            <button onClick={props.onNow} style={{ fontFamily: 'inherit', fontSize: 12, fontWeight: 500, color: timeTravel ? 'var(--text2)' : 'var(--faint2)', background: 'var(--bg)', border: '1px solid var(--line)', borderRadius: 7, padding: '4px 12px', cursor: 'pointer', flex: 'none' }}>Now</button>
          </div>

          <div ref={trackRef} data-testid="history-track" onPointerDown={onTrackDown} style={{ position: 'relative', flex: 1, margin: '0 16px 11px', cursor: 'crosshair', touchAction: 'none' }}>
            <div style={{ position: 'absolute', left: 0, right: 0, top: '50%', height: 1, background: 'var(--line)' }} />
            {axisTicks.map((a, i) => (
              <React.Fragment key={i}>
                <div style={{ position: 'absolute', left: a.pct + '%', top: 0, bottom: 0, width: 1, background: 'var(--line)' }} />
                <div style={{ position: 'absolute', left: a.pct + '%', bottom: -2, transform: 'translateX(4px)', fontSize: 9.5, color: 'var(--faint2)', fontFamily: "'JetBrains Mono', monospace", whiteSpace: 'nowrap' }}>{a.label}</div>
              </React.Fragment>
            ))}
            {sampled.map((e, i) => {
              const pct = toPct(e.ts, view);
              const past = e.ts <= playT;
              const c = colorForKind(e.kind, accent);
              return (
                <div
                  key={i}
                  onPointerDown={onJump(e.ts)}
                  title={`${e.kind} · ${e.path} · ${fmtFull(e.ts)}`}
                  style={{ position: 'absolute', left: pct + '%', top: '50%', width: 18, height: 18, marginLeft: -9, marginTop: -9, borderRadius: '50%', cursor: 'pointer', display: 'flex', alignItems: 'center', justifyContent: 'center', zIndex: 3 }}
                >
                  <span style={{ width: 9, height: 9, borderRadius: '50%', background: past ? c : 'var(--bg)', border: '1.5px solid ' + c, opacity: past ? 1 : 0.5 }} />
                </div>
              );
            })}
            <div style={{ position: 'absolute', left: nowPct + '%', top: 0, bottom: 0, width: 0, borderLeft: '1px dashed var(--faint2)' }} />
            <div style={{ position: 'absolute', left: playPct + '%', top: 3, bottom: 3, width: 2, marginLeft: -1, background: accent, borderRadius: 1, zIndex: 5 }}>
              <div onPointerDown={onHandleDown} style={{ position: 'absolute', left: -11, top: '50%', width: 24, height: 28, marginTop: -14, borderRadius: 8, background: accent, border: '2px solid var(--bg)', boxShadow: '0 2px 6px rgba(28,25,23,0.22)', cursor: 'ew-resize' }} />
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

      {logCtx && (
        <>
          <div onClick={() => setLogCtx(null)} onContextMenu={(e) => { e.preventDefault(); setLogCtx(null); }} style={{ position: 'fixed', inset: 0, zIndex: 64 }} />
          <div style={{ position: 'fixed', left: logCtx.x, top: logCtx.y, zIndex: 65, width: 156, background: 'var(--bg)', border: '1px solid var(--line)', borderRadius: 10, boxShadow: '0 10px 28px rgba(28,25,23,0.15)', padding: 4 }}>
            <div className="asp-hover-soft" onClick={() => { copy(logCtx.line); setLogCtx(null); }} style={{ display: 'flex', alignItems: 'center', gap: 9, padding: '7px 11px', borderRadius: 7, cursor: 'pointer', fontSize: 13, color: 'var(--text)' }}>
              <Icon.CopyIcon size={14} stroke="#78716c" style={{ flex: 'none' }} />
              <span>Copy line</span>
            </div>
            <div className="asp-hover-soft" onClick={onCopyAll} style={{ display: 'flex', alignItems: 'center', gap: 9, padding: '7px 11px', borderRadius: 7, cursor: 'pointer', fontSize: 13, color: 'var(--text)' }}>
              <Icon.ListIcon size={14} stroke="#78716c" style={{ flex: 'none' }} />
              <span>Copy all</span>
            </div>
          </div>
        </>
      )}
    </div>
  );
}
