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
import { type LogEntry, LogBuffer } from './log-buffer.ts';
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

    if (this.settings.enabled && this.settings.peerUrl) {
      this.log.append('auto-sync on load (enabled + peer set)');
      void this.syncNow();
    }
  }

  onunload(): void {
    this.sdk?.free();
  }

  /** Subscribe to sync-state changes (the settings banner uses this). Fires
   * immediately with the current state; returns an unsubscribe. */
  onSyncState(cb: (s: SyncState) => void): () => void {
    this.stateListeners.add(cb);
    cb(this.syncState);
    return () => this.stateListeners.delete(cb);
  }

  /** Run one sync pass with visible feedback. Never fails silently. Returns
   * whether it converged. */
  private async runSync(): Promise<boolean> {
    if (!this.controller) return false;
    const peerUrl = normalizePeerUrl(this.settings.peerUrl); // bare host → wss://
    if (!peerUrl) {
      new Notice('asp: set a Peer URL first');
      this.log.append('sync: no Peer URL set — nothing to connect to', 'error');
      return false;
    }
    try {
      await this.controller.syncOnce({
        peerUrl,
        authKey: this.settings.authKey || undefined,
      });
      new Notice('asp: synced');
      return true;
    } catch (e) {
      // The controller already logged the underlying error.
      new Notice(`asp sync failed: ${String(e)}`);
      return false;
    }
  }

  /** First-time connect (the stage-1 button). On success, unlock the sync
   * controls so the next settings render shows them. */
  async connect(): Promise<boolean> {
    this.log.append('connect: attempting first sync…');
    const ok = await this.runSync();
    if (ok && !this.settings.connectedOnce) {
      this.settings.connectedOnce = true;
      this.settings.enabled = true;
      await this.saveData(this.settings);
      this.log.append('connect: enrolled ✓ — sync controls unlocked');
    }
    return ok;
  }

  async syncNow(): Promise<void> {
    await this.runSync();
  }

  private async readIgnore(host: ObsidianHost): Promise<string> {
    const b = await host.read('.aspignore');
    return b ? new TextDecoder().decode(b) : '';
  }

  private scheduleSync(): void {
    if (!this.settings.enabled || !this.settings.peerUrl) return;
    clearTimeout(this.debounce);
    this.debounce = setTimeout(() => void this.syncNow(), 600);
  }

  deviceKey(): string {
    return this.sdk?.nodeSsh() ?? '';
  }

  private renderStatus(s: string): void {
    if (this.statusEl) this.statusEl.setText?.(`asp: ${s}`) ?? (this.statusEl.textContent = `asp: ${s}`);
  }
}

const CARD_STYLE: Partial<CSSStyleDeclaration> = {
  border: '1px solid var(--background-modifier-border)',
  borderRadius: '8px',
  background: 'var(--background-secondary)',
  padding: '10px 12px',
  margin: '12px 0 0',
};

/** Map a sync state to the top banner's label + dot colour. */
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
  // Subscriptions live only while the tab is open; torn down on hide/re-render.
  private unsubLog?: () => void;
  private unsubState?: () => void;

  constructor(
    app: AspPlugin['app'],
    private plugin: AspPlugin,
  ) {
    super(app, plugin);
  }

  hide(): void {
    this.unsubLog?.();
    this.unsubState?.();
    this.unsubLog = undefined;
    this.unsubState = undefined;
  }

  display(): void {
    this.hide();
    const root = this.containerEl;
    root.empty?.();
    while (root.firstChild) root.removeChild(root.firstChild);

    // Live status at the very top: Not connected / Syncing… / In sync.
    this.renderStatusBanner(root);

    const connected = this.plugin.settings.connectedOnce;

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
      this.renderDeviceKeyCard(root);
    }

    // Log — both stages, inside a card.
    this.renderLogCard(root);
  }

  /** A titled, bordered card; returns the body element to fill. */
  private card(root: HTMLElement, title: string): HTMLElement {
    const wrap = document.createElement('div');
    Object.assign(wrap.style, CARD_STYLE);
    const head = document.createElement('div');
    head.textContent = title;
    Object.assign(head.style, {
      fontSize: '11px',
      fontWeight: '600',
      letterSpacing: '0.06em',
      textTransform: 'uppercase',
      color: 'var(--text-muted)',
      marginBottom: '8px',
    } as Partial<CSSStyleDeclaration>);
    const body = document.createElement('div');
    wrap.appendChild(head);
    wrap.appendChild(body);
    root.appendChild(wrap);
    return body;
  }

  private addButton(parent: HTMLElement, text: string, onClick: () => void): void {
    const btn = document.createElement('button');
    btn.textContent = text;
    btn.addEventListener('click', onClick);
    parent.appendChild(btn);
  }

  /** Live connection status, pinned at the top of the tab. */
  private renderStatusBanner(root: HTMLElement): void {
    const banner = document.createElement('div');
    Object.assign(banner.style, {
      display: 'flex',
      alignItems: 'center',
      gap: '8px',
      padding: '8px 12px',
      margin: '4px 0',
      border: '1px solid var(--background-modifier-border)',
      borderRadius: '8px',
      background: 'var(--background-secondary)',
      fontWeight: '600',
    } as Partial<CSSStyleDeclaration>);
    const dot = document.createElement('span');
    Object.assign(dot.style, {
      width: '10px',
      height: '10px',
      borderRadius: '50%',
      flex: '0 0 auto',
    } as Partial<CSSStyleDeclaration>);
    const label = document.createElement('span');
    banner.appendChild(dot);
    banner.appendChild(label);
    root.appendChild(banner);

    // Fires immediately with the current state, then on every change.
    this.unsubState = this.plugin.onSyncState((s) => {
      const info = statusInfo(s);
      dot.style.background = info.color;
      label.textContent = info.label;
    });
  }

  /** Device key in a card: a wrapping, selectable monospace box (so the long
   * ssh-ed25519 string can't overflow on mobile) + a Copy button. */
  private renderDeviceKeyCard(root: HTMLElement): void {
    const body = this.card(root, 'Device key');
    const box = document.createElement('div');
    box.textContent = this.plugin.deviceKey() || '(generated on first load)';
    Object.assign(box.style, {
      fontFamily: 'var(--font-monospace, monospace)',
      fontSize: '12px',
      lineHeight: '1.4',
      wordBreak: 'break-all',
      overflowWrap: 'anywhere',
      whiteSpace: 'pre-wrap',
      userSelect: 'all',
    } as Partial<CSSStyleDeclaration>);
    body.appendChild(box);
    const actions = document.createElement('div');
    actions.style.marginTop = '8px';
    body.appendChild(actions);
    this.addButton(actions, 'Copy', () => {
      void navigator.clipboard?.writeText(this.plugin.deviceKey());
      new Notice('asp: device key copied');
    });
  }

  /** Activity log in a card: Copy-all / Clear + a scrolling pre. The only
   * window into the plugin on mobile (no dev console), visible before connect. */
  private renderLogCard(root: HTMLElement): void {
    const body = this.card(root, 'Log');
    const actions = document.createElement('div');
    Object.assign(actions.style, {
      display: 'flex',
      gap: '8px',
      marginBottom: '8px',
    } as Partial<CSSStyleDeclaration>);
    body.appendChild(actions);

    const pre = document.createElement('pre');
    Object.assign(pre.style, {
      maxHeight: '220px',
      overflow: 'auto',
      whiteSpace: 'pre-wrap',
      wordBreak: 'break-word',
      fontFamily: 'var(--font-monospace, monospace)',
      fontSize: '11px',
      lineHeight: '1.45',
      padding: '8px',
      margin: '0',
      border: '1px solid var(--background-modifier-border)',
      borderRadius: '6px',
      background: 'var(--background-primary)',
    } as Partial<CSSStyleDeclaration>);
    body.appendChild(pre);

    const fmt = (e: LogEntry) => `[${e.ts}] ${e.level === 'error' ? 'ERR ' : ''}${e.msg}`;
    const render = () => {
      const entries = this.plugin.log.snapshot();
      pre.textContent = entries.length ? entries.map(fmt).join('\n') : '(no activity yet)';
      pre.scrollTop = pre.scrollHeight;
    };

    this.addButton(actions, 'Copy all', () => {
      void navigator.clipboard?.writeText(this.plugin.log.toText());
      new Notice('asp: log copied');
    });
    this.addButton(actions, 'Clear', () => {
      this.plugin.log.clear();
      render();
    });

    render();
    this.unsubLog = this.plugin.log.subscribe(() => render());
  }
}
