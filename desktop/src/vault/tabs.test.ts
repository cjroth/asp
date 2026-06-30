// Exhaustive unit tests for the pure tab/hash helpers. Pinned at 100% coverage.
import { afterEach, beforeEach, describe, expect, it } from '../test-shim';
import { buildHash, closeAll, closeOthers, closeTab, closeToLeft, closeToRight, loadOpenTabs, parseHash, remapTabs, removeTabs, reorderTabs, saveOpenTabs, withTab } from './tabs';

describe('buildHash / parseHash', () => {
  it('round-trips a plain vault + file', () => {
    const h = buildHash('vid1', 'README.md');
    expect(h).toBe('#vid1/README.md');
    expect(parseHash(h)).toEqual({ vaultId: 'vid1', path: 'README.md' });
  });

  it('round-trips a path with slashes (the / is encoded, not a separator)', () => {
    const h = buildHash('vid1', 'notes/sub/a.md');
    expect(h).toBe('#vid1/notes%2Fsub%2Fa.md');
    expect(parseHash(h)).toEqual({ vaultId: 'vid1', path: 'notes/sub/a.md' });
  });

  it('round-trips spaces and unicode', () => {
    const path = 'my folder/héllo wörld 📓.md';
    const vaultId = 'vault id with spaces';
    const h = buildHash(vaultId, path);
    expect(parseHash(h)).toEqual({ vaultId, path });
  });

  it('parses a hash without the leading #', () => {
    expect(parseHash('vid1/a.md')).toEqual({ vaultId: 'vid1', path: 'a.md' });
  });

  it('returns null for empty / #-only / garbage hashes', () => {
    expect(parseHash('')).toBeNull();
    expect(parseHash('#')).toBeNull();
    expect(parseHash('#nopathhere')).toBeNull(); // no slash
    expect(parseHash('#/onlypath')).toBeNull(); // empty vaultId (slash at 0)
    expect(parseHash('#vaultonly/')).toBeNull(); // trailing slash, empty path
  });

  it('returns null for malformed percent-encoding', () => {
    expect(parseHash('#%E0%A4%A/x')).toBeNull(); // bad vaultId encoding
    expect(parseHash('#ok/%')).toBeNull(); // bad path encoding
  });

});

describe('open-tabs persistence', () => {
  beforeEach(() => localStorage.clear());
  afterEach(() => localStorage.clear());

  it('returns [] when nothing is stored', () => {
    expect(loadOpenTabs('vidX')).toEqual([]);
  });

  it('saves and loads a tab list under asp.tabs.<vaultId>', () => {
    saveOpenTabs('vidX', ['a.md', 'b.md']);
    expect(localStorage.getItem('asp.tabs.vidX')).toBe(JSON.stringify(['a.md', 'b.md']));
    expect(loadOpenTabs('vidX')).toEqual(['a.md', 'b.md']);
  });

  it('returns [] for corrupt JSON', () => {
    localStorage.setItem('asp.tabs.vidX', '{not json');
    expect(loadOpenTabs('vidX')).toEqual([]);
  });

  it('returns [] when the stored value is not an array', () => {
    localStorage.setItem('asp.tabs.vidX', JSON.stringify({ a: 1 }));
    expect(loadOpenTabs('vidX')).toEqual([]);
  });

  it('drops non-string entries from a stored array', () => {
    localStorage.setItem('asp.tabs.vidX', JSON.stringify(['a.md', 3, null, 'b.md']));
    expect(loadOpenTabs('vidX')).toEqual(['a.md', 'b.md']);
  });

  it('swallows errors when localStorage throws on read and write', () => {
    const orig = globalThis.localStorage;
    const boom = {
      getItem() {
        throw new Error('nope');
      },
      setItem() {
        throw new Error('nope');
      },
    } as unknown as Storage;
    Object.defineProperty(globalThis, 'localStorage', { value: boom, configurable: true, writable: true });
    expect(loadOpenTabs('v')).toEqual([]);
    expect(() => saveOpenTabs('v', ['a'])).not.toThrow();
    Object.defineProperty(globalThis, 'localStorage', { value: orig, configurable: true, writable: true });
  });
});

describe('withTab', () => {
  it('appends a new path', () => {
    expect(withTab(['a'], 'b')).toEqual(['a', 'b']);
  });
  it('is a no-op (same ref) when already present', () => {
    const t = ['a', 'b'];
    expect(withTab(t, 'a')).toBe(t);
  });
  it('appends to an empty list', () => {
    expect(withTab([], 'a')).toEqual(['a']);
  });
});

describe('closeTab neighbor selection', () => {
  it('closing a NON-active tab keeps the active file', () => {
    expect(closeTab(['a', 'b', 'c'], 'b', 'a')).toEqual({ tabs: ['b', 'c'], active: 'b' });
  });

  it('closing the active MIDDLE tab selects the next', () => {
    expect(closeTab(['a', 'b', 'c'], 'b', 'b')).toEqual({ tabs: ['a', 'c'], active: 'c' });
  });

  it('closing the active LAST tab selects the previous', () => {
    expect(closeTab(['a', 'b', 'c'], 'c', 'c')).toEqual({ tabs: ['a', 'b'], active: 'b' });
  });

  it('closing the active FIRST tab selects the next', () => {
    expect(closeTab(['a', 'b', 'c'], 'a', 'a')).toEqual({ tabs: ['b', 'c'], active: 'b' });
  });

  it('closing the only (active) tab yields a null active', () => {
    expect(closeTab(['a'], 'a', 'a')).toEqual({ tabs: [], active: null });
  });

  it('closing a path not in the list is a no-op', () => {
    expect(closeTab(['a', 'b'], 'a', 'zzz')).toEqual({ tabs: ['a', 'b'], active: 'a' });
  });
});

describe('remapTabs', () => {
  it('remaps a single renamed file', () => {
    expect(remapTabs(['a.md', 'b.md'], 'a.md', 'c.md')).toEqual(['c.md', 'b.md']);
  });

  it('remaps a folder subtree prefix but not a same-named-prefix file', () => {
    expect(remapTabs(['notes/a.md', 'notesX.md', 'notes/sub/b.md'], 'notes', 'archive')).toEqual([
      'archive/a.md',
      'notesX.md',
      'archive/sub/b.md',
    ]);
  });

  it('de-dupes when a remap collides with an existing tab', () => {
    expect(remapTabs(['a.md', 'b.md'], 'a.md', 'b.md')).toEqual(['b.md']);
  });

  it('leaves unrelated tabs untouched', () => {
    expect(remapTabs(['x.md'], 'a.md', 'c.md')).toEqual(['x.md']);
  });
});

describe('removeTabs', () => {
  it('removes exact matches', () => {
    expect(removeTabs(['a.md', 'b.md', 'c.md'], ['b.md'])).toEqual(['a.md', 'c.md']);
  });

  it('removes a whole folder subtree', () => {
    expect(removeTabs(['notes/a.md', 'notes/sub/b.md', 'other.md'], ['notes'])).toEqual(['other.md']);
  });

  it('does not remove a same-prefix sibling file', () => {
    expect(removeTabs(['notes.md', 'notes/a.md'], ['notes'])).toEqual(['notes.md']);
  });

  it('removes several paths at once', () => {
    expect(removeTabs(['a', 'b', 'c'], ['a', 'c'])).toEqual(['b']);
  });
});

describe('reorderTabs', () => {
  it('moves a tab forward', () => {
    expect(reorderTabs(['a', 'b', 'c', 'd'], 0, 2)).toEqual(['b', 'c', 'a', 'd']);
  });

  it('moves a tab backward', () => {
    expect(reorderTabs(['a', 'b', 'c', 'd'], 3, 1)).toEqual(['a', 'd', 'b', 'c']);
  });

  it('is a no-op (same ref) when from === to', () => {
    const t = ['a', 'b', 'c'];
    expect(reorderTabs(t, 1, 1)).toBe(t);
  });

  it('returns the original (same ref) for an out-of-range index', () => {
    const t = ['a', 'b'];
    expect(reorderTabs(t, -1, 0)).toBe(t);
    expect(reorderTabs(t, 5, 0)).toBe(t);
    expect(reorderTabs(t, 0, -1)).toBe(t);
    expect(reorderTabs(t, 0, 9)).toBe(t);
  });
});

describe('closeOthers', () => {
  it('keeps only the given path (middle)', () => {
    expect(closeOthers(['a', 'b', 'c'], 'b')).toEqual(['b']);
  });
  it('keeps only the given path (first)', () => {
    expect(closeOthers(['a', 'b', 'c'], 'a')).toEqual(['a']);
  });
  it('keeps only the given path (last)', () => {
    expect(closeOthers(['a', 'b', 'c'], 'c')).toEqual(['c']);
  });
  it('is a no-op on a single matching tab', () => {
    expect(closeOthers(['a'], 'a')).toEqual(['a']);
  });
  it('keeps nothing when the path is not present', () => {
    expect(closeOthers(['a', 'b'], 'zzz')).toEqual([]);
  });
  it('keeps nothing on an empty list', () => {
    expect(closeOthers([], 'a')).toEqual([]);
  });
});

describe('closeToLeft', () => {
  it('drops everything before the path (middle)', () => {
    expect(closeToLeft(['a', 'b', 'c'], 'b')).toEqual(['b', 'c']);
  });
  it('is a no-op when the path is first', () => {
    expect(closeToLeft(['a', 'b', 'c'], 'a')).toEqual(['a', 'b', 'c']);
  });
  it('keeps only the path when it is last', () => {
    expect(closeToLeft(['a', 'b', 'c'], 'c')).toEqual(['c']);
  });
  it('is a no-op on a single tab', () => {
    expect(closeToLeft(['a'], 'a')).toEqual(['a']);
  });
  it('returns the original (same ref) when the path is not present', () => {
    const t = ['a', 'b'];
    expect(closeToLeft(t, 'zzz')).toBe(t);
  });
  it('returns the original (same ref) on an empty list', () => {
    const t: string[] = [];
    expect(closeToLeft(t, 'a')).toBe(t);
  });
});

describe('closeToRight', () => {
  it('drops everything after the path (middle)', () => {
    expect(closeToRight(['a', 'b', 'c'], 'b')).toEqual(['a', 'b']);
  });
  it('keeps only the path when it is first', () => {
    expect(closeToRight(['a', 'b', 'c'], 'a')).toEqual(['a']);
  });
  it('is a no-op when the path is last', () => {
    expect(closeToRight(['a', 'b', 'c'], 'c')).toEqual(['a', 'b', 'c']);
  });
  it('is a no-op on a single tab', () => {
    expect(closeToRight(['a'], 'a')).toEqual(['a']);
  });
  it('returns the original (same ref) when the path is not present', () => {
    const t = ['a', 'b'];
    expect(closeToRight(t, 'zzz')).toBe(t);
  });
  it('returns the original (same ref) on an empty list', () => {
    const t: string[] = [];
    expect(closeToRight(t, 'a')).toBe(t);
  });
});

describe('closeAll', () => {
  it('returns an empty list', () => {
    expect(closeAll()).toEqual([]);
  });
});
