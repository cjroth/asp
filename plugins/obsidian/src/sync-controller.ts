// The sync controller: drives connect + catch-up over the SDK and renders the
// converged tree back to the host. A thin node never runs the multi-tip merge or
// listens — it makes an outbound connection to a full node (an `asp watch
// --listen` process or Context Desktop) which serves the merged tree.

import type { Vault } from '../../../sdks/typescript/src/index.ts';
import type { Bridge } from './bridge.ts';
import type { Logger } from './log-buffer.ts';

export type SyncState = 'idle' | 'connecting' | 'connected' | 'error';

export interface SyncConfig {
  peerUrl: string;
  authKey?: string;
}

export class SyncController {
  state: SyncState = 'idle';
  lastError?: string;
  private onState?: (s: SyncState) => void;
  private log: Logger = () => {};

  constructor(
    private vault: Vault,
    private bridge: Bridge,
  ) {}

  setLogger(log: Logger) {
    this.log = log;
  }

  subscribe(cb: (s: SyncState) => void) {
    this.onState = cb;
  }

  private set(s: SyncState, err?: string) {
    this.state = s;
    this.lastError = err;
    this.onState?.(s);
  }

  /** One sync pass: reconcile local changes, connect, catch up, materialize. */
  async syncOnce(cfg: SyncConfig): Promise<void> {
    this.set('connecting');
    this.log(`sync: connecting to ${cfg.peerUrl}${cfg.authKey ? ' (with auth key)' : ''}…`);
    try {
      await this.bridge.reconcileFromHost();
      await this.vault.sync(cfg.peerUrl, { authKey: cfg.authKey });
      this.log('sync: handshake + catch-up complete');
      const { written, removed } = await this.bridge.materializeToHost();
      this.set('connected');
      this.log(`sync: connected ✓ (${written} written, ${removed} removed)`);
    } catch (e) {
      const msg = String(e);
      this.set('error', msg);
      this.log(`sync failed: ${msg}`, 'error');
      throw e;
    }
  }
}
