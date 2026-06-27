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
    expect(p.font).toBe('Sans'); // default preserved
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

  it('fontFamilyOf prefers the override', () => {
    expect(fontFamilyOf({ ...DEFAULT_PREFS, font: 'Sans', fontOverride: null })).toBe(FONT_FAMILIES.Sans);
    expect(fontFamilyOf({ ...DEFAULT_PREFS, font: 'Sans', fontOverride: 'Serif' })).toBe(FONT_FAMILIES.Serif);
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
