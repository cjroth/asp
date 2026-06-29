// Bun test preload (configured in bunfig.toml). bun:test has no built-in DOM —
// vitest used `environment: 'jsdom'` — so we register a jsdom window/document as
// globals before any test runs, then apply the project's test setup (in-memory
// localStorage, an execCommand stub, and the default desktop-platform flag).
import { JSDOM } from 'jsdom';

const dom = new JSDOM('<!doctype html><html><body></body></html>', {
  url: 'http://localhost/',
  pretendToBeVisual: true,
});
const g = globalThis as unknown as Record<string, unknown>;
g.window = dom.window;
g.document = dom.window.document;
g.navigator = dom.window.navigator;
for (const k of Object.getOwnPropertyNames(dom.window)) {
  if (!(k in g)) {
    try {
      g[k] = (dom.window as unknown as Record<string, unknown>)[k];
    } catch {
      /* read-only window prop — skip */
    }
  }
}
// bun ships native Event/Node/etc. globals; jsdom's `dispatchEvent` brand-checks
// against ITS realm's classes, so `new Event()` from bun's global is rejected as
// "not of type Event". Force the DOM event/node classes to jsdom's so the global
// and `window.*` versions are one and the same (matches a real browser + vitest).
for (const k of [
  'Event', 'CustomEvent', 'MouseEvent', 'KeyboardEvent', 'InputEvent', 'FocusEvent',
  'PointerEvent', 'UIEvent', 'DragEvent', 'WheelEvent', 'Node', 'Element', 'HTMLElement',
  'HTMLInputElement', 'HTMLTextAreaElement', 'DocumentFragment', 'DOMParser', 'Range',
  'getComputedStyle', 'requestAnimationFrame', 'cancelAnimationFrame', 'MutationObserver',
]) {
  if (k in dom.window) g[k] = (dom.window as unknown as Record<string, unknown>)[k];
}
// @testing-library/react needs this to silence the act(...) environment warning.
g.IS_REACT_ACT_ENVIRONMENT = true;

// jsdom's localStorage is a no-op under an opaque origin; install an in-memory
// Storage so persistence (prefs, vault metadata, theme) is testable.
class MemoryStorage implements Storage {
  private m = new Map<string, string>();
  get length(): number {
    return this.m.size;
  }
  clear(): void {
    this.m.clear();
  }
  getItem(key: string): string | null {
    return this.m.has(key) ? (this.m.get(key) as string) : null;
  }
  key(index: number): string | null {
    return Array.from(this.m.keys())[index] ?? null;
  }
  removeItem(key: string): void {
    this.m.delete(key);
  }
  setItem(key: string, value: string): void {
    this.m.set(key, String(value));
  }
}

const store = new MemoryStorage();
Object.defineProperty(globalThis, 'localStorage', { value: store, configurable: true, writable: true });
Object.defineProperty(window, 'localStorage', { value: store, configurable: true, writable: true });

// jsdom doesn't implement execCommand (used by the editor's plain-text paste).
if (!document.execCommand) {
  (document as unknown as { execCommand: () => boolean }).execCommand = () => false;
}

// Default unit tests to the desktop platform (matches the e2e mock). The
// web-platform tests delete this flag to exercise the browser/OPFS branch.
(window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ =
  (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ || {};
