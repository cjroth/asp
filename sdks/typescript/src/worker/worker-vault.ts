// Main-thread proxy: presents the slice of the `Vault` API the Obsidian plugin
// uses, forwarding each call to the engine worker and awaiting the reply. All
// engine compute happens in the worker, so these calls are cheap on the
// renderer thread (a postMessage round-trip, not a wasm call). Identity is
// cached from `init` so the settings UI can read the device key synchronously.

import type { Port } from './channel.ts';
import type { Command, FromWorker, Identity, InitPayload, Reply, ToWorker } from './protocol.ts';

type Pending = { resolve: (v: Reply['value']) => void; reject: (e: Error) => void };

export class WorkerVault {
  private nextId = 1;
  private readonly pending = new Map<number, Pending>();
  private identity?: Identity;

  constructor(private readonly port: Port<ToWorker, FromWorker>) {
    port.onMessage((reply) => {
      const p = this.pending.get(reply.id);
      if (!p) return;
      this.pending.delete(reply.id);
      if (reply.ok) p.resolve(reply.value);
      else p.reject(new Error(reply.error ?? 'engine worker error'));
    });
  }

  private call(cmd: Omit<Command, 'id' | 'kind'>): Promise<Reply['value']> {
    const id = this.nextId++;
    return new Promise<Reply['value']>((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.port.post({ kind: 'cmd', id, ...cmd } as Command);
    });
  }

  /** Stand up the engine in the worker; returns (and caches) the device
   * identity. Must be awaited before any other call. */
  async init(payload: InitPayload): Promise<Identity> {
    const id = (await this.call({ op: 'init', payload })) as Identity;
    this.identity = id;
    return id;
  }

  // Cached identity — synchronous, valid after `init()`.
  nodeSsh(): string {
    return this.identity?.nodeSsh ?? '';
  }
  nodeId(): string {
    return this.identity?.nodeId ?? '';
  }
  vaultId(): string {
    return this.identity?.vaultId ?? '';
  }

  async writeFile(path: string, bytes: Uint8Array): Promise<void> {
    await this.call({ op: 'writeFile', path, bytes });
  }
  async deleteFile(path: string): Promise<void> {
    await this.call({ op: 'deleteFile', path });
  }
  async renameFile(from: string, to: string): Promise<void> {
    await this.call({ op: 'renameFile', from, to });
  }
  async commitFiles(files: Record<string, Uint8Array>): Promise<void> {
    await this.call({ op: 'commitFiles', files });
  }
  async files(): Promise<Record<string, Uint8Array>> {
    return (await this.call({ op: 'files' })) as Record<string, Uint8Array>;
  }
  async sync(url: string, opts: { authKey?: string } = {}): Promise<number> {
    return (await this.call({ op: 'sync', url, authKey: opts.authKey })) as number;
  }
  async free(): Promise<void> {
    await this.call({ op: 'free' });
  }
}
