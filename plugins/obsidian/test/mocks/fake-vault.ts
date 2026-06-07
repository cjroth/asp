// A minimal in-memory HostVault standing in for the Obsidian vault adapter — the
// plugin's bridge/controller run against it identically to a real vault.

import type { HostVault } from '../../src/host.ts';

export class FakeVault implements HostVault {
  files = new Map<string, Uint8Array>();

  async read(path: string): Promise<Uint8Array | null> {
    return this.files.get(path) ?? null;
  }
  async write(path: string, bytes: Uint8Array): Promise<void> {
    this.files.set(path, bytes);
  }
  async remove(path: string): Promise<void> {
    this.files.delete(path);
  }
  async list(): Promise<string[]> {
    return [...this.files.keys()];
  }

  setText(path: string, s: string) {
    this.files.set(path, new TextEncoder().encode(s));
  }
  getText(path: string): string | undefined {
    const b = this.files.get(path);
    return b ? new TextDecoder().decode(b) : undefined;
  }
}
