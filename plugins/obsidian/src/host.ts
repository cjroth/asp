// The host-vault seam. The bridge talks to this interface, not to Obsidian
// directly — so the exact same sync logic runs against a real Obsidian vault and
// against a FakeVault in tests (no protocol logic lives in the host glue).

export interface HostVault {
  read(path: string): Promise<Uint8Array | null>;
  write(path: string, bytes: Uint8Array): Promise<void>;
  remove(path: string): Promise<void>;
  list(): Promise<string[]>;
}
