// A keyed debounce-coalescer. `schedule(key, value)` runs `run(key, value)` after
// `delayMs` of quiet for that key, coalescing a burst of calls into one run with
// the latest value. `flush`/`flushKey` run pending work immediately — e.g. on
// page-hide so a debounced OPFS state write isn't lost when the tab closes.
//
// The web backend uses this so editing a large vault doesn't re-serialize and
// re-write the whole engine state to OPFS on every keystroke-save (or every
// synced remote row); the durable copy is the live peer sync, OPFS is the cache.
export interface Coalescer<T> {
  schedule(key: string, value: T): void;
  flushKey(key: string): void;
  flush(): void;
  /** Drop a key's pending work without running it (e.g. the vault was deleted). */
  cancel(key: string): void;
  pendingKeys(): string[];
}

export function makeCoalescer<T>(run: (key: string, value: T) => void, delayMs: number): Coalescer<T> {
  const timers = new Map<string, ReturnType<typeof setTimeout>>();
  const pending = new Map<string, T>();

  const fire = (key: string): void => {
    const timer = timers.get(key);
    if (timer !== undefined) {
      clearTimeout(timer);
      timers.delete(key);
    }
    if (pending.has(key)) {
      const value = pending.get(key) as T;
      pending.delete(key);
      run(key, value);
    }
  };

  return {
    schedule(key, value) {
      pending.set(key, value);
      const existing = timers.get(key);
      if (existing !== undefined) clearTimeout(existing);
      timers.set(key, setTimeout(() => fire(key), delayMs));
    },
    flushKey(key) {
      fire(key);
    },
    cancel(key) {
      const timer = timers.get(key);
      if (timer !== undefined) {
        clearTimeout(timer);
        timers.delete(key);
      }
      pending.delete(key);
    },
    flush() {
      for (const key of [...pending.keys()]) fire(key);
    },
    pendingKeys() {
      return [...pending.keys()];
    },
  };
}
