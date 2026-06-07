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
    await this.adapter.writeBinary(path, bytes.buffer as ArrayBuffer);
  }
  async remove(path: string): Promise<void> {
    try {
      await this.adapter.remove(path);
    } catch {
      /* already gone */
    }
  }
  async list(): Promise<string[]> {
    return await this.walk('');
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
