// In-memory ring buffer of recent plugin trace lines + host-side errors. Backs
// the in-app log viewer in settings: on mobile (iOS/Android) Obsidian's WebView
// dev console is unreachable, so capturing what the plugin does ourselves is the
// only way to see — and copy — a trace for debugging. Visible before a sync ever
// connects.
//
// Bounded so a long session can't grow unbounded; ordered oldest-first.
// Subscribers are notified on every append so an open viewer can stream live.
// Not persisted — a relaunch clears it ("show me what's happened since I started
// watching").

export interface LogEntry {
  /** `HH:MM:SS.mmm` wall clock at capture. */
  ts: string;
  level: 'info' | 'error';
  msg: string;
}

/** What the sync controller / bridge call to record a line. Optional everywhere
 * (tests construct them without one), so it defaults to a no-op. */
export type Logger = (msg: string, level?: 'info' | 'error') => void;

export class LogBuffer {
  private readonly entries: LogEntry[] = [];
  private readonly subscribers = new Set<(e: LogEntry) => void>();

  constructor(private readonly capacity = 500) {}

  /** Append a line (also mirrored to the dev console when reachable), evict the
   * oldest at capacity, and fan out to subscribers. */
  append(msg: string, level: 'info' | 'error' = 'info'): void {
    const entry: LogEntry = { ts: aspTimestamp(), level, msg };
    this.entries.push(entry);
    if (this.entries.length > this.capacity) this.entries.shift();
    // Mirror to the console for desktop debugging; harmless on mobile.
    (level === 'error' ? console.error : console.log)(`[asp ${entry.ts}] ${msg}`);
    for (const fn of this.subscribers) {
      try {
        fn(entry);
      } catch {
        // a subscriber throwing must not break the others or the buffer
      }
    }
  }

  /** Stream future appends. Returns an unsubscribe. */
  subscribe(fn: (e: LogEntry) => void): () => void {
    this.subscribers.add(fn);
    return () => this.subscribers.delete(fn);
  }

  /** A snapshot of the current entries, oldest first. */
  snapshot(): LogEntry[] {
    return this.entries.slice();
  }

  clear(): void {
    this.entries.length = 0;
  }

  /** Plain-text dump, one line per entry — what "Copy all" puts on the
   * clipboard so a capture pastes cleanly into a chat:
   *   [asp HH:MM:SS.mmm] message
   *   [asp HH:MM:SS.mmm] ERROR message
   */
  toText(): string {
    return this.entries
      .map((e) => `[asp ${e.ts}] ${e.level === 'error' ? 'ERROR ' : ''}${e.msg}`)
      .join('\n');
  }
}

/** `HH:MM:SS.mmm` formatter. */
export function aspTimestamp(d: Date = new Date()): string {
  const p = (n: number, w = 2) => String(n).padStart(w, '0');
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}.${p(d.getMilliseconds(), 3)}`;
}
