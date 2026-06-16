// Worker-side host: owns the in-process `Vault` (wasm engine + WebSocket
// transport) and services `Command`s from the main thread. Every heavy
// synchronous engine call (fold, merge, content hash, the session feed loop
// inside `sync`) runs HERE — off the renderer thread — so the Obsidian UI
// never freezes during a sync.

import { initEngine } from '../engine.ts';
import { Vault } from '../vault.ts';
import type { Port } from './channel.ts';
import type { Command, FromWorker, Identity, Reply, ToWorker } from './protocol.ts';

export class EngineWorkerHost {
  private vault?: Vault;

  constructor(private readonly port: Port<FromWorker, ToWorker>) {
    port.onMessage((cmd) => void this.handle(cmd));
  }

  private reply(id: number, ok: boolean, value?: Reply['value'], error?: string): void {
    this.port.post({ kind: 'reply', id, ok, value, error });
  }

  private vaultOrThrow(): Vault {
    if (!this.vault) throw new Error('engine worker: not initialized');
    return this.vault;
  }

  private async handle(cmd: Command): Promise<void> {
    try {
      switch (cmd.op) {
        case 'init': {
          // No-op on the nodejs glue (tests); instantiates the wasm from the
          // inlined bytes under the browser/worker glue.
          await initEngine(cmd.payload.wasmBytes);
          const v = new Vault(cmd.payload.seed, cmd.payload.vaultId);
          this.vault = v;
          const id: Identity = { nodeSsh: v.nodeSsh(), nodeId: v.nodeId(), vaultId: v.vaultId() };
          return this.reply(cmd.id, true, id);
        }
        case 'writeFile':
          this.vaultOrThrow().writeFile(cmd.path, cmd.bytes);
          return this.reply(cmd.id, true);
        case 'deleteFile':
          this.vaultOrThrow().deleteFile(cmd.path);
          return this.reply(cmd.id, true);
        case 'deleteFiles':
          this.vaultOrThrow().deleteFiles(cmd.paths);
          return this.reply(cmd.id, true);
        case 'renameFile':
          this.vaultOrThrow().renameFile(cmd.from, cmd.to);
          return this.reply(cmd.id, true);
        case 'commitFiles':
          this.vaultOrThrow().commitFiles(cmd.files);
          return this.reply(cmd.id, true);
        case 'writeFiles':
          this.vaultOrThrow().writeFiles(cmd.files);
          return this.reply(cmd.id, true);
        case 'files':
          return this.reply(cmd.id, true, this.vaultOrThrow().files());
        case 'filesDetail':
          return this.reply(cmd.id, true, this.vaultOrThrow().filesDetail());
        case 'readFile':
          return this.reply(cmd.id, true, this.vaultOrThrow().readFile(cmd.path));
        case 'dump':
          return this.reply(cmd.id, true, this.vaultOrThrow().dump());
        case 'load':
          this.vaultOrThrow().load(cmd.stateJson);
          return this.reply(cmd.id, true);
        case 'dumpState':
          // Binary state (rows + each blob once) — structured-clone-safe, and
          // far smaller than the legacy JSON dump on a large vault.
          return this.reply(cmd.id, true, this.vaultOrThrow().dumpState());
        case 'loadState':
          return this.reply(cmd.id, true, this.vaultOrThrow().loadState(cmd.bytes));
        case 'sync': {
          const integrated = await this.vaultOrThrow().sync(cmd.ticket, { authKey: cmd.authKey, relayUrl: cmd.relayUrl });
          return this.reply(cmd.id, true, integrated);
        }
        case 'abort':
          // Handled while the in-flight `sync` handler is suspended at its await:
          // closes that sync's socket so it rejects and its reply unblocks.
          this.vault?.cancel();
          return this.reply(cmd.id, true);
        case 'free':
          this.vault?.free();
          this.vault = undefined;
          return this.reply(cmd.id, true);
      }
    } catch (e) {
      // Send the bare message across the worker boundary — worker-vault re-wraps
      // it in `new Error(...)`, so String(e)'s "Error: " prefix would double up.
      this.reply(cmd.id, false, undefined, e instanceof Error ? e.message : String(e));
    }
  }
}
