// User preferences + theme. The design exposes accent / font / writing-column /
// frontmatter-style as canvas props and theme / font-override / sidebar-width /
// hidden-files / pretty-names as in-app state; here they are one persisted Prefs
// object (localStorage `asp.prefs.v1`). No backend involvement — these are local
// view settings only.

export type FontKey = 'Sans' | 'Serif' | 'Mono';
export type FrontmatterStyle = 'Card' | 'Banner' | 'Below';
export type Theme = 'light' | 'dark';

export interface Prefs {
  accent: string;
  font: FontKey; // base reading font
  fontOverride: FontKey | null; // status-bar toggle (Serif ⇄ Sans)
  frontmatterStyle: FrontmatterStyle;
  writingColumn: boolean;
  theme: Theme;
  sidebarW: number;
  histBarH: number;
  showHidden: boolean;
  prettyNames: boolean;
}

export const DEFAULT_PREFS: Prefs = {
  accent: '#3d63dd',
  font: 'Sans',
  fontOverride: null,
  frontmatterStyle: 'Below',
  writingColumn: true,
  theme: 'light',
  sidebarW: 266,
  histBarH: 150,
  showHidden: false,
  prettyNames: false,
};

export const FONT_FAMILIES: Record<FontKey, string> = {
  Sans: "system-ui, -apple-system, 'Segoe UI', sans-serif",
  Serif: "'Newsreader', Georgia, serif",
  Mono: "'JetBrains Mono', ui-monospace, Menlo, monospace",
};

const KEY = 'asp.prefs.v1';

export function loadPrefs(): Prefs {
  try {
    const raw = localStorage.getItem(KEY);
    if (raw) return { ...DEFAULT_PREFS, ...(JSON.parse(raw) as Partial<Prefs>) };
  } catch {
    /* ignore */
  }
  return { ...DEFAULT_PREFS };
}

export function savePrefs(prefs: Prefs): void {
  try {
    localStorage.setItem(KEY, JSON.stringify(prefs));
  } catch {
    /* ignore */
  }
}

// The whole app themes from the `data-theme` attribute on <html> (styles.css
// reads it). Mirrors the design's `applyTheme`.
export function applyTheme(theme: Theme): void {
  try {
    document.documentElement.setAttribute('data-theme', theme === 'dark' ? 'dark' : 'light');
  } catch {
    /* ignore */
  }
}

// The effective reading font: the status-bar override wins over the base font.
export function fontFamilyOf(prefs: Prefs): string {
  return FONT_FAMILIES[prefs.fontOverride || prefs.font];
}

export const SIDEBAR_MIN = 200;
export const SIDEBAR_MAX = 460;
export function clampSidebar(w: number): number {
  return Math.max(SIDEBAR_MIN, Math.min(SIDEBAR_MAX, w));
}

// The bottom history/log bar grows upward from the bottom. One shared height
// drives whichever panel is open; dragging below COLLAPSE snaps it fully shut.
export const HISTBAR_MIN = 96;
export const HISTBAR_MAX = 640;
export const HISTBAR_COLLAPSE = 72;
export function clampHistBar(h: number): number {
  return Math.max(HISTBAR_MIN, Math.min(HISTBAR_MAX, h));
}
