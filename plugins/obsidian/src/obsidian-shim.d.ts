// Minimal ambient declarations for the subset of the Obsidian API the plugin
// uses, so the plugin type-checks/bundles in the monorepo without the full
// `obsidian` package (Obsidian provides the real module at runtime; esbuild
// externalizes it).
declare module 'obsidian' {
  export interface DataAdapter {
    read(path: string): Promise<string>;
    readBinary(path: string): Promise<ArrayBuffer>;
    write(path: string, data: string): Promise<void>;
    writeBinary(path: string, data: ArrayBuffer): Promise<void>;
    exists(path: string): Promise<boolean>;
    remove(path: string): Promise<void>;
    mkdir(path: string): Promise<void>;
    rmdir(path: string, recursive: boolean): Promise<void>;
    list(path: string): Promise<{ files: string[]; folders: string[] }>;
  }
  export interface Vault {
    adapter: DataAdapter;
    on(name: string, cb: (...args: unknown[]) => unknown): EventRef;
    getFiles(): { path: string }[];
  }
  export interface EventRef {}
  export interface App {
    vault: Vault;
  }
  export class Plugin {
    app: App;
    constructor(app: App, manifest: unknown);
    addStatusBarItem(): HTMLElement;
    addCommand(cmd: { id: string; name: string; callback: () => void }): void;
    addSettingTab(tab: PluginSettingTab): void;
    registerEvent(ref: EventRef): void;
    loadData(): Promise<unknown>;
    saveData(data: unknown): Promise<void>;
    onload(): void | Promise<void>;
    onunload(): void;
  }
  export class PluginSettingTab {
    app: App;
    containerEl: HTMLElement;
    constructor(app: App, plugin: Plugin);
    display(): void;
  }
  export class Setting {
    constructor(containerEl: HTMLElement);
    setName(name: string): this;
    setDesc(desc: string): this;
    addText(cb: (t: TextComponent) => void): this;
    addToggle(cb: (t: ToggleComponent) => void): this;
    addButton(cb: (b: ButtonComponent) => void): this;
  }
  export interface TextComponent {
    setValue(v: string): this;
    setPlaceholder(v: string): this;
    onChange(cb: (v: string) => void): this;
  }
  export interface ToggleComponent {
    setValue(v: boolean): this;
    onChange(cb: (v: boolean) => void): this;
  }
  export interface ButtonComponent {
    setButtonText(v: string): this;
    onClick(cb: () => void): this;
  }
  export class Notice {
    constructor(message: string);
  }
}
