// The two-way bridge: Obsidian vault ⇄ the SDK engine. Host events push file
// bytes into the engine; after sync the engine's materialized tree is rendered
// back to the host. Echo storms are suppressed structurally — the engine is
// content-addressed, so re-writing identical bytes authors no row, and we only
// render host files whose bytes actually changed.

import type { EngineVault } from '../../../sdks/typescript/src/index.ts';
import type { HostVault } from './host.ts';
import type { Logger } from './log-buffer.ts';
import { PathFilter } from './path-filter.ts';

function eq(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) return false;
  return true;
}

/** Run `fn` over `items` with at most `limit` in flight. Host filesystem ops on
 * mobile are 50–100× slower per call than desktop, so doing them one-at-a-time
 * serializes that latency — a large pull then takes tens of seconds. Bounded
 * concurrency overlaps the waits (capped so we don't exhaust file handles). */
async function mapLimit<T>(items: T[], limit: number, fn: (item: T) => Promise<void>): Promise<void> {
  let next = 0;
  const worker = async () => {
    while (next < items.length) {
      const i = next++;
      await fn(items[i]);
    }
  };
  await Promise.all(Array.from({ length: Math.min(limit, items.length) }, worker));
}

const HOST_IO_CONCURRENCY = 16;

export class Bridge {
  private log: Logger = () => {};
  /** path → content hash last written to the host, so an incremental
   * materialize skips unchanged files without reading or fetching them. */
  private materializedHashes = new Map<string, string>();

  constructor(
    private vault: EngineVault,
    private host: HostVault,
    private filter: PathFilter = new PathFilter(),
  ) {}

  setFilter(f: PathFilter) {
    this.filter = f;
  }

  setLogger(log: Logger) {
    this.log = log;
  }

  /** A host file was created or modified — capture it into the engine. */
  async pushWrite(path: string): Promise<void> {
    if (this.filter.ignored(path)) return;
    const bytes = await this.host.read(path);
    if (bytes == null) return;
    await this.vault.writeFile(path, bytes); // no-op in the engine if unchanged
  }

  async pushDelete(path: string): Promise<void> {
    if (this.filter.ignored(path)) return;
    await this.vault.deleteFile(path);
  }

  async pushRename(from: string, to: string): Promise<void> {
    if (this.filter.ignored(from) || this.filter.ignored(to)) return;
    await this.vault.renameFile(from, to);
  }

  /** Seed the engine from the host's current contents (startup reconcile).
   * Returns the number of files captured.
   *
   * `captureDeletes`: also author deletes for paths the ENGINE holds but the
   * host disk no longer has — files deleted while the host app was closed (no
   * delete events fire for those; without this the peer's copy resurrects them
   * on the next materialize). Only safe on a WARM engine (restored state or a
   * completed sync this session): on a cold engine "missing from disk" just
   * means "not materialized yet", and authoring deletes would wipe every file
   * created on other devices. The caller gates it. */
  async reconcileFromHost(opts: { captureDeletes?: boolean } = {}): Promise<number> {
    // Stage every local file in ONE batch (a single fold) rather than a
    // record_write per file — per-file re-folding is O(n²), which made the
    // first sync of a large vault crawl.
    const files: Record<string, Uint8Array> = {};
    const paths = (await this.host.list()).filter((p) => !this.filter.ignored(p));
    // Read the host files concurrently — sequential reads of a large vault on
    // mobile (high-latency fs) made the first sync crawl.
    await mapLimit(paths, HOST_IO_CONCURRENCY, async (path) => {
      const bytes = await this.host.read(path);
      if (bytes != null) files[path] = bytes;
    });
    const n = Object.keys(files).length;
    if (n > 0) await this.vault.writeFiles(files);

    let deleted = 0;
    if (opts.captureDeletes) {
      // Engine paths missing from the host = deleted offline. Locally-ignored
      // paths are exempt — ignoring a file must not delete it vault-wide.
      const onHost = new Set(paths);
      const missing = (await this.vault.filesDetail())
        .filter(
          (f) =>
            !f.deleted && f.merge_class !== 'dir' && !this.filter.ignored(f.path) && !onHost.has(f.path),
        )
        .map((f) => f.path);
      if (missing.length > 0) await this.vault.deleteFiles(missing);
      deleted = missing.length;
    }

    this.log(
      `reconcile: staged ${n} local file${n === 1 ? '' : 's'} into the engine` +
        (deleted > 0 ? ` (${deleted} deleted while the app was closed)` : ''),
    );
    return n;
  }

  /** Render the engine's materialized tree back to the host vault. Returns the
   * counts actually changed on disk. */
  async materializeToHost(): Promise<{ written: number; removed: number }> {
    // INCREMENTAL: list per-file metadata (path + content hash — cheap, no
    // content) and fetch only changed files' bytes one at a time. The old path
    // pulled EVERY file's content at once via files() → files_json serialized
    // all bytes into a single JSON number-array string, which OOMs/truncates the
    // worker on a large vault ("Unexpected end of JSON input"). This keeps memory
    // flat and writes only what actually changed.
    const detail = (await this.vault.filesDetail()).filter(
      (f) => !f.deleted && f.merge_class !== 'dir' && !this.filter.ignored(f.path),
    );
    const want = new Set(detail.map((f) => f.path));
    let written = 0;
    let removed = 0;

    // Only files whose content changed since we last wrote them (cheap sync
    // filter, no I/O). Then fetch + write them CONCURRENTLY — the per-file host
    // writes are the bottleneck on mobile's high-latency fs (doing thousands
    // one-at-a-time took tens of seconds; see the per-file timing in the logs).
    const changed = detail.filter((f) => this.materializedHashes.get(f.path) !== (f.result_hash ?? ''));
    await mapLimit(changed, HOST_IO_CONCURRENCY, async (f) => {
      const bytes = await this.vault.readFile(f.path);
      if (bytes == null) return;
      const cur = await this.host.read(f.path);
      if (cur == null || !eq(cur, bytes)) {
        await this.host.write(f.path, bytes);
        written++; // JS is single-threaded between awaits — no race on this counter
      }
      this.materializedHashes.set(f.path, f.result_hash ?? '');
    });

    // Remove host files the engine no longer has (deletes/renames-away), also
    // concurrently. (No per-file log line — a large pull would flood the log.)
    const toRemove = (await this.host.list()).filter((p) => !this.filter.ignored(p) && !want.has(p));
    await mapLimit(toRemove, HOST_IO_CONCURRENCY, async (path) => {
      await this.host.remove(path);
      this.materializedHashes.delete(path);
      removed++;
    });

    this.log(`materialize: ${written} written, ${removed} removed (${want.size} in tree)`);
    return { written, removed };
  }
}
