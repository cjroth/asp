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

  /**
   * One sync pass: (optionally) re-capture the host tree, connect, catch up,
   * and materialize ONLY when the peer actually sent something.
   *
   * - `reconcile` (default false): re-read every host file into the engine.
   *   Skip it on the hot path — live edits are already captured by the host
   *   event handlers, so re-reading the whole vault on every sync is the main
   *   source of UI lag. Pass it only for the initial capture / manual recovery.
   * - `background` (default false): a periodic poll — don't flash the status to
   *   "connecting" each tick (avoids a 10s dot flicker once connected).
   */
  async syncOnce(cfg: SyncConfig, opts: { reconcile?: boolean; background?: boolean } = {}): Promise<void> {
    if (!opts.background || this.state !== 'connected') this.set('connecting');
    if (!opts.background) {
      this.log(`sync: connecting to ${cfg.peerUrl}${cfg.authKey ? ' (with auth key)' : ''}…`);
    }
    try {
      if (opts.reconcile) await this.bridge.reconcileFromHost();
      const integrated = await this.vault.sync(cfg.peerUrl, { authKey: cfg.authKey });
      // Only rewrite the host tree when the peer sent new rows — otherwise the
      // O(files) materialize scan runs for nothing on every no-op poll.
      if (integrated > 0) {
        const { written, removed } = await this.bridge.materializeToHost();
        this.log(`sync: pulled ${integrated} row(s) → ${written} written, ${removed} removed`);
      } else if (!opts.background) {
        this.log('sync: up to date (nothing new from peer)');
      }
      this.set('connected');
    } catch (e) {
      const msg = String(e);
      this.set('error', msg);
      this.log(`sync failed: ${msg}`, 'error');
      throw e;
    }
  }
}
