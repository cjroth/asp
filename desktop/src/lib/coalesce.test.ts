import { afterEach, beforeEach, describe, expect, it, vi } from '../test-shim';
import { makeCoalescer } from './coalesce';

beforeEach(() => vi.useFakeTimers());
afterEach(() => vi.useRealTimers());

describe('makeCoalescer', () => {
  it('runs once after the quiet window with the latest value (coalesces a burst)', () => {
    const runs: Array<[string, number]> = [];
    const c = makeCoalescer<number>((k, v) => runs.push([k, v]), 100);
    c.schedule('a', 1);
    c.schedule('a', 2);
    c.schedule('a', 3);
    expect(runs).toEqual([]); // nothing yet
    expect(c.pendingKeys()).toEqual(['a']);
    vi.advanceTimersByTime(99);
    expect(runs).toEqual([]);
    vi.advanceTimersByTime(1);
    expect(runs).toEqual([['a', 3]]); // one run, last value
    expect(c.pendingKeys()).toEqual([]);
  });

  it('debounces — each schedule resets the timer', () => {
    const runs: number[] = [];
    const c = makeCoalescer<number>((_k, v) => runs.push(v), 100);
    c.schedule('a', 1);
    vi.advanceTimersByTime(80);
    c.schedule('a', 2); // resets
    vi.advanceTimersByTime(80);
    expect(runs).toEqual([]); // 160ms total but never 100ms quiet
    vi.advanceTimersByTime(20);
    expect(runs).toEqual([2]);
  });

  it('keeps keys independent', () => {
    const runs: Array<[string, number]> = [];
    const c = makeCoalescer<number>((k, v) => runs.push([k, v]), 100);
    c.schedule('a', 1);
    c.schedule('b', 2);
    expect(new Set(c.pendingKeys())).toEqual(new Set(['a', 'b']));
    vi.advanceTimersByTime(100);
    expect(new Set(runs.map((r) => r[0]))).toEqual(new Set(['a', 'b']));
  });

  it('flush() runs all pending immediately and clears timers', () => {
    const runs: string[] = [];
    const c = makeCoalescer<number>((k) => runs.push(k), 100);
    c.schedule('a', 1);
    c.schedule('b', 2);
    c.flush();
    expect(new Set(runs)).toEqual(new Set(['a', 'b']));
    expect(c.pendingKeys()).toEqual([]);
    // timers were cleared — advancing fires nothing more.
    vi.advanceTimersByTime(1000);
    expect(runs.length).toBe(2);
  });

  it('flushKey() runs just that key', () => {
    const runs: string[] = [];
    const c = makeCoalescer<number>((k) => runs.push(k), 100);
    c.schedule('a', 1);
    c.schedule('b', 2);
    c.flushKey('a');
    expect(runs).toEqual(['a']);
    expect(c.pendingKeys()).toEqual(['b']);
    vi.advanceTimersByTime(100);
    expect(runs).toEqual(['a', 'b']);
  });

  it('flush/flushKey with nothing pending is a no-op', () => {
    const runs: string[] = [];
    const c = makeCoalescer<number>((k) => runs.push(k), 100);
    c.flush();
    c.flushKey('nope');
    c.cancel('nope');
    expect(runs).toEqual([]);
  });

  it('cancel() drops pending work without running it', () => {
    const runs: string[] = [];
    const c = makeCoalescer<number>((k) => runs.push(k), 100);
    c.schedule('a', 1);
    c.schedule('b', 2);
    c.cancel('a');
    expect(c.pendingKeys()).toEqual(['b']);
    vi.advanceTimersByTime(1000);
    expect(runs).toEqual(['b']); // 'a' never ran; 'b' still did
  });

  it('can schedule again after firing', () => {
    const runs: number[] = [];
    const c = makeCoalescer<number>((_k, v) => runs.push(v), 100);
    c.schedule('a', 1);
    vi.advanceTimersByTime(100);
    c.schedule('a', 2);
    vi.advanceTimersByTime(100);
    expect(runs).toEqual([1, 2]);
  });
});
