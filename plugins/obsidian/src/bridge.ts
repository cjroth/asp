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
    let n = 0;
    for (const path of await this.host.list()) {
      if (this.filter.ignored(path)) continue;
      const bytes = await this.host.read(path);
      if (bytes != null) {
        await this.vault.writeFile(path, bytes);
        n++;
      }
    }
    this.log(`reconcile: staged ${n} local file${n === 1 ? '' : 's'} into the engine`);
    return n;
  }

  /** Render the engine's materialized tree back to the host vault. Returns the
   * counts actually changed on disk. */
  async materializeToHost(): Promise<{ written: number; removed: number }> {
    const files = await this.vault.files();
    const want = new Set(Object.keys(files));
    let written = 0;
    let removed = 0;
    for (const [path, bytes] of Object.entries(files)) {
      const cur = await this.host.read(path);
      if (cur == null || !eq(cur, bytes)) {
        await this.host.write(path, bytes);
        written++;
        this.log(`pull: wrote ${path}`);
      }
    }
    // Remove host files the engine no longer has (deletes/renames-away).
    for (const path of await this.host.list()) {
      if (this.filter.ignored(path)) continue;
      if (!want.has(path)) {
        await this.host.remove(path);
        removed++;
        this.log(`pull: removed ${path}`);
      }
    }
    this.log(`materialize: ${written} written, ${removed} removed (${want.size} in tree)`);
    return { written, removed };
  }
}
