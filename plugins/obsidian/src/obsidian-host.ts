// HostVault adapter over the real Obsidian vault adapter. Host glue only — file
// I/O. Everything synced lives as normal files; `.asp/` is excluded by the filter.

import type { DataAdapter } from 'obsidian';
import type { HostVault } from './host.ts';

export class ObsidianHost implements HostVault {
  constructor(private adapter: DataAdapter) {}

  async read(path: string): Promise<Uint8Array | null> {
    try {
      if (!(await this.adapter.exists(path))) return null;
      return new Uint8Array(await this.adapter.readBinary(path));
    } catch {
      return null;
    }
  }
  async write(path: string, bytes: Uint8Array): Promise<void> {
    // A synced file may land in a subfolder that doesn't exist locally yet.
    // Obsidian's `writeBinary` does NOT create parents (and `mkdir` is not
    // recursive), so without this the host throws "parent folder doesn't
    // exist" on the first note in any new folder.
    await this.ensureParent(path);
    await this.adapter.writeBinary(path, bytes.buffer as ArrayBuffer);
  }
  async remove(path: string): Promise<void> {
    try {
      await this.adapter.remove(path);
    } catch {
      /* already gone */
    }
    // The engine models files only — a folder move is N file moves — so reap
    // now-empty ancestor folders the removal left behind (best-effort).
    await this.pruneEmptyParents(path);
  }
  async list(): Promise<string[]> {
    return await this.walk('');
  }

  /** Create every missing ancestor folder of `path`. `mkdir` is not recursive,
   * so walk the prefix and create each segment; idempotent (an existing folder
   * is benign). */
  private async ensureParent(path: string): Promise<void> {
    const slash = path.lastIndexOf('/');
    if (slash <= 0) return; // vault root — nothing to create
    const parts = path.slice(0, slash).split('/').filter(Boolean);
    let cur = '';
    for (const seg of parts) {
      cur = cur ? `${cur}/${seg}` : seg;
      try {
        if (!(await this.adapter.exists(cur))) await this.adapter.mkdir(cur);
      } catch (e) {
        // A folder that physically exists but is missing from a cold cache
        // makes mkdir throw "already exists" — benign. Re-raise anything else.
        if (!/already exists/i.test(String((e as Error)?.message ?? e))) throw e;
      }
    }
  }

  /** Walk up from `path`, deleting ancestor folders that are now empty. Stops
   * at the first non-empty folder (or the vault root). Best-effort. */
  private async pruneEmptyParents(path: string): Promise<void> {
    let dir = path.includes('/') ? path.slice(0, path.lastIndexOf('/')) : '';
    while (dir && dir !== '.asp' && !dir.endsWith('/.asp')) {
      let entry: { files: string[]; folders: string[] };
      try {
        entry = await this.adapter.list(dir);
      } catch {
        break;
      }
      if (entry.files.length || entry.folders.length) break; // still in use
      try {
        await this.adapter.rmdir(dir, false);
      } catch {
        break;
      }
      dir = dir.includes('/') ? dir.slice(0, dir.lastIndexOf('/')) : '';
    }
  }

  private async walk(dir: string): Promise<string[]> {
    const out: string[] = [];
    let entry: { files: string[]; folders: string[] };
    try {
      entry = await this.adapter.list(dir);
    } catch {
      return out;
    }
    for (const f of entry.files) out.push(f);
    for (const sub of entry.folders) {
      if (sub === '.asp' || sub.endsWith('/.asp')) continue;
      out.push(...(await this.walk(sub)));
    }
    return out;
  }
}
