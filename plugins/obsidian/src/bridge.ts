// The two-way bridge: Obsidian vault ⇄ the SDK engine. Host events push file
// bytes into the engine; after sync the engine's materialized tree is rendered
// back to the host. Echo storms are suppressed structurally — the engine is
// content-addressed, so re-writing identical bytes authors no row, and we only
// render host files whose bytes actually changed.

import type { Vault } from '../../../sdks/typescript/src/index.ts';
import type { HostVault } from './host.ts';
import { PathFilter } from './path-filter.ts';

function eq(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) return false;
  return true;
}

export class Bridge {
  constructor(
    private vault: Vault,
    private host: HostVault,
    private filter: PathFilter = new PathFilter(),
  ) {}

  setFilter(f: PathFilter) {
    this.filter = f;
  }

  /** A host file was created or modified — capture it into the engine. */
  async pushWrite(path: string): Promise<void> {
    if (this.filter.ignored(path)) return;
    const bytes = await this.host.read(path);
    if (bytes == null) return;
    this.vault.writeFile(path, bytes); // no-op in the engine if unchanged
  }

  async pushDelete(path: string): Promise<void> {
    if (this.filter.ignored(path)) return;
    this.vault.deleteFile(path);
  }

  async pushRename(from: string, to: string): Promise<void> {
    if (this.filter.ignored(from) || this.filter.ignored(to)) return;
    this.vault.renameFile(from, to);
  }

  /** Seed the engine from the host's current contents (startup reconcile). */
  async reconcileFromHost(): Promise<void> {
    for (const path of await this.host.list()) {
      if (this.filter.ignored(path)) continue;
      const bytes = await this.host.read(path);
      if (bytes != null) this.vault.writeFile(path, bytes);
    }
  }

  /** Render the engine's materialized tree back to the host vault. */
  async materializeToHost(): Promise<void> {
    const files = this.vault.files();
    const want = new Set(Object.keys(files));
    for (const [path, bytes] of Object.entries(files)) {
      const cur = await this.host.read(path);
      if (cur == null || !eq(cur, bytes)) await this.host.write(path, bytes);
    }
    // Remove host files the engine no longer has (deletes/renames-away).
    for (const path of await this.host.list()) {
      if (this.filter.ignored(path)) continue;
      if (!want.has(path)) await this.host.remove(path);
    }
  }
}
