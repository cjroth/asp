// jsdom's localStorage is a no-op under an opaque origin, and Node's experimental
// global `localStorage` is disabled without --localstorage-file. Install a simple
// in-memory Storage so persistence (prefs, vault metadata, theme) is testable.
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
if (typeof window !== 'undefined') {
  Object.defineProperty(window, 'localStorage', { value: store, configurable: true, writable: true });
}

// jsdom doesn't implement execCommand (used by the editor's plain-text paste).
if (typeof document !== 'undefined' && !document.execCommand) {
  (document as unknown as { execCommand: () => boolean }).execCommand = () => false;
}

// Default unit tests to the desktop platform (matches the e2e mock). The
// web-platform test deletes this flag to exercise the browser/OPFS branch.
if (typeof window !== 'undefined') {
  (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ || {};
}
