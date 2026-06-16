/* ====================================================================
   network-proxy.ts · main-thread proxy for the engine Web Worker
   --------------------------------------------------------------------
   The whole multi-engine mesh sim (every WasmEngine, the gossip routing,
   the live ws:// peers) runs inside ONE Web Worker — off the renderer
   thread, so a big synchronous catch-up (integrate over thousands of
   rows) never freezes the UI. This proxy presents the SAME method surface
   the in-page `createNetwork` api exposes, so the React components call it
   unchanged; under the hood each call is a postMessage.

   Three message families cross the channel (mirrors the SDK's engine-host
   protocol, but for the whole network rather than a single Vault):
     • cmd   (main → worker) — an api method call, correlated by id.
     • reply (worker → main) — the result of a request-style cmd.
     • event (worker → main) — pushed snapshots / packets / toasts.

   Snapshots are PUSHED on every change (the worker coalesces a burst into
   one post) and cached here, so `snapshot()` stays a synchronous read the
   render path can call freely. The main thread coalesces the resulting
   re-renders with requestAnimationFrame (see App's scheduleRender).
   ==================================================================== */

export interface ProxyCallbacks {
  /** A new snapshot arrived (or a pulled one) — schedule a render. */
  onChange: () => void;
  /** A packet was sent — feed the main-thread map animation. */
  onPacket: (pk: any) => void;
  onToast: (msg: string, kind: string) => void;
}

type Cmd = { kind: 'cmd'; id: number; op: string; args: any[] };
type FromWorker =
  | { kind: 'reply'; id: number; ok: boolean; value?: any; error?: string }
  | { kind: 'snapshot'; snap: any }
  | { kind: 'packet'; pk: any }
  | { kind: 'toast'; msg: string; tone: string };

const EMPTY_SNAP = { nodes: [] as any[], edges: [] as any[] };

export class NetworkProxy {
  private nextId = 1;
  private readonly pending = new Map<number, { resolve: (v: any) => void; reject: (e: Error) => void }>();
  private snap: any = EMPTY_SNAP;
  private cb: ProxyCallbacks = { onChange() {}, onPacket() {}, onToast() {} };

  constructor(private readonly worker: Worker) {
    worker.onmessage = (ev: MessageEvent) => this.onMessage(ev.data as FromWorker);
  }

  /** App wires its React-bound callbacks here once it mounts (the proxy is
   * constructed in main.tsx, before those refs exist). */
  setCallbacks(cb: ProxyCallbacks) {
    this.cb = cb;
  }

  private onMessage(m: FromWorker) {
    switch (m.kind) {
      case 'reply': {
        const p = this.pending.get(m.id);
        if (!p) return;
        this.pending.delete(m.id);
        if (m.ok) p.resolve(m.value);
        else p.reject(new Error(m.error ?? 'network worker error'));
        return;
      }
      case 'snapshot':
        this.snap = m.snap;
        this.cb.onChange();
        return;
      case 'packet':
        this.cb.onPacket(m.pk);
        return;
      case 'toast':
        this.cb.onToast(m.msg, m.tone);
        return;
    }
  }

  /** Fire-and-forget mutation (id 0 — the worker won't reply). */
  private send(op: string, ...args: any[]): void {
    this.worker.postMessage({ kind: 'cmd', id: 0, op, args } as Cmd);
  }

  /** Request/reply — resolves with the worker's return value. */
  private request<T = any>(op: string, ...args: any[]): Promise<T> {
    const id = this.nextId++;
    return new Promise<T>((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.worker.postMessage({ kind: 'cmd', id, op, args } as Cmd);
    });
  }

  // ---- lifecycle / request-style -----------------------------------------
  /** Stand up the engine in the worker (instantiate the wasm from the inlined
   * bytes, build the mesh sim). Must be awaited before any other call. */
  init(wasmBytes: Uint8Array): Promise<void> {
    return this.request('init', wasmBytes);
  }
  addNode(opts: any): Promise<string> {
    return this.request('addNode', opts);
  }
  connectPeer(nodeId: string, ticket: string, authKey?: string, relayUrl?: string): Promise<boolean> {
    return this.request('connectPeer', nodeId, ticket, authKey, relayUrl);
  }
  /** Serialize the whole mesh to the final OPFS JSON string, built INSIDE the
   * worker (it holds every row) so the main thread never stringifies the
   * multi-MB document. `tweaks` is folded in so the result is ready to write. */
  serialize(tweaks: unknown): Promise<string> {
    return this.request('serialize', tweaks);
  }
  /** Restore from the raw OPFS JSON text — the worker parses it once (off the
   * main thread). Resolves with the persisted `tweaks` (or null). */
  restore(rawJson: string): Promise<any> {
    return this.request('restore', rawJson);
  }
  reset(): Promise<void> {
    return this.request('reset');
  }

  // ---- synchronous read (last pushed snapshot) ---------------------------
  snapshot() {
    return this.snap;
  }

  /** The node's change history (timeline + diff). Async — computed in the
   * worker on demand (it scans all rows + blobs), NOT shipped in every snapshot
   * (that would undo the snapshot diet). */
  history(nodeId: string): Promise<any[]> {
    return this.request('history', nodeId);
  }

  // ---- fire-and-forget mutations -----------------------------------------
  setConfig(patch: any) { this.send('setConfig', patch); }
  removeNode(id: string) { this.send('removeNode', id); }
  renameNode(id: string, name: string) { this.send('renameNode', id, name); }
  setOnline(id: string, online: boolean) { this.send('setOnline', id, online); }
  disconnectPeer(id: string) { this.send('disconnectPeer', id); }
  clearFresh() { this.send('clearFresh'); }
  openFile(nodeId: string, fileId: string) { this.send('openFile', nodeId, fileId); }
  stageEdit(nodeId: string, fileId: string, content: string) { this.send('stageEdit', nodeId, fileId, content); }
  commitNow(nodeId: string, fileId: string) { this.send('commitNow', nodeId, fileId); }
  createFile(nodeId: string, dir: string, name: string) { this.send('createFile', nodeId, dir, name); }
  createFolder(nodeId: string, dir: string, name: string) { this.send('createFolder', nodeId, dir, name); }
  renameFile(nodeId: string, fileId: string, newPath: string) { this.send('renameFile', nodeId, fileId, newPath); }
  moveFile(nodeId: string, fileId: string, newDir: string) { this.send('moveFile', nodeId, fileId, newDir); }
  renameFolder(nodeId: string, oldPath: string, newPath: string) { this.send('renameFolder', nodeId, oldPath, newPath); }
  deleteFolder(nodeId: string, folderPath: string) { this.send('deleteFolder', nodeId, folderPath); }
  deleteFile(nodeId: string, fileId: string) { this.send('deleteFile', nodeId, fileId); }
}
