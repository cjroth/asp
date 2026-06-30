import { afterEach, describe, expect, it, vi } from '../test-shim';
import { basename, freeName, makeAccessKey, relTime, shortFingerprint } from './format';

afterEach(() => vi.useRealTimers());

describe('format', () => {
  it('basename strips trailing slashes and dirs', () => {
    expect(basename('/a/b/c')).toBe('c');
    expect(basename('/a/b/')).toBe('b');
    expect(basename('solo')).toBe('solo');
    expect(basename('/')).toBe('/'); // empty parts → fall back to the input
    expect(basename('')).toBe('');
  });

  it('relTime buckets', () => {
    const now = 1_700_000_000;
    vi.useFakeTimers();
    vi.setSystemTime(now * 1000);
    expect(relTime(null)).toBe('—');
    expect(relTime(0)).toBe('—');
    expect(relTime(now - 2)).toBe('just now');
    expect(relTime(now - 30)).toBe('30s ago');
    expect(relTime(now - 120)).toBe('2m ago');
    expect(relTime(now - 7200)).toBe('2h ago');
    expect(relTime(now - 100000)).toBe('yesterday');
    expect(relTime(now - 300000)).toBe('3d ago');
  });

  it('shortFingerprint abbreviates long keys, keeps short ones', () => {
    expect(shortFingerprint('ssh-ed25519 ABCDEFGHIJKLMNOPQRSTUV')).toBe('ABCDEFGH…STUV');
    expect(shortFingerprint('ssh-ed25519 short')).toBe('short');
  });

  it('makeAccessKey is 4 groups of 4 from the safe alphabet', () => {
    const k = makeAccessKey();
    expect(k).toMatch(/^[ABCDEFGHJKLMNPQRSTUVWXYZ23456789]{4}(-[ABCDEFGHJKLMNPQRSTUVWXYZ23456789]{4}){3}$/);
  });

  it('freeName finds the first free untitled name', () => {
    expect(freeName(new Set(), '.md')).toBe('untitled.md');
    expect(freeName(new Set(['untitled.md']), '.md')).toBe('untitled-1.md');
    expect(freeName(new Set(['untitled.md', 'untitled-1.md']), '.md')).toBe('untitled-2.md');
    expect(freeName(new Set(['untitled.md', 'untitled-1.md', 'untitled-2.md']), '.md')).toBe('untitled-3.md');
    expect(freeName(new Set(), '')).toBe('untitled');
  });
});
