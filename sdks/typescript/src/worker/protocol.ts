// Message protocol for the engine Web Worker. The wasm engine + WebSocket
// transport run inside a Worker so the heavy synchronous engine calls (the
// fold, merge, content hashing, the session feed loop) never block the host's
// renderer thread. The main thread keeps the Obsidian-facing surface: a
// `WorkerVault` that proxies the `Vault` methods the plugin uses.
//
// Two message families cross the channel:
//   • command (main → worker) — a Vault method call, correlated by id.
//   • reply   (worker → main) — the result of a command, by the same id.
// Everything is structured-clone-safe (strings, numbers, Uint8Arrays) so it
// survives `postMessage` unchanged.

/** The `init` payload — everything the worker needs to stand up the engine.
 * The wasm bytes are shipped in so the worker never fetches (mobile-WebView
 * safe, the same constraint that drives the plugin's inlining). */
export interface InitPayload {
  /** 32-byte device identity seed. */
  seed: Uint8Array;
  /** Empty adopts the peer's vault on first connect. */
  vaultId: string;
  /** Inlined asp-core wasm — instantiated inside the worker (ignored by the
   * nodejs glue used under tests). */
  wasmBytes: Uint8Array;
}

/** Identity returned by `init` — constant afterwards, so the main-side proxy
 * caches it for synchronous reads (the settings UI needs the key without a
 * round-trip). */
export interface Identity {
  nodeSsh: string;
  nodeId: string;
  vaultId: string;
}

/** A Vault-method command. `id` correlates the reply. */
export type Command =
  | { kind: 'cmd'; id: number; op: 'init'; payload: InitPayload }
  | { kind: 'cmd'; id: number; op: 'writeFile'; path: string; bytes: Uint8Array }
  | { kind: 'cmd'; id: number; op: 'deleteFile'; path: string }
  | { kind: 'cmd'; id: number; op: 'renameFile'; from: string; to: string }
  | { kind: 'cmd'; id: number; op: 'commitFiles'; files: Record<string, Uint8Array> }
  | { kind: 'cmd'; id: number; op: 'writeFiles'; files: Record<string, Uint8Array> }
  | { kind: 'cmd'; id: number; op: 'files' }
  | { kind: 'cmd'; id: number; op: 'sync'; url: string; authKey?: string }
  | { kind: 'cmd'; id: number; op: 'free' };

/** The reply to a `Command`, by the same `id`. */
export interface Reply {
  kind: 'reply';
  id: number;
  ok: boolean;
  /** `init` → Identity; `files` → Record<path,bytes>; `sync` → integrated count. */
  value?: Identity | Record<string, Uint8Array> | number;
  /** Present when `!ok`. */
  error?: string;
}

export type ToWorker = Command;
export type FromWorker = Reply;
