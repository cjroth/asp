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
   * Returns the number of files captured. */
  async reconcileFromHost(): Promise<number> {
    // Stage every local file in ONE batch (a single fold) rather than a
    // record_write per file — per-file re-folding is O(n²), which made the
    // first sync of a large vault crawl.
    const files: Record<string, Uint8Array> = {};
    for (const path of await this.host.list()) {
      if (this.filter.ignored(path)) continue;
      const bytes = await this.host.read(path);
      if (bytes != null) files[path] = bytes;
    }
    const n = Object.keys(files).length;
    if (n > 0) await this.vault.writeFiles(files);
    this.log(`reconcile: staged ${n} local file${n === 1 ? '' : 's'} into the engine`);
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
      (f) => !f.deleted && f.merge_class !== 'dir',
    );
    const want = new Set(detail.map((f) => f.path));
    let written = 0;
    let removed = 0;
    for (const f of detail) {
      const hash = f.result_hash ?? '';
      // Unchanged since we last wrote it → skip (no read, no fetch).
      if (this.materializedHashes.get(f.path) === hash) continue;
      const bytes = await this.vault.readFile(f.path);
      if (bytes == null) continue;
      const cur = await this.host.read(f.path);
      if (cur == null || !eq(cur, bytes)) {
        await this.host.write(f.path, bytes);
        written++;
        this.log(`pull: wrote ${f.path}`);
      }
      this.materializedHashes.set(f.path, hash);
    }
    // Remove host files the engine no longer has (deletes/renames-away).
    for (const path of await this.host.list()) {
      if (this.filter.ignored(path)) continue;
      if (!want.has(path)) {
        await this.host.remove(path);
        this.materializedHashes.delete(path);
        removed++;
        this.log(`pull: removed ${path}`);
      }
    }
    this.log(`materialize: ${written} written, ${removed} removed (${want.size} in tree)`);
    return { written, removed };
  }
}
