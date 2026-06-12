// The sync controller: drives connect + catch-up over the SDK and renders the
// converged tree back to the host. A thin node never runs the multi-tip merge or
// listens — it makes an outbound connection to a full node (an `asp watch
// --listen` process or Context Desktop) which serves the merged tree.

import type { EngineVault } from '../../../sdks/typescript/src/index.ts';
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
    private vault: EngineVault,
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

  /** Return to the pristine 'idle' state — used when the user resets the sync
   * config (the remote is forgotten, so any prior connected/error state is
   * stale). Notifies subscribers so the status row repaints. */
  reset() {
    this.set('idle');
  }

  /** Abort an in-flight connect/sync — e.g. a connect to a mistyped URL that's
   * hanging. Closes the socket (the pending `vault.sync` rejects) and returns to
   * idle. Safe to call when nothing is in flight. */
  async cancel(): Promise<void> {
    this.log('sync: cancelling…');
    try {
      await this.vault.cancel();
    } catch {
      /* nothing in flight */
    }
    this.set('idle');
  }

  /**
   * One sync pass: (optionally) re-capture the host tree, connect, catch up,
   * and materialize ONLY when the peer actually sent something.
   *
   * - `reconcile` (default false): re-read every host file into the engine.
   *   Skip it on the hot path — live edits are already captured by the host
   *   event handlers, so re-reading the whole vault on every sync is the main
   *   source of UI lag. Pass it only for the initial capture / manual recovery.
   * - `captureDeletes` (default false): during the reconcile, also author
   *   deletes for files the engine holds but the disk no longer has — captures
   *   deletions made while the app was closed. ONLY safe on a warm engine
   *   (restored state / already synced); on a cold engine it would delete
   *   everything not yet materialized. The plugin gates it.
   * - `background` (default false): a periodic poll — don't flash the status to
   *   "connecting" each tick (avoids a 10s dot flicker once connected).
   */
  async syncOnce(
    cfg: SyncConfig,
    opts: {
      reconcile?: boolean;
      captureDeletes?: boolean;
      background?: boolean;
      adoptFirst?: boolean;
    } = {},
  ): Promise<void> {
    if (!opts.background || this.state !== 'connected') this.set('connecting');
    if (!opts.background) {
      this.log(`sync: connecting to ${cfg.peerUrl}${cfg.authKey ? ' (with auth key)' : ''}…`);
    }
    try {
      // `adoptFirst`: the engine was rebuilt fresh (no persisted state to restore)
      // and the host disk may hold files that already exist on the peer. If we
      // reconciled now, those files would be authored with NEW ids that COLLIDE
      // with the peer's ids for the same path → the fold disambiguates them to
      // `a (1).md` and they multiply every reload (the duplicate-explosion loop).
      // So pull the peer's canonical state FIRST so reconcileFromHost matches by
      // path and reuses the peer's id. Crucially, do NOT materialize between the
      // pull and reconcile — that removal pass would delete local-only files
      // before reconcile adds them to the engine (data loss). A warm/restored
      // engine skips this and uses the plain reconcile→sync order (3-way merge).
      let adopted = 0;
      if (opts.adoptFirst) {
        adopted = await this.vault.sync(cfg.peerUrl, { authKey: cfg.authKey });
      }
      if (opts.reconcile) {
        await this.bridge.reconcileFromHost({ captureDeletes: opts.captureDeletes });
      }
      const integrated = await this.vault.sync(cfg.peerUrl, { authKey: cfg.authKey });
      // Materialize AFTER reconcile (so the engine holds peer + local files and
      // the removal pass only drops genuinely-gone files). Skip on no-op polls.
      if (adopted > 0 || integrated > 0) {
        const { written, removed } = await this.bridge.materializeToHost();
        this.log(`sync: pulled ${adopted + integrated} row(s) → ${written} written, ${removed} removed`);
      } else if (!opts.background) {
        this.log('sync: up to date (nothing new from peer)');
      }
      this.set('connected');
    } catch (e) {
      let msg = e instanceof Error ? e.message : String(e);
      // A user-initiated cancel is not a failure — drop back to idle quietly
      // (the cancel() call already set idle; don't paint a red "error").
      if (/cancell?ed/i.test(msg)) {
        this.set('idle');
        this.log('sync: cancelled');
        throw e;
      }
      // A browser/Electron WebSocket can't read the HTTP status of a failed
      // upgrade — a hub 401 (wrong auth key) surfaces only as a generic
      // pre-handshake connect error. If we presented an auth key, that's the
      // likely cause: the key didn't match. (An already-authorized device
      // needs no auth key at all.)
      if (cfg.authKey && /ws connect failed|before handshake|connection error/i.test(msg)) {
        msg += ' — the hub rejected the upgrade. The auth key looks wrong; if this device is already authorized you can leave it blank.';
      }
      this.set('error', msg);
      this.log(`sync failed: ${msg}`, 'error');
      throw e;
    }
  }
}
