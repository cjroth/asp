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
}

const DEFAULTS: AspSettings = { peerUrl: '', authKey: '', seedHex: '', enabled: false };

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

    // Instantiate the wasm engine from the inlined bytes before any engine use
    // (the web target loads asynchronously). Idempotent.
    await initAsp(wasmBytes());

    // The one engine, in wasm — a thin node. Empty vault id adopts the peer's.
    this.sdk = new SdkVault(hexToBytes(this.settings.seedHex), '');
    const host = new ObsidianHost(this.app.vault.adapter);
    this.bridge = new Bridge(this.sdk, host, new PathFilter(await this.readIgnore(host)));
    this.controller = new SyncController(this.sdk, this.bridge);

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

    if (this.settings.enabled && this.settings.peerUrl) void this.syncNow();
  }

  onunload(): void {
    this.sdk?.free();
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

  async syncNow(): Promise<void> {
    if (!this.controller || !this.settings.peerUrl) return;
    try {
      await this.controller.syncOnce({
        peerUrl: this.settings.peerUrl,
        authKey: this.settings.authKey || undefined,
      });
    } catch (e) {
      new Notice(`asp sync failed: ${String(e)}`);
    }
  }

  deviceKey(): string {
    return this.sdk?.nodeSsh() ?? '';
  }

  private renderStatus(s: string): void {
    if (this.statusEl) this.statusEl.setText?.(`asp: ${s}`) ?? (this.statusEl.textContent = `asp: ${s}`);
  }
}

class AspSettingTab extends PluginSettingTab {
  constructor(
    app: AspPlugin['app'],
    private plugin: AspPlugin,
  ) {
    super(app, plugin);
  }

  display(): void {
    const { containerEl } = this;
    containerEl.empty?.();

    new Setting(containerEl)
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

    new Setting(containerEl)
      .setName('Auth key')
      .setDesc('The AUTH_KEY enrollment secret for first connect (optional once enrolled).')
      .addText((t) =>
        t.setValue(this.plugin.settings.authKey).onChange(async (v) => {
          this.plugin.settings.authKey = v.trim();
          await this.plugin.saveData(this.plugin.settings);
        }),
      );

    new Setting(containerEl)
      .setName('Sync enabled')
      .addToggle((t) =>
        t.setValue(this.plugin.settings.enabled).onChange(async (v) => {
          this.plugin.settings.enabled = v;
          await this.plugin.saveData(this.plugin.settings);
          if (v) void this.plugin.syncNow();
        }),
      );

    new Setting(containerEl)
      .setName('Device key')
      .setDesc(this.plugin.deviceKey() || '(generated on first load)')
      .addButton((b) =>
        b.setButtonText('Copy').onClick(() => void navigator.clipboard?.writeText(this.plugin.deviceKey())),
      );

    new Setting(containerEl).setName('Sync now').addButton((b) =>
      b.setButtonText('Sync').onClick(() => void this.plugin.syncNow()),
    );
  }
}
