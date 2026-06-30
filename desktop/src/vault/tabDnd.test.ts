// Exhaustive tests for the pure @dnd-kit → onReorder index mapping. This is the
// load-bearing logic of tab reordering (the React/pointer plumbing is exercised
// by the keyboard-sensor test in TabBar.test.tsx and the e2e drag), so it is
// pinned at 100%.
import { describe, expect, it } from '../test-shim';
import { reorderFromDragEnd } from './tabDnd';

const tabs = ['a.md', 'b.md', 'c.md'];

describe('reorderFromDragEnd', () => {
  it('maps a forward move (active id, over id) → {from, to}', () => {
    expect(reorderFromDragEnd(tabs, 'a.md', 'c.md')).toEqual({ from: 0, to: 2 });
  });

  it('maps a backward move', () => {
    expect(reorderFromDragEnd(tabs, 'c.md', 'a.md')).toEqual({ from: 2, to: 0 });
  });

  it('maps a single-step move', () => {
    expect(reorderFromDragEnd(tabs, 'b.md', 'c.md')).toEqual({ from: 1, to: 2 });
  });

  it('returns null when dropped on itself (no move)', () => {
    expect(reorderFromDragEnd(tabs, 'b.md', 'b.md')).toBeNull();
  });

  it('returns null when there is no drop target (over is null/undefined)', () => {
    expect(reorderFromDragEnd(tabs, 'a.md', null)).toBeNull();
    expect(reorderFromDragEnd(tabs, 'a.md', undefined)).toBeNull();
  });

  it('returns null when the active id is null/undefined', () => {
    expect(reorderFromDragEnd(tabs, null, 'a.md')).toBeNull();
    expect(reorderFromDragEnd(tabs, undefined, 'a.md')).toBeNull();
  });

  it('returns null when an id is not among the tabs', () => {
    expect(reorderFromDragEnd(tabs, 'a.md', 'gone.md')).toBeNull();
    expect(reorderFromDragEnd(tabs, 'gone.md', 'a.md')).toBeNull();
  });

  it('coerces numeric ids to strings before lookup', () => {
    // @dnd-kit ids can be string | number; our tab ids are paths, but guard the
    // numeric branch so a numeric id never silently mismatches.
    expect(reorderFromDragEnd(['0', '1', '2'], 0, 2)).toEqual({ from: 0, to: 2 });
  });
});
