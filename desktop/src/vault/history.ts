// History time-travel track geometry — a faithful port of the design's timeline
// math (curView/clampView/zoomAround/STEPS/toPct/fmtFull/fmtTick), plus a small
// adapter that turns the backend's real log events (`api.history`, unix seconds)
// into the epoch-ms event model the track renders. The design synthesized fake
// per-file versions; here the events are real and content-at-a-time comes from
// `api.readFileAt`.
import type { HistEvent } from '../lib/api';

export const MIN = 60000;
export const HOUR = 3600000;
export const DAY = 86400000;

export interface View {
  start: number; // epoch ms
  end: number; // epoch ms
}
export interface TrackEvent {
  id: string;
  ts: number; // epoch ms
  kind: string;
  path: string;
}

export function defaultView(now: number): View {
  return { start: now - 7 * DAY, end: now + 0.4 * DAY };
}

export function clampView(start: number, end: number, now: number): View {
  const span = end - start;
  const maxEnd = now + span * 0.4;
  if (end > maxEnd) {
    const sh = end - maxEnd;
    start -= sh;
    end -= sh;
  }
  const minStart = now - 90 * DAY;
  if (start < minStart) {
    const sh = minStart - start;
    start += sh;
    end += sh;
  }
  return { start, end };
}

export function toPct(ts: number, view: View): number {
  return ((ts - view.start) / (view.end - view.start)) * 100;
}

const STEPS = [5 * MIN, 15 * MIN, 30 * MIN, HOUR, 3 * HOUR, 6 * HOUR, 12 * HOUR, DAY, 2 * DAY, 7 * DAY, 14 * DAY, 30 * DAY];

export function chooseStep(span: number): number {
  const raw = span / 6;
  let step = STEPS[STEPS.length - 1];
  for (const s of STEPS) {
    if (s >= raw) {
      step = s;
      break;
    }
  }
  return step;
}

// New span for a zoom factor, with the design's [10min, 60day] clamps.
export function clampSpan(span: number): number {
  return Math.max(MIN * 10, Math.min(60 * DAY, span));
}

// Zoom keeping the point at fraction `f` of the view fixed.
export function zoomKeepingFocus(view: View, f: number, factor: number, now: number): View {
  const span = view.end - view.start;
  const focus = view.start + f * span;
  const ns = clampSpan(span * factor);
  return clampView(focus - f * ns, focus - f * ns + ns, now);
}

// Zoom centered on a wall-clock instant (playhead or now) — the +/- buttons.
export function zoomAround(view: View, center: number, factor: number, now: number): View {
  const span = view.end - view.start;
  const f = (center - view.start) / span;
  const ns = clampSpan(span * factor);
  return clampView(center - f * ns, center - f * ns + ns, now);
}

// Re-center the view on `now` if `now` fell outside it (the "Now" button).
export function viewForNow(view: View, now: number): View {
  if (now > view.end || now < view.start) {
    const span = view.end - view.start;
    return { start: now - span * 0.82, end: now + span * 0.18 };
  }
  return view;
}

const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];
const pad = (x: number) => (x < 10 ? '0' : '') + x;

export function fmtFull(ts: number): string {
  const d = new Date(ts);
  return MONTHS[d.getMonth()] + ' ' + d.getDate() + ', ' + pad(d.getHours()) + ':' + pad(d.getMinutes());
}
export function fmtTick(ts: number, step: number): string {
  const d = new Date(ts);
  return step >= DAY ? MONTHS[d.getMonth()] + ' ' + d.getDate() : pad(d.getHours()) + ':' + pad(d.getMinutes());
}

export interface AxisTick {
  label: string;
  pct: number;
}
export function axisTicksFor(view: View): AxisTick[] {
  const step = chooseStep(view.end - view.start);
  const out: AxisTick[] = [];
  for (let t = Math.ceil(view.start / step) * step; t <= view.end; t += step) {
    out.push({ label: fmtTick(t, step), pct: toPct(t, view) });
  }
  return out;
}

export function colorOf(kind: string): string {
  return kind === 'create' ? '#3fa45a' : kind === 'edit' ? '#3d63dd' : kind === 'rename' ? '#d9a93d' : '#d96a6a';
}

// Convert backend history (unix SECONDS) into epoch-ms track events.
export function buildEvents(hist: HistEvent[]): TrackEvent[] {
  return hist
    .map((e) => ({ id: e.id, ts: e.ts * 1000, kind: e.kind, path: e.path }))
    .sort((a, b) => a.ts - b.ts);
}

// Earliest event ts (ms) per path — used to tell "this file did not exist yet".
export function createTsByPath(events: TrackEvent[]): Record<string, number> {
  const m: Record<string, number> = {};
  for (const e of events) {
    if (m[e.path] == null || e.ts < m[e.path]) m[e.path] = e.ts;
  }
  return m;
}
