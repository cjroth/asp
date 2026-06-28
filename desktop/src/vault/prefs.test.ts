import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  applyTheme,
  clampHistBar,
  clampSidebar,
  DEFAULT_PREFS,
  FONT_FAMILIES,
  fontFamilyOf,
  HISTBAR_MAX,
  HISTBAR_MIN,
  loadPrefs,
  savePrefs,
  SIDEBAR_MAX,
  SIDEBAR_MIN,
} from './prefs';

afterEach(() => {
  localStorage.clear();
  document.documentElement.removeAttribute('data-theme');
});

describe('prefs', () => {
  it('loads defaults when nothing is stored', () => {
    expect(loadPrefs()).toEqual(DEFAULT_PREFS);
  });

  it('merges stored partial prefs over defaults', () => {
    localStorage.setItem('asp.prefs.v1', JSON.stringify({ accent: '#ff0000', theme: 'dark' }));
    const p = loadPrefs();
    expect(p.accent).toBe('#ff0000');
    expect(p.theme).toBe('dark');
    expect(p.prettyNames).toBe(false); // default preserved
  });

  it('tolerates legacy persisted prefs that still carry font / fontOverride keys', () => {
    // Old builds wrote `font` + `fontOverride`; they are no longer part of Prefs.
    // The spread keeps them harmless and the active prefs are unaffected.
    localStorage.setItem('asp.prefs.v1', JSON.stringify({ font: 'Mono', fontOverride: 'Sans', accent: '#abcdef' }));
    const p = loadPrefs();
    expect(p.accent).toBe('#abcdef');
    expect(fontFamilyOf()).toBe(FONT_FAMILIES.Serif); // still serif regardless
  });

  it('tolerates corrupt JSON', () => {
    localStorage.setItem('asp.prefs.v1', '{not json');
    expect(loadPrefs()).toEqual(DEFAULT_PREFS);
  });

  it('round-trips via savePrefs', () => {
    const p = { ...DEFAULT_PREFS, prettyNames: true, sidebarW: 320 };
    savePrefs(p);
    expect(loadPrefs()).toEqual(p);
  });

  it('applyTheme sets the data-theme attribute', () => {
    applyTheme('dark');
    expect(document.documentElement.getAttribute('data-theme')).toBe('dark');
    applyTheme('light');
    expect(document.documentElement.getAttribute('data-theme')).toBe('light');
  });

  it('fontFamilyOf always returns the serif reading font', () => {
    expect(fontFamilyOf()).toBe(FONT_FAMILIES.Serif);
  });

  it('clampSidebar bounds the width', () => {
    expect(clampSidebar(10)).toBe(SIDEBAR_MIN);
    expect(clampSidebar(9999)).toBe(SIDEBAR_MAX);
    expect(clampSidebar(300)).toBe(300);
  });

  it('clampHistBar bounds the height', () => {
    expect(clampHistBar(10)).toBe(HISTBAR_MIN);
    expect(clampHistBar(9999)).toBe(HISTBAR_MAX);
    expect(clampHistBar(200)).toBe(200);
  });

  it('defaults the history bar height', () => {
    expect(DEFAULT_PREFS.histBarH).toBe(150);
  });

  it('savePrefs swallows storage errors', () => {
    const spy = vi.spyOn(localStorage, 'setItem').mockImplementation(() => { throw new Error('quota'); });
    expect(() => savePrefs(DEFAULT_PREFS)).not.toThrow();
    spy.mockRestore();
  });

  it('applyTheme swallows DOM errors', () => {
    const spy = vi.spyOn(document.documentElement, 'setAttribute').mockImplementation(() => { throw new Error('no dom'); });
    expect(() => applyTheme('dark')).not.toThrow();
    spy.mockRestore();
  });
});
