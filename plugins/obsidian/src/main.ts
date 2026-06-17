// Context for Obsidian — the v1 reference thin-client surface. A thin node on the
// @asp/sdk: it holds a complete local working copy, authors rows offline, and
// converges on reconnect by making an OUTBOUND connection to a full node (an `asp
// watch --listen` process or Context Desktop). It never runs the multi-tip merge
// and never listens/relays. NO protocol/merge/fold/auth logic lives here — any
// behavioral difference from the `asp` CLI is a bug; this file is host glue only
// (vault I/O, the event→push path, settings, a status bar).

import { Notice, Plugin, PluginSettingTab, Setting, type EventRef } from 'obsidian';
import {
  type FromWorker,
  normalizePeerUrl,
  type ToWorker,
  WorkerVault,
  workerPort,
} from '../../../sdks/typescript/src/index.ts';
import { Bridge } from './bridge.ts';
import { LogBuffer } from './log-buffer.ts';
import { LogModal } from './log-modal.ts';
import { ConfirmModal } from './confirm-modal.ts';
import { ObsidianHost } from './obsidian-host.ts';
import { PathFilter } from './path-filter.ts';
import { type SyncState, SyncController } from './sync-controller.ts';

// Inlined at build time by esbuild (see esbuild.config.mjs), so `main.js` is
// fully self-contained — no sibling files to fetch:
//   • the wasm engine bytes (base64), shipped to the worker's `init`;
//   • the engine Web Worker bundle (IIFE source), started as a Blob Worker.
declare const __ASP_WASM_B64__: string;
declare const __ASP_ENGINE_WORKER__: string;
function wasmBytes(): Uint8Array {
  const bin = atob(__ASP_WASM_B64__);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

interface AspSettings {
  /** The peer's iroh ticket (or bare node id) — what `asp watch --listen` prints. */
  peerUrl: string;
  /** Optional relay override (a self-hosted `asp relay`); blank = public relays. */
  relayUrl: string;
  /** When false the relay field is hidden and the public relays are used; flipping
   * the "Use custom relay" toggle on reveals `relayUrl`. */
  useCustomRelay: boolean;
  seedHex: string;
  enabled: boolean;
  /** True once a first connect has succeeded — gates the sync controls so a
   * fresh setup only shows connection fields + a Connect button. */
  connectedOnce: boolean;
}

const DEFAULTS: AspSettings = {
  peerUrl: '',
  relayUrl: '',
  useCustomRelay: false,
  seedHex: '',
  enabled: false,
  connectedOnce: false,
};

/** How often to poll the peer for changes. A thin node makes outbound,
 * one-shot syncs (it never listens), so a peer-side change only lands on the
 * next sync — without this, a remote rename never shows up until a *local*
 * edit triggers one. Each poll is cheap now (no full re-read; materialize only
 * runs when rows actually arrive). */
const POLL_MS = 10_000;

function randomSeedHex(): string {
  const b = new Uint8Array(32);
  crypto.getRandomValues(b);
  return [...b].map((x) => x.toString(16).padStart(2, '0')).join('');
}
function hexToBytes(h: string): Uint8Array {
  const out = new Uint8Array(h.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = Number.parseInt(h.slice(i * 2, i * 2 + 2), 16);
  return out;
}

export default class AspPlugin extends Plugin {
  settings: AspSettings = { ...DEFAULTS };
  /** In-app trace, visible in settings before a sync ever connects — the only
   * way to read what the plugin is doing on mobile (no dev console there). */
  readonly log = new LogBuffer();
  /** Last sync state, mirrored to the status bar AND any open settings banner. */
  syncState: SyncState = 'idle';
  private readonly stateListeners = new Set<(s: SyncState) => void>();
  private worker?: Worker;
  private sdk?: WorkerVault;
  private bridge?: Bridge;
  private controller?: SyncController;
  private statusEl?: HTMLElement;
  private debounce?: ReturnType<typeof setTimeout>;
  private pollTimer?: ReturnType<typeof setInterval>;
  /** Guards against overlapping syncs (a poll firing mid-sync would open a
   * second connection feeding the same engine). */
  private syncing = false;
  /** One-time enrollment secret for the next connect attempt. In memory only —
   * never persisted (a stored auth key would silently 401 the upgrade once the
   * hub rotates it or once the device is already enrolled). Discarded on use. */
  pendingAuthKey = '';
  private saveStateTimer?: ReturnType<typeof setTimeout>;
  /** True once the engine's view reflects this device's state — set when
   * persisted state is restored on load, or after the first successful sync.
   * Warm: reconcile matches existing ids (no adopt-first needed) and may
   * safely capture offline deletions (engine-known paths missing from disk
   * really were deleted). Cold: neither holds — reconcile must adopt the
   * peer's ids first and must NOT author deletes. */
  private engineWarm = false;

  async onload(): Promise<void> {
    const loaded = ((await this.loadData()) as Partial<AspSettings> & { authKey?: string }) ?? {};
    // Purge any auth key persisted by an older build — it's enrollment-only and
    // must not linger (a stale value silently 401s the upgrade on reconnect).
    const hadStoredAuthKey = typeof loaded.authKey === 'string' && loaded.authKey !== '';
    delete loaded.authKey;
    this.settings = Object.assign({ ...DEFAULTS }, loaded);
    if (hadStoredAuthKey) {
      await this.saveData(this.settings); // rewrite without the dropped key
      this.log.append('migrated: dropped stored auth key (now enrollment-only, in memory)');
    }
    if (!this.settings.seedHex) {
      this.settings.seedHex = randomSeedHex();
      await this.saveData(this.settings);
    }
    // Migration: a tester upgrading from a build without staged settings who
    // already has a peer configured has clearly connected before — don't bounce
    // them back to the stage-1 "Connect" screen.
    if (!this.settings.connectedOnce && this.settings.peerUrl) {
      this.settings.connectedOnce = true;
      await this.saveData(this.settings);
    }

    this.log.append('plugin loading — starting the engine worker…');
    // The one engine, in wasm — but inside a Web Worker, so its synchronous
    // fold/merge/hash/feed work never blocks the Obsidian UI. The worker source
    // and wasm bytes are both inlined; we start it from a Blob (no sibling file)
    // and ship the bytes in via `init`. Empty vault id adopts the peer's.
    const blob = new Blob([__ASP_ENGINE_WORKER__], { type: 'application/javascript' });
    const url = URL.createObjectURL(blob);
    this.worker = new Worker(url);
    URL.revokeObjectURL(url);
    this.worker.onerror = (e) =>
      this.log.append(`engine worker error: ${e.message ?? String(e)}`, 'error');
    this.sdk = new WorkerVault(workerPort<ToWorker, FromWorker>(this.worker));
    const identity = await this.sdk.init({
      seed: hexToBytes(this.settings.seedHex),
      vaultId: '',
      wasmBytes: wasmBytes(),
    });
    this.log.append(`engine worker ready — device key ${identity.nodeSsh}`);

    // Restore persisted engine state BEFORE any reconcile. The engine is rebuilt
    // fresh each load; without this it would re-import its own materialized tree
    // (incl. the fold's `a (1).md` disambiguations) as brand-new files, which
    // collide and multiply every reload (the duplicate-explosion loop). Restoring
    // gives reconcileFromHost the real file ids so it matches by path instead.
    await this.restoreEngineState();

    const host = new ObsidianHost(this.app.vault.adapter);
    this.bridge = new Bridge(this.sdk, host, new PathFilter(await this.readIgnore(host)));
    this.controller = new SyncController(this.sdk, this.bridge);

    const logger = (msg: string, level?: 'info' | 'error') => this.log.append(msg, level);
    this.bridge.setLogger(logger);
    this.controller.setLogger(logger);

    this.statusEl = this.addStatusBarItem();
    this.controller.subscribe((s) => {
      this.syncState = s;
      this.renderStatus(s);
      for (const l of this.stateListeners) l(s);
    });
    this.renderStatus('idle');

    // Obsidian vault events → capture into the engine (debounced), then sync.
    const push = () => this.scheduleSync();
    for (const [evt, handler] of [
      ['create', (f: { path: string }) => this.bridge?.pushWrite(f.path)],
      ['modify', (f: { path: string }) => this.bridge?.pushWrite(f.path)],
      ['delete', (f: { path: string }) => this.bridge?.pushDelete(f.path)],
    ] as const) {
      const ref: EventRef = this.app.vault.on(evt, (f: unknown) => {
        void handler(f as { path: string });
        push();
      });
      this.registerEvent(ref);
    }
    this.registerEvent(
      this.app.vault.on('rename', (f: unknown, oldPath: unknown) => {
        void this.bridge?.pushRename(String(oldPath), (f as { path: string }).path);
        push();
      }),
    );

    this.addCommand({ id: 'asp-sync-now', name: 'Sync now', callback: () => void this.syncNow() });
    this.addSettingTab(new AspSettingTab(this.app, this));

    // Initial full sync (captures the whole vault into the engine once), then a
    // lightweight periodic pull so peer-side changes appear without a local edit.
    if (this.settings.enabled && this.settings.peerUrl) {
      this.log.append('startup: initial sync…');
      // runSync derives adopt-first / capture-deletes from engine warmth.
      void this.runSync({ reconcile: true, quiet: true });
    }
    this.startPolling();
  }

  onunload(): void {
    this.stopPolling();
    clearTimeout(this.debounce);
    void this.sdk?.free();
    this.worker?.terminate();
    this.worker = undefined;
  }

  /** Periodic background pull. Cheap: no full re-read, and materialize only
   * runs when the peer actually sent rows. Skips ticks while disabled, without
   * a peer, or while another sync is in flight. */
  private startPolling(): void {
    this.stopPolling();
    this.pollTimer = setInterval(() => {
      if (!this.settings.enabled || !this.settings.peerUrl || !this.settings.connectedOnce) return;
      void this.runSync({ reconcile: false, background: true, quiet: true });
    }, POLL_MS);
  }

  private stopPolling(): void {
    if (this.pollTimer) {
      clearInterval(this.pollTimer);
      this.pollTimer = undefined;
    }
  }

  /** Subscribe to sync-state changes (the settings banner uses this). Fires
   * immediately with the current state; returns an unsubscribe. */
  onSyncState(cb: (s: SyncState) => void): () => void {
    this.stateListeners.add(cb);
    cb(this.syncState);
    return () => this.stateListeners.delete(cb);
  }

  /**
   * Run one sync pass. `reconcile` re-captures the whole vault (initial /
   * manual recovery); `background` keeps the status steady for polls; `quiet`
   * suppresses the toast (used by automatic syncs). Never overlaps another sync.
   * Adopt-first vs capture-deletes is derived from engine warmth here — a cold
   * engine must pull the peer's ids before reconciling (the duplicate-explosion
   * loop) and must never author deletes for not-yet-materialized files; a warm
   * one must do the opposite, or files deleted while the app was closed
   * resurrect on every launch. Returns whether it converged.
   */
  private async runSync(
    opts: { reconcile?: boolean; background?: boolean; quiet?: boolean } = {},
  ): Promise<boolean> {
    if (!this.controller) return false;
    if (this.syncing) return false; // a sync is already in flight
    const peerUrl = normalizePeerUrl(this.settings.peerUrl); // a ticket / node id (trimmed)
    if (!peerUrl) {
      if (!opts.quiet) new Notice('asp: set a peer ticket first');
      this.log.append('sync: no peer ticket set — nothing to connect to', 'error');
      return false;
    }
    this.syncing = true;
    try {
      await this.controller.syncOnce(
        {
          peerUrl,
          authKey: this.pendingAuthKey || undefined,
          relayUrl: (this.settings.useCustomRelay && this.settings.relayUrl) || undefined,
        },
        {
          reconcile: opts.reconcile,
          captureDeletes: opts.reconcile && this.engineWarm,
          background: opts.background,
          // Only meaningful when a reconcile follows (it exists to give the
          // reconcile the peer's ids); a non-reconcile pass pulls anyway.
          adoptFirst: opts.reconcile && !this.engineWarm,
        },
      );
      this.engineWarm = true; // the engine now reflects this device's state
      if (!opts.quiet) new Notice('asp: synced');
      return true;
    } catch (e) {
      // The controller already logged the underlying error.
      if (!opts.quiet) new Notice(`asp sync failed: ${e instanceof Error ? e.message : String(e)}`);
      return false;
    } finally {
      // Persist engine state so reloads don't re-import — also after a FAILED
      // pass (offline edits/deletes were still captured as rows and must
      // survive an app kill). Never while cold: persisting a never-synced
      // engine would make the next launch skip adopt-first and mint colliding
      // ids for every file already on the peer.
      if (this.engineWarm) this.scheduleSaveState();
      this.syncing = false;
    }
  }

  /** First-time connect (the stage-1 button). On success, unlock the sync
   * controls so the next settings render shows them. */
  async connect(): Promise<boolean> {
    this.log.append('connect: attempting first sync…');
    const ok = await this.runSync({ reconcile: true });
    if (ok) {
      // Enrollment done — burn the one-time auth key so it never lingers.
      this.pendingAuthKey = '';
      if (!this.settings.connectedOnce) {
        this.settings.connectedOnce = true;
        this.settings.enabled = true;
        await this.saveData(this.settings);
        this.log.append('connect: enrolled ✓ — sync controls unlocked');
      }
    }
    return ok;
  }

  async syncNow(): Promise<void> {
    await this.runSync({ reconcile: true });
  }

  /** Abort an in-flight connect/sync — the "Cancel" affordance for a connect
   * that's hanging (e.g. a mistyped Peer URL). Closes the socket so the pending
   * sync rejects, and frees the `syncing` latch so a corrected URL can be tried
   * at once. */
  async cancelSync(): Promise<void> {
    this.log.append('connect: cancelled by user');
    await this.controller?.cancel();
    this.syncing = false;
  }

  /** File holding the serialized engine state (compact msgpack: rows + each
   * blob once), inside the plugin's own dir so it travels with the install but
   * isn't a vault note. */
  private statePath(): string {
    return `${this.manifest.dir}/engine-state.bin`;
  }

  /** The pre-0.1.21 state file: a JSON dump that duplicated blobs per row and
   * inflated every byte to ~4 chars — large vaults OOM'd the worker saving it,
   * so every launch cold-started. Read once on upgrade, then replaced. */
  private legacyStatePath(): string {
    return `${this.manifest.dir}/engine-state.json`;
  }

  /** Re-integrate persisted engine state, if any, on startup — BEFORE the first
   * reconcile, so the engine knows its real file ids and reconcileFromHost
   * matches by path instead of re-importing the materialized tree as new files
   * (the duplicate-explosion loop). */
  private async restoreEngineState(): Promise<void> {
    const adapter = this.app.vault.adapter;
    try {
      const p = this.statePath();
      if (await adapter.exists(p)) {
        const rows = await this.sdk.loadState(new Uint8Array(await adapter.readBinary(p)));
        this.engineWarm = true;
        this.log.append(`restored engine state (${rows} rows) — reconcile will match existing files`);
        return;
      }
      // Upgrade path: restore the legacy JSON dump once, then re-save in the
      // compact format and drop the old file.
      const legacy = this.legacyStatePath();
      if (await adapter.exists(legacy)) {
        const json = await adapter.read(legacy);
        if (json) {
          await this.sdk.load(json);
          this.engineWarm = true;
          await this.saveEngineState();
          await adapter.remove(legacy);
          this.log.append('migrated legacy engine state to the compact format');
        }
      }
    } catch (e) {
      this.log.append(`restore engine state failed (will reconcile fresh): ${String(e)}`, 'error');
    }
  }

  /** Debounced: persist the engine state so the next launch restores instead of
   * re-importing. Serializing the whole log is non-trivial, so coalesce bursts. */
  private scheduleSaveState(): void {
    clearTimeout(this.saveStateTimer);
    this.saveStateTimer = setTimeout(() => void this.saveEngineState(), 2000);
  }

  private async saveEngineState(): Promise<void> {
    try {
      const bytes = await this.sdk.dumpState();
      // Slice to the view's exact range — writeBinary takes an ArrayBuffer, and
      // the Uint8Array from the worker may sit in a larger buffer.
      const buf = bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength);
      await this.app.vault.adapter.writeBinary(this.statePath(), buf as ArrayBuffer);
    } catch (e) {
      this.log.append(`save engine state failed: ${String(e)}`, 'error');
    }
  }

  /** Forget the remote so the user can set sync up from scratch (re-enter a URL
   * and, if needed, an enrollment key). Clears the peer/connection config only —
   * the vault history, the engine, and this device's identity (seedHex) are all
   * left intact. Drops back to the stage-1 connection flow. */
  async resetSyncConfig(): Promise<void> {
    clearTimeout(this.debounce);
    this.pendingAuthKey = '';
    this.settings.peerUrl = '';
    this.settings.connectedOnce = false;
    this.settings.enabled = false;
    await this.saveData(this.settings);
    this.controller?.reset(); // drop stale connected/error state → idle
    this.log.append('sync config reset — remote forgotten (vault history kept)');
  }

  private async readIgnore(host: ObsidianHost): Promise<string> {
    const b = await host.read('.aspignore');
    return b ? new TextDecoder().decode(b) : '';
  }

  private scheduleSync(): void {
    if (!this.settings.enabled || !this.settings.peerUrl) return;
    clearTimeout(this.debounce);
    // A local edit was already captured by the event handler — just push it.
    // No reconcile (don't re-read the whole vault), no toast, no status flash.
    this.debounce = setTimeout(
      () => void this.runSync({ reconcile: false, background: true, quiet: true }),
      600,
    );
  }

  deviceKey(): string {
    return this.sdk?.nodeSsh() ?? '';
  }

  private renderStatus(s: string): void {
    if (this.statusEl) this.statusEl.setText?.(`asp: ${s}`) ?? (this.statusEl.textContent = `asp: ${s}`);
  }
}

/** Map a sync state to a short status label + dot colour. */
function statusInfo(s: SyncState): { label: string; color: string } {
  switch (s) {
    case 'connecting':
      return { label: 'Syncing…', color: 'var(--color-yellow, #d29922)' };
    case 'connected':
      return { label: 'In sync', color: 'var(--color-green, #3fb950)' };
    case 'error':
      return { label: 'Not connected', color: 'var(--color-red, #f85149)' };
    default:
      return { label: 'Not connected', color: 'var(--text-muted, #888)' };
  }
}

class AspSettingTab extends PluginSettingTab {
  // Live status subscription, torn down on hide/re-render.
  private unsubState?: () => void;
  // Repaints the status row from current state + the enabled toggle. Held so the
  // toggle can refresh the status immediately (flipping `enabled` doesn't emit a
  // SyncState change on its own).
  private applyStatus?: () => void;

  constructor(
    app: AspPlugin['app'],
    private plugin: AspPlugin,
  ) {
    super(app, plugin);
  }

  hide(): void {
    this.unsubState?.();
    this.unsubState = undefined;
    this.applyStatus = undefined;
  }

  display(): void {
    this.hide();
    const root = this.containerEl;
    root.empty?.();
    while (root.firstChild) root.removeChild(root.firstChild);

    const connected = this.plugin.settings.connectedOnce;

    // Live status at the top — a native row whose control shows a dot + label.
    this.renderStatusRow(root);

    // Peer ticket — both stages. Paste the iroh ticket that `asp watch --listen`
    // (the hub) prints on start (or a bare node id).
    new Setting(root)
      .setName('Peer Ticket')
      .addText((t) =>
        t
          .setPlaceholder('paste the hub ticket…')
          .setValue(this.plugin.settings.peerUrl)
          .onChange(async (v) => {
            this.plugin.settings.peerUrl = v.trim();
            await this.plugin.saveData(this.plugin.settings);
          }),
      );

    // Relay is hidden by default — the public relays just work. A "Use custom
    // relay" toggle reveals the field for the rare self-hosted `asp relay` case.
    new Setting(root)
      .setName('Use Custom Relay')
      .addToggle((t) =>
        t.setValue(this.plugin.settings.useCustomRelay).onChange(async (v) => {
          this.plugin.settings.useCustomRelay = v;
          await this.plugin.saveData(this.plugin.settings);
          this.display(); // show/hide the relay field
        }),
      );
    if (this.plugin.settings.useCustomRelay) {
      new Setting(root)
        .setName('Relay URL')
        .setDesc('A self-hosted `asp relay`, e.g. http://relay.example:8080.')
        .addText((t) =>
          t
            .setPlaceholder('http://relay.example:8080')
            .setValue(this.plugin.settings.relayUrl)
            .onChange(async (v) => {
              this.plugin.settings.relayUrl = v.trim();
              await this.plugin.saveData(this.plugin.settings);
            }),
        );
    }

    if (!connected) {
      // ---- Stage 1: connecting. The auth key is a one-time ENROLLMENT secret:
      // it admits a device the hub doesn't trust yet, after which the device's
      // own key is authorized and the auth key is never needed again. So it is
      // NEVER persisted — it lives only in memory for this connect attempt and
      // is discarded once used (a stale stored key would silently 401 the
      // upgrade before the device key is ever checked). See `pendingAuthKey`.
      new Setting(root)
        .setName('Auth key')
        .setDesc('One-time enrollment secret. Only needed if this device is not yet authorized on the hub. Not saved.')
        .addText((t) =>
          t.setValue(this.plugin.pendingAuthKey).onChange((v) => {
            this.plugin.pendingAuthKey = v.trim();
          }),
        );
      new Setting(root).setName('Connect').addButton((b) =>
        b.setButtonText('Connect').onClick(async () => {
          // Toggle: while a connect is in flight this same button cancels it, so
          // a hanging connect (e.g. a mistyped URL) is never a dead end.
          if (this.plugin.syncState === 'connecting') {
            await this.plugin.cancelSync();
            b.setButtonText('Connect');
            return;
          }
          b.setButtonText('Cancel');
          const ok = await this.plugin.connect();
          b.setButtonText('Connect');
          if (ok) this.display(); // reveal stage 2
        }),
      );
    } else {
      // ---- Stage 2: connected. No auth key, no manual "Sync now" button —
      // sync runs automatically on edit, and the command palette still offers
      // "Sync now".
      new Setting(root).setName('Sync Enabled').addToggle((t) =>
        t.setValue(this.plugin.settings.enabled).onChange(async (v) => {
          this.plugin.settings.enabled = v;
          await this.plugin.saveData(this.plugin.settings);
          this.applyStatus?.(); // off → "Paused" right away (no SyncState change otherwise)
          if (v) void this.plugin.syncNow();
        }),
      );

      // The device's public identity — a read-only field (scrolls within the
      // input, so the long key can't overflow on mobile) + Copy.
      new Setting(root)
        .setName("This Device's Public Key")
        .addText((t) => t.setValue(this.plugin.deviceKey()).setDisabled(true))
        .addButton((b) =>
          b.setButtonText('Copy').onClick(() => {
            void navigator.clipboard?.writeText(this.plugin.deviceKey());
            new Notice('asp: public key copied');
          }),
        );
    }

    // Log — both stages. A button that opens the viewer modal (readable +
    // copyable on mobile, where the dev console isn't reachable).
    new Setting(root)
      .setName('Log')
      .addButton((b) =>
        b.setButtonText('Open log').onClick(() => new LogModal(this.app, this.plugin.log).open()),
      );

    // Reset — below the log (stage 2 only). Forgets the remote and returns to
    // the connection flow; vault history and device identity are kept.
    if (connected) {
      new Setting(root)
        .setName('Reset Sync Config')
        .addButton((b) =>
          b
            .setButtonText('Reset')
            .setWarning()
            .onClick(() => {
              new ConfirmModal(this.app, {
                title: 'Reset sync config?',
                body: 'This forgets the hub URL so you can set sync up from scratch. Your vault history and this device’s identity are kept.',
                confirmText: 'Reset',
                onConfirm: async () => {
                  await this.plugin.resetSyncConfig();
                  new Notice('asp: sync config reset');
                  this.display(); // back to stage 1
                },
              }).open();
            }),
        );
    }
  }

  /** A native "Status" row whose control shows a live dot + label. */
  private renderStatusRow(root: HTMLElement): void {
    const setting = new Setting(root).setName('Status');
    const dot = document.createElement('span');
    Object.assign(dot.style, {
      width: '10px',
      height: '10px',
      borderRadius: '50%',
      display: 'inline-block',
      verticalAlign: 'middle',
      marginRight: '8px',
      flex: '0 0 auto',
    } as Partial<CSSStyleDeclaration>);
    const label = document.createElement('span');
    setting.controlEl.appendChild(dot);
    setting.controlEl.appendChild(label);

    let cur: SyncState = this.plugin.syncState;
    const paint = () => {
      // When the user has turned sync off (after connecting), say so explicitly
      // instead of leaving the last "In sync" result on screen — that stale
      // label reads as if sync were still running.
      const paused = this.plugin.settings.connectedOnce && !this.plugin.settings.enabled;
      const info = paused ? { label: 'Paused', color: 'var(--text-muted, #888)' } : statusInfo(cur);
      dot.style.background = info.color;
      label.textContent = info.label;
    };
    this.applyStatus = paint;
    // Fires immediately with the current state, then on every change.
    this.unsubState = this.plugin.onSyncState((s) => {
      cur = s;
      paint();
    });
  }
}
