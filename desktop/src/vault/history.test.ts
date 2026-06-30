import { describe, expect, it } from '../test-shim';
import {
  axisTicksFor,
  buildEvents,
  chooseStep,
  clampSpan,
  clampView,
  colorOf,
  createTsByPath,
  DAY,
  defaultView,
  fmtFull,
  fmtTick,
  HOUR,
  MIN,
  toPct,
  viewForNow,
  zoomAround,
  zoomKeepingFocus,
} from './history';

const NOW = 1_700_000_000_000;

describe('toPct', () => {
  it('maps view start/end to 0/100', () => {
    const v = { start: 0, end: 100 };
    expect(toPct(0, v)).toBe(0);
    expect(toPct(50, v)).toBe(50);
    expect(toPct(100, v)).toBe(100);
  });
});

describe('chooseStep', () => {
  it('picks the first STEP >= span/6', () => {
    expect(chooseStep(6 * HOUR)).toBe(HOUR); // span/6 = 1h
    expect(chooseStep(7 * DAY)).toBe(2 * DAY); // span/6 ~ 1.17d -> 2d
    expect(chooseStep(6 * 4 * MIN)).toBe(5 * MIN); // span/6 = 4m -> 5m
  });
});

describe('clampView / clampSpan', () => {
  it('clampSpan honors the [10min, 60day] bounds', () => {
    expect(clampSpan(MIN)).toBe(10 * MIN);
    expect(clampSpan(100 * DAY)).toBe(60 * DAY);
    expect(clampSpan(3 * DAY)).toBe(3 * DAY);
  });
  it('keeps end within now + 40% of span (shifts window, not size)', () => {
    const span = 10 * DAY;
    const v = clampView(NOW + 5 * DAY, NOW + 5 * DAY + span, NOW);
    expect(v.end - v.start).toBeCloseTo(span, 6);
    expect(v.end).toBeLessThanOrEqual(NOW + span * 0.4 + 1);
  });
  it('keeps start above now - 90 days', () => {
    const span = 10 * DAY;
    const v = clampView(NOW - 200 * DAY, NOW - 200 * DAY + span, NOW);
    expect(v.start).toBeGreaterThanOrEqual(NOW - 90 * DAY - 1);
    expect(v.end - v.start).toBeCloseTo(span, 6);
  });
});

describe('zoom', () => {
  it('zoomKeepingFocus keeps the point under the cursor fixed', () => {
    const v = defaultView(NOW);
    const f = 0.5;
    const focusBefore = v.start + f * (v.end - v.start);
    const nv = zoomKeepingFocus(v, f, 0.5, NOW);
    const focusAfter = nv.start + f * (nv.end - nv.start);
    expect(focusAfter).toBeCloseTo(focusBefore, 3);
    expect(nv.end - nv.start).toBeLessThan(v.end - v.start);
  });
  it('zoomAround a center keeps that instant at the same fraction', () => {
    const v = defaultView(NOW);
    const center = NOW - 2 * DAY;
    const fBefore = (center - v.start) / (v.end - v.start);
    const nv = zoomAround(v, center, 1.8, NOW);
    const fAfter = (center - nv.start) / (nv.end - nv.start);
    expect(fAfter).toBeCloseTo(fBefore, 3);
  });
});

describe('viewForNow', () => {
  it('re-centers only when now is outside the view', () => {
    const inside = { start: NOW - DAY, end: NOW + DAY };
    expect(viewForNow(inside, NOW)).toBe(inside);
    const outside = { start: NOW - 10 * DAY, end: NOW - 5 * DAY };
    const re = viewForNow(outside, NOW);
    expect(NOW).toBeGreaterThanOrEqual(re.start);
    expect(NOW).toBeLessThanOrEqual(re.end);
  });
});

describe('axisTicksFor', () => {
  it('produces aligned ticks across the window with pct in range', () => {
    const v = defaultView(NOW);
    const ticks = axisTicksFor(v);
    expect(ticks.length).toBeGreaterThan(0);
    for (const t of ticks) {
      expect(t.pct).toBeGreaterThanOrEqual(-1);
      expect(t.pct).toBeLessThanOrEqual(101);
      expect(typeof t.label).toBe('string');
    }
  });
});

describe('buildEvents / createTsByPath / colorOf', () => {
  it('converts unix seconds to ms, sorts, and finds earliest per path', () => {
    const evs = buildEvents([
      { id: 'b', ts: 200, lamport: 2, kind: 'edit', path: 'a.md' },
      { id: 'a', ts: 100, lamport: 1, kind: 'create', path: 'a.md' },
      { id: 'c', ts: 150, lamport: 3, kind: 'create', path: 'b.md' },
    ]);
    expect(evs.map((e) => e.id)).toEqual(['a', 'c', 'b']);
    expect(evs[0].ts).toBe(100_000);
    const created = createTsByPath(evs);
    expect(created['a.md']).toBe(100_000);
    expect(created['b.md']).toBe(150_000);
  });
  it('colors events by kind', () => {
    expect(colorOf('create')).toBe('#3fa45a');
    expect(colorOf('edit')).toBe('#3d63dd');
    expect(colorOf('rename')).toBe('#d9a93d');
    expect(colorOf('delete')).toBe('#d96a6a');
  });
});

describe('time formatting', () => {
  it('fmtFull formats a full timestamp (pads single-digit time parts)', () => {
    // 2021-11-14T22:13:20Z — exercise pad on both single- and double-digit fields.
    const full = fmtFull(1_636_927_400_000);
    expect(full).toMatch(/^[A-Z][a-z]{2} \d{1,2}, \d\d:\d\d$/);
  });
  it('fmtTick shows a date for day-scale steps and a clock otherwise', () => {
    const ts = 1_636_927_400_000;
    expect(fmtTick(ts, DAY)).toMatch(/^[A-Z][a-z]{2} \d{1,2}$/);
    expect(fmtTick(ts, HOUR)).toMatch(/^\d\d:\d\d$/);
  });
});
