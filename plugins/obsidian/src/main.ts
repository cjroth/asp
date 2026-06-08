// Context for Obsidian — the v1 reference thin-client surface. A thin node on the
// @asp/sdk: it holds a complete local working copy, authors rows offline, and
// converges on reconnect by making an OUTBOUND connection to a full node (an `asp
// watch --listen` process or Context Desktop). It never runs the multi-tip merge
// and never listens/relays. NO protocol/merge/fold/auth logic lives here — any
// behavioral difference from the `asp` CLI is a bug; this file is host glue only
// (vault I/O, the event→push path, settings, a status bar).

import { Notice, Plugin, PluginSettingTab, Setting, type EventRef } from 'obsidian';
import { initAsp, Vault as SdkVault } from '../../../sdks/typescript/src/index.ts';
import { Bridge } from './bridge.ts';
import { type LogEntry, LogBuffer } from './log-buffer.ts';
import { ObsidianHost } from './obsidian-host.ts';
import { PathFilter } from './path-filter.ts';
import { SyncController } from './sync-controller.ts';

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
    this.controller.subscribe((s) => this.renderStatus(s));
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

  /** Run one sync pass with visible feedback. Never fails silently. Returns
   * whether it converged. */
  private async runSync(): Promise<boolean> {
    if (!this.controller) return false;
    if (!this.settings.peerUrl) {
      new Notice('asp: set a Peer URL first');
      this.log.append('sync: no Peer URL set — nothing to connect to', 'error');
      return false;
    }
    try {
      await this.controller.syncOnce({
        peerUrl: this.settings.peerUrl,
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

class AspSettingTab extends PluginSettingTab {
  /** Live-log subscription, torn down on each re-render and when the tab hides. */
  private unsub?: () => void;

  constructor(
    app: AspPlugin['app'],
    private plugin: AspPlugin,
  ) {
    super(app, plugin);
  }

  hide(): void {
    this.unsub?.();
    this.unsub = undefined;
  }

  display(): void {
    this.hide();
    const root = this.containerEl;
    root.empty?.();
    while (root.firstChild) root.removeChild(root.firstChild);

    const connected = this.plugin.settings.connectedOnce;

    // ---- Connection details (both stages) -------------------------------
    new Setting(root)
      .setName('Peer URL')
      .setDesc('A full node in listen mode, e.g. wss://hub:9000 (or ws:// behind a TLS proxy).')
      .addText((t) =>
        t
          .setPlaceholder('wss://host:9000')
          .setValue(this.plugin.settings.peerUrl)
          .onChange(async (v) => {
            this.plugin.settings.peerUrl = v.trim();
            await this.plugin.saveData(this.plugin.settings);
          }),
      );

    new Setting(root)
      .setName('Auth key')
      .setDesc('The AUTH_KEY enrollment secret for first connect (optional once enrolled).')
      .addText((t) =>
        t.setValue(this.plugin.settings.authKey).onChange(async (v) => {
          this.plugin.settings.authKey = v.trim();
          await this.plugin.saveData(this.plugin.settings);
        }),
      );

    if (!connected) {
      // ---- Stage 1: not yet connected — just connect. The sync controls
      // (enable toggle, device key, sync now) appear only after a first
      // successful connect, so a fresh setup isn't cluttered with options
      // that don't apply yet.
      new Setting(root)
        .setName('Connect')
        .setDesc('Connect to the peer and sync for the first time. Sync options unlock once connected.')
        .addButton((b) =>
          b.setButtonText('Connect').onClick(async () => {
            b.setButtonText('Connecting…');
            const ok = await this.plugin.connect();
            b.setButtonText('Connect');
            if (ok) this.display(); // reveal stage 2
          }),
        );
    } else {
      // ---- Stage 2: connected — full sync controls.
      new Setting(root).setName('Sync enabled').addToggle((t) =>
        t.setValue(this.plugin.settings.enabled).onChange(async (v) => {
          this.plugin.settings.enabled = v;
          await this.plugin.saveData(this.plugin.settings);
          if (v) void this.plugin.syncNow();
        }),
      );

      new Setting(root)
        .setName('Sync now')
        .addButton((b) => b.setButtonText('Sync').onClick(() => void this.plugin.syncNow()));

      this.renderDeviceKey(root);
    }

    // ---- Log (both stages) ----------------------------------------------
    this.renderLog(root);
  }

  /** The device key rendered in a wrapping, selectable monospace box so the
   * long ssh-ed25519 string can't overflow the card and shift the settings
   * screen on mobile (it used to be a single unbroken `setDesc` line). */
  private renderDeviceKey(root: HTMLElement): void {
    new Setting(root)
      .setName('Device key')
      .setDesc('Authorize this device on the peer with this key.')
      .addButton((b) =>
        b.setButtonText('Copy').onClick(() => {
          void navigator.clipboard?.writeText(this.plugin.deviceKey());
          new Notice('asp: device key copied');
        }),
      );

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
      padding: '6px 8px',
      margin: '0 0 12px',
      border: '1px solid var(--background-modifier-border)',
      borderRadius: '6px',
      background: 'var(--background-secondary)',
    } as Partial<CSSStyleDeclaration>);
    root.appendChild(box);
  }

  /** Always-visible, copyable activity log — the only window into what the
   * plugin is doing on mobile (no dev console there), and useful for debugging
   * before a connection is ever made. */
  private renderLog(root: HTMLElement): void {
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
      margin: '4px 0 0',
      border: '1px solid var(--background-modifier-border)',
      borderRadius: '6px',
      background: 'var(--background-secondary)',
    } as Partial<CSSStyleDeclaration>);

    const fmt = (e: LogEntry) => `[${e.ts}] ${e.level === 'error' ? 'ERR ' : ''}${e.msg}`;
    const render = () => {
      const entries = this.plugin.log.snapshot();
      pre.textContent = entries.length ? entries.map(fmt).join('\n') : '(no activity yet)';
      pre.scrollTop = pre.scrollHeight;
    };

    new Setting(root)
      .setName('Log')
      .setDesc('Recent activity. Copy this when reporting an issue.')
      .addButton((b) =>
        b.setButtonText('Copy all').onClick(() => {
          void navigator.clipboard?.writeText(this.plugin.log.toText());
          new Notice('asp: log copied');
        }),
      )
      .addButton((b) =>
        b.setButtonText('Clear').onClick(() => {
          this.plugin.log.clear();
          render();
        }),
      );

    root.appendChild(pre);
    render();
    // Stream new lines into the open viewer; torn down in hide()/next display().
    this.unsub = this.plugin.log.subscribe(() => render());
  }
}
