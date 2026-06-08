// Context for Obsidian — the v1 reference thin-client surface. A thin node on the
// @asp/sdk: it holds a complete local working copy, authors rows offline, and
// converges on reconnect by making an OUTBOUND connection to a full node (an `asp
// watch --listen` process or Context Desktop). It never runs the multi-tip merge
// and never listens/relays. NO protocol/merge/fold/auth logic lives here — any
// behavioral difference from the `asp` CLI is a bug; this file is host glue only
// (vault I/O, the event→push path, settings, a status bar).

import { Notice, Plugin, PluginSettingTab, Setting, type EventRef } from 'obsidian';
import { initAsp, normalizePeerUrl, Vault as SdkVault } from '../../../sdks/typescript/src/index.ts';
import { Bridge } from './bridge.ts';
import { LogBuffer } from './log-buffer.ts';
import { LogModal } from './log-modal.ts';
import { ObsidianHost } from './obsidian-host.ts';
import { PathFilter } from './path-filter.ts';
import { type SyncState, SyncController } from './sync-controller.ts';

// The wasm engine bytes, inlined at build time by esbuild (see
// esbuild.config.mjs). Decoded once and handed to `initAsp` so `main.js` is
// fully self-contained — no sibling .wasm to fetch.
declare const __ASP_WASM_B64__: string;
function wasmBytes(): Uint8Array {
  const bin = atob(__ASP_WASM_B64__);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

interface AspSettings {
  peerUrl: string;
  authKey: string;
  seedHex: string;
  enabled: boolean;
  /** True once a first connect has succeeded — gates the sync controls so a
   * fresh setup only shows connection fields + a Connect button. */
  connectedOnce: boolean;
}

const DEFAULTS: AspSettings = {
  peerUrl: '',
  authKey: '',
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
  private sdk?: SdkVault;
  private bridge?: Bridge;
  private controller?: SyncController;
  private statusEl?: HTMLElement;
  private debounce?: ReturnType<typeof setTimeout>;
  private pollTimer?: ReturnType<typeof setInterval>;
  /** Guards against overlapping syncs (a poll firing mid-sync would open a
   * second connection feeding the same engine). */
  private syncing = false;

  async onload(): Promise<void> {
    this.settings = Object.assign({ ...DEFAULTS }, (await this.loadData()) as AspSettings);
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

    this.log.append('plugin loading — initializing the wasm engine…');
    // Instantiate the wasm engine from the inlined bytes before any engine use
    // (the web target loads asynchronously). Idempotent.
    await initAsp(wasmBytes());

    // The one engine, in wasm — a thin node. Empty vault id adopts the peer's.
    this.sdk = new SdkVault(hexToBytes(this.settings.seedHex), '');
    const host = new ObsidianHost(this.app.vault.adapter);
    this.bridge = new Bridge(this.sdk, host, new PathFilter(await this.readIgnore(host)));
    this.controller = new SyncController(this.sdk, this.bridge);

    const logger = (msg: string, level?: 'info' | 'error') => this.log.append(msg, level);
    this.bridge.setLogger(logger);
    this.controller.setLogger(logger);
    this.log.append(`engine ready — device key ${this.deviceKey()}`);

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
      void this.runSync({ reconcile: true, quiet: true });
    }
    this.startPolling();
  }

  onunload(): void {
    this.stopPolling();
    clearTimeout(this.debounce);
    this.sdk?.free();
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
   * Returns whether it converged.
   */
  private async runSync(
    opts: { reconcile?: boolean; background?: boolean; quiet?: boolean } = {},
  ): Promise<boolean> {
    if (!this.controller) return false;
    if (this.syncing) return false; // a sync is already in flight
    const peerUrl = normalizePeerUrl(this.settings.peerUrl); // bare host → wss://
    if (!peerUrl) {
      if (!opts.quiet) new Notice('asp: set a Peer URL first');
      this.log.append('sync: no Peer URL set — nothing to connect to', 'error');
      return false;
    }
    this.syncing = true;
    try {
      await this.controller.syncOnce(
        { peerUrl, authKey: this.settings.authKey || undefined },
        { reconcile: opts.reconcile, background: opts.background },
      );
      if (!opts.quiet) new Notice('asp: synced');
      return true;
    } catch (e) {
      // The controller already logged the underlying error.
      if (!opts.quiet) new Notice(`asp sync failed: ${String(e)}`);
      return false;
    } finally {
      this.syncing = false;
    }
  }

  /** First-time connect (the stage-1 button). On success, unlock the sync
   * controls so the next settings render shows them. */
  async connect(): Promise<boolean> {
    this.log.append('connect: attempting first sync…');
    const ok = await this.runSync({ reconcile: true });
    if (ok && !this.settings.connectedOnce) {
      this.settings.connectedOnce = true;
      this.settings.enabled = true;
      await this.saveData(this.settings);
      this.log.append('connect: enrolled ✓ — sync controls unlocked');
    }
    return ok;
  }

  async syncNow(): Promise<void> {
    await this.runSync({ reconcile: true });
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

  constructor(
    app: AspPlugin['app'],
    private plugin: AspPlugin,
  ) {
    super(app, plugin);
  }

  hide(): void {
    this.unsubState?.();
    this.unsubState = undefined;
  }

  display(): void {
    this.hide();
    const root = this.containerEl;
    root.empty?.();
    while (root.firstChild) root.removeChild(root.firstChild);

    const connected = this.plugin.settings.connectedOnce;

    // Live status at the top — a native row whose control shows a dot + label.
    this.renderStatusRow(root);

    // Peer URL — both stages. A bare host is fine; wss:// is assumed.
    new Setting(root).setName('Peer URL').addText((t) =>
      t
        .setPlaceholder('hub:9000  (wss:// assumed)')
        .setValue(this.plugin.settings.peerUrl)
        .onChange(async (v) => {
          this.plugin.settings.peerUrl = v.trim();
          await this.plugin.saveData(this.plugin.settings);
        }),
    );

    if (!connected) {
      // ---- Stage 1: connecting. The auth key is an enrollment secret — it
      // only matters here, so it's hidden once connected.
      new Setting(root).setName('Auth key').addText((t) =>
        t.setValue(this.plugin.settings.authKey).onChange(async (v) => {
          this.plugin.settings.authKey = v.trim();
          await this.plugin.saveData(this.plugin.settings);
        }),
      );
      new Setting(root).setName('Connect').addButton((b) =>
        b.setButtonText('Connect').onClick(async () => {
          b.setButtonText('Connecting…');
          const ok = await this.plugin.connect();
          b.setButtonText('Connect');
          if (ok) this.display(); // reveal stage 2
        }),
      );
    } else {
      // ---- Stage 2: connected. No auth key, no manual "Sync now" button —
      // sync runs automatically on edit, and the command palette still offers
      // "Sync now".
      new Setting(root).setName('Sync enabled').addToggle((t) =>
        t.setValue(this.plugin.settings.enabled).onChange(async (v) => {
          this.plugin.settings.enabled = v;
          await this.plugin.saveData(this.plugin.settings);
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

    // Fires immediately with the current state, then on every change.
    this.unsubState = this.plugin.onSyncState((s) => {
      const info = statusInfo(s);
      dot.style.background = info.color;
      label.textContent = info.label;
    });
  }
}
