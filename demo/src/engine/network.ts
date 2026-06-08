/* ====================================================================
   network.ts · the ASP demo "network"
   --------------------------------------------------------------------
   Each node is the REAL asp-core engine compiled to wasm (`WasmEngine`
   from @asp/sdk). The protocol — log rows, Merkle ids, the deterministic
   fold, 3-way merge, version vectors, catch-up — is real and runs in the
   browser. Only the NETWORK is simulated (latency, packet animation,
   offline link, gossip routing, commit debounce), wrapping real WireRow
   payloads moved between in-tab engines via rows_after() -> integrate().

   Mirrors the snapshot shape of the original design prototype so the UI
   components stay presentational.
   ==================================================================== */
import { WasmEngine } from '../../../sdks/typescript/src/index.ts';

const enc = new TextEncoder();
const dec = new TextDecoder();

// ---- small helpers --------------------------------------------------
function nowClock(): string {
  const d = new Date();
  const p = (x: number, n = 2) => String(x).padStart(n, '0');
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}.${p(d.getMilliseconds(), 3)}`;
}
function randomSeed(): Uint8Array {
  const b = new Uint8Array(32);
  crypto.getRandomValues(b);
  return b;
}
function shortHex(n = 4): string {
  const b = new Uint8Array(n);
  crypto.getRandomValues(b);
  return [...b].map((x) => x.toString(16).padStart(2, '0')).join('').slice(0, n);
}
function bytesToHex(b: Uint8Array): string {
  return [...b].map((x) => x.toString(16).padStart(2, '0')).join('');
}
function hexToBytes(h: string): Uint8Array {
  const out = new Uint8Array(h.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = Number.parseInt(h.slice(i * 2, i * 2 + 2), 16);
  return out;
}
function hostOf(url: string): string {
  try { return new URL(url).host; } catch { return url.replace(/^wss?:\/\//, ''); }
}
function baseOf(p: string): string {
  const i = p.lastIndexOf('/');
  return i < 0 ? p : p.slice(i + 1);
}
function dirOf(p: string): string {
  const i = p.lastIndexOf('/');
  return i < 0 ? '' : p.slice(0, i);
}

const NODE_NAMES = ['laptop', 'desktop', 'studio', 'phone', 'tablet', 'server', 'macbook', 'workstation'];
const NODE_COLORS = ['#5fb6d4', '#74cf9e', '#c9a6ee', '#e6c06a', '#e08a7a', '#7ab8e0'];

const SEED: { path: string; body: string }[] = [
  { path: 'README.md', body: '# Vault\n\nShared context for agents + notes.\nSynced live by ASP — no commit, no push.\n' },
  { path: 'notes/todo.md', body: '# Todo\n\n- [ ] draft the sync spec\n- [ ] wire up the fold\n- [ ] test offline catch-up\n' },
  { path: 'notes/ideas.md', body: '# Ideas\n\n- content-addressed blobs\n- lamport ordering\n- rename keeps file_id\n' },
  { path: 'journal/2026-06-07.md', body: '## 2026-06-07\n\nStarted the agent vault. It just works across devices.\n' },
  { path: 'src/fold.rs', body: '// deterministic fold\nfn fold(log: &Log) -> State {\n    log.sorted().iter().fold(State::new(), apply)\n}\n' },
];

// ---- token + line types (event log) --------------------------------
export interface Tok { t: string; c: string }
export interface LogLine { id: number; ts: string; parts: Tok[]; fresh: boolean }
export interface Packet { id: number; fromId: string; toId: string; kind: string; started: number; dur: number }

interface FileView {
  file_id: string;
  path: string;
  content: string;
  merge_class: string;
  result_hash: string | null;
  deleted: boolean;
  collided: boolean;
}

interface Node {
  id: string;            // full node_id() — unique key + edge endpoint
  localId: string;       // stable demo id (survives reload; node_id is per-instance)
  seedHex: string;       // the ed25519 connection seed (persisted for restore + ws auth)
  name: string;
  color: string;
  online: boolean;
  eng: InstanceType<typeof WasmEngine>;
  openFileId: string | null;
  staged: Record<string, string>;
  folders: Set<string>;  // local-only folders (mkdir before a file lands)
  lines: LogLine[];
  lineSeq: number;
  createdRemote: string | null;
  externalUrl?: string;  // a real `asp watch --listen` peer this node bridges to
  authKey?: string;
  syncing?: boolean;     // initial handshake/catch-up in flight
  liveWs?: WebSocket;    // a persistent (watch) connection to the external peer
  live?: boolean;        // the live connection is open + authed
  liveAuthed?: boolean;
}

export interface NetworkOpts {
  latencyMs?: number;
  debounceMs?: number;
  onChange?: () => void;
  onPacket?: (pk: Packet) => void;
  onToast?: (msg: string, kind: string) => void;
}

function nowPerf(): number {
  return typeof performance !== 'undefined' ? performance.now() : Date.now();
}

export function createNetwork(opts: NetworkOpts) {
  const O = { latencyMs: 520, debounceMs: 850, onChange() {}, onPacket() {}, onToast() {}, ...opts };
  const cfg = { latencyMs: O.latencyMs, debounceMs: O.debounceMs };

  const nodes: Node[] = [];
  const edges: { a: string; b: string }[] = [];
  let nodeSeq = 0;
  let localSeq = 0;
  let packetSeq = 0;
  const debounceTimers: Record<string, ReturnType<typeof setTimeout>> = {};

  const emit = () => O.onChange();
  const findNode = (id: string) => nodes.find((n) => n.id === id);
  const peersOf = (id: string): string[] => {
    const out: string[] = [];
    for (const e of edges) {
      if (e.a === id) out.push(e.b);
      else if (e.b === id) out.push(e.a);
    }
    return out;
  };
  const edgeExists = (a: string, b: string) => edges.some((e) => (e.a === a && e.b === b) || (e.a === b && e.b === a));

  function logLine(node: Node, parts: Tok[]) {
    node.lines.push({ id: node.lineSeq++, ts: nowClock(), parts, fresh: true });
    if (node.lines.length > 400) node.lines.splice(0, node.lines.length - 400);
  }

  const vvOf = (node: Node): Record<string, number> => JSON.parse(node.eng.version_vector());

  // The materialized vault as the UI's file map (keyed by file_id).
  function filesView(node: Node): Record<string, FileView> {
    const detail = JSON.parse(node.eng.files_detail_json()) as {
      file_id: string; path: string; result_hash: string | null; merge_class: string; deleted: boolean; conflict: boolean;
    }[];
    const raw = JSON.parse(node.eng.files_json()) as Record<string, number[]>;
    const byPath: Record<string, string> = {};
    for (const [p, arr] of Object.entries(raw)) byPath[p] = dec.decode(Uint8Array.from(arr));
    const out: Record<string, FileView> = {};
    for (const f of detail) {
      out[f.file_id] = {
        file_id: f.file_id,
        path: f.path,
        content: byPath[f.path] ?? '',
        merge_class: f.merge_class,
        result_hash: f.result_hash,
        deleted: f.deleted,
        collided: f.conflict,
      };
    }
    return out;
  }

  // The rows `node` authored since `beforeVv` (i.e. just now) — used for the
  // commit log line's lamport/seq/id detail.
  function authoredSince(node: Node, beforeVv: Record<string, number>) {
    return JSON.parse(node.eng.rows_after(JSON.stringify(beforeVv))) as { row: any; blobs: any[] }[];
  }

  // ---- the simulated transport: one real catch-up over one edge ------
  function syncEdge(from: Node, to: Node, kind: string, exceptId?: string) {
    if (!from.online || !to.online || !edgeExists(from.id, to.id)) return;
    const deltaJson = from.eng.rows_after(to.eng.version_vector());
    const rows = JSON.parse(deltaJson) as any[];
    if (rows.length === 0) return;

    const pid = ++packetSeq;
    const bytes = JSON.stringify(rows).length;
    O.onPacket({ id: pid, fromId: from.id, toId: to.id, kind, started: nowPerf(), dur: cfg.latencyMs });
    if (kind === 'row') {
      logLine(from, [
        { t: 'DEBUG', c: 'lvl' }, { t: ' push      ', c: 'tag' },
        { t: `→ ${to.name} `, c: 'dim' },
        { t: `${rows.length} row${rows.length > 1 ? 's' : ''}`, c: 'k push' },
        { t: ` (${bytes}B)`, c: 'dim' },
      ]);
    }

    setTimeout(() => {
      if (!from.online || !to.online || !edgeExists(from.id, to.id)) return; // frame lost
      const n = to.eng.integrate(deltaJson);
      if (n > 0) {
        const top = rows[rows.length - 1]?.row;
        if (kind === 'catchup') {
          logLine(to, [
            { t: 'INFO', c: 'lvl' }, { t: ' catch-up  ', c: 'tag' },
            { t: `← ${from.name} `, c: 'dim' },
            { t: `+${n} rows`, c: 'k catchup' }, { t: ' folded → materialized', c: 'dim' },
          ]);
        } else {
          logLine(to, [
            { t: 'INFO', c: 'lvl' }, { t: ' integrate ', c: 'tag' },
            { t: `← ${from.name} `, c: 'dim' },
            { t: top ? top.kind : 'rows', c: `k ${top ? top.kind : ''}` },
            { t: ` ${top && top.path ? baseOf(top.path) : `+${n}`}`, c: 'hl' },
            { t: top ? ` id=${top.id} lamport=${top.lamport}` : '', c: 'dim' },
          ]);
        }
        // forward (gossip): re-run anti-entropy to our other peers.
        for (const pid2 of peersOf(to.id)) {
          if (pid2 === exceptId || pid2 === from.id) continue;
          const peer = findNode(pid2);
          if (peer) syncEdge(to, peer, 'row', from.id);
        }
        // bridge: forward in-page-originated rows to a live external peer too.
        pushLive(to, deltaJson);
      }
      emit();
    }, cfg.latencyMs);
  }

  function gossip(node: Node, kind = 'row') {
    for (const pid of peersOf(node.id)) {
      const peer = findNode(pid);
      if (peer) syncEdge(node, peer, kind);
    }
  }

  // Optimistic real-time push of new rows over a node's live ws:// connection
  // (the wire analogue of the native daemon pushing to connected peers).
  function pushLive(node: Node, rowsJson: string) {
    const ws = node.liveWs;
    if (!ws || !node.liveAuthed || ws.readyState !== 1) return;
    const rows = JSON.parse(rowsJson) as any[];
    if (rows.length === 0) return;
    try {
      ws.send(node.eng.push_frame(rowsJson) as any);
      logLine(node, [
        { t: 'DEBUG', c: 'lvl' }, { t: ' push      ', c: 'tag' },
        { t: `→ ${hostOf(node.externalUrl || '')} `, c: 'dim' },
        { t: `${rows.length} row${rows.length > 1 ? 's' : ''}`, c: 'k push' }, { t: ' live', c: 'dim' },
      ]);
    } catch { /* socket closing */ }
  }

  // ====================================================================
  // PUBLIC API
  // ====================================================================
  const api: any = {};

  api.setConfig = (patch: Partial<typeof cfg>) => Object.assign(cfg, patch);

  api.addNode = ({ name, remoteId, externalUrl, authKey }: { name?: string; remoteId?: string | null; externalUrl?: string; authKey?: string }) => {
    const idx = nodeSeq++;
    const remote = remoteId != null ? findNode(remoteId) : null;
    // genesis seeds its own vault; a clone (in-page or external) adopts the
    // peer's vault id — empty string adopts on first real handshake.
    const vaultId = remote ? remote.eng.vault_id() : externalUrl ? '' : `vault-${shortHex(4)}`;
    const seed = randomSeed();
    const eng = new WasmEngine(seed, vaultId);
    const node: Node = {
      id: eng.node_id(),
      localId: `n${localSeq++}`,
      seedHex: bytesToHex(seed),
      name: name || NODE_NAMES[idx % NODE_NAMES.length] + (idx >= NODE_NAMES.length ? `-${idx}` : ''),
      color: NODE_COLORS[idx % NODE_COLORS.length],
      online: true,
      eng,
      openFileId: null,
      staged: {},
      folders: new Set(),
      lines: [],
      lineSeq: 0,
      createdRemote: null,
    };
    nodes.push(node);

    logLine(node, [
      { t: 'INFO', c: 'lvl' }, { t: ' init      ', c: 'tag' },
      { t: `node ${node.name}`, c: 'hl' }, { t: ` site_id=${node.id.slice(0, 8)} ed25519`, c: 'dim' },
    ]);

    if (externalUrl) {
      // clone from a REAL `asp watch --listen` peer over ws:// — the genuine
      // Session handshake + version-vector catch-up (asp clone <url>).
      node.createdRemote = hostOf(externalUrl);
      api.connectPeer(node.id, externalUrl, authKey).then(() => {
        const first = Object.values(filesView(node)).find((f) => !f.deleted);
        if (first && !node.openFileId) { node.openFileId = first.file_id; emit(); }
      });
    } else if (!remote) {
      // genesis vault: author the seed files (real create rows)
      for (const s of SEED) node.eng.record_write(s.path, enc.encode(s.body));
      const view = filesView(node);
      const readme = Object.values(view).find((f) => f.path === 'README.md');
      node.openFileId = readme ? readme.file_id : Object.values(view)[0]?.file_id ?? null;
      logLine(node, [
        { t: 'INFO', c: 'lvl' }, { t: ' commit    ', c: 'tag' },
        { t: 'genesis', c: 'k create' }, { t: ` ${SEED.length} files materialized`, c: 'dim' },
      ]);
    } else {
      // clone: handshake (cosmetic) + real full catch-up via version vector
      node.createdRemote = remote.name;
      edges.push({ a: node.id, b: remote.id });
      logLine(node, [
        { t: 'INFO', c: 'lvl' }, { t: ' clone     ', c: 'tag' },
        { t: `dial ${remote.name}`, c: 'hl' }, { t: ' wss:// handshake…', c: 'dim' },
      ]);
      logLine(node, [
        { t: 'INFO', c: 'lvl' }, { t: ' handshake ', c: 'tag' },
        { t: 'ed25519 mutual-auth ok', c: 'k handshake' }, { t: ' · admitted (authorized_keys)', c: 'dim' },
      ]);
      logLine(remote, [
        { t: 'INFO', c: 'lvl' }, { t: ' peer      ', c: 'tag' },
        { t: `${node.name} connected`, c: 'k peer' }, { t: ' · key authorized · catch-up', c: 'dim' },
      ]);
      syncEdge(remote, node, 'catchup');
      node.openFileId = remote.openFileId;
    }
    emit();
    return node.id;
  };

  // ---- bridge a node to a REAL peer over ws:// (the genuine Session) --------
  // A PERSISTENT (watch) connection to an `asp watch --listen` node: the ed25519
  // handshake + version-vector catch-up via the engine's connect_start()/feed(),
  // then the socket STAYS OPEN. Incoming Push/Rows frames are fed → integrated →
  // gossiped through the in-page mesh (real-time receive); locally-authored rows
  // are sent as Rows frames (real-time send, see pushLive). The promise resolves
  // once the INITIAL catch-up converges (so addNode can open a file).
  api.connectPeer = (nodeId: string, url: string, authKey?: string): Promise<boolean> => {
    const node = findNode(nodeId);
    if (!node) return Promise.resolve(false);
    if (node.liveWs) { try { node.liveWs.close(); } catch {} node.liveWs = undefined; node.live = false; node.liveAuthed = false; }
    node.externalUrl = url;
    node.authKey = authKey || undefined;
    node.syncing = true;

    const timeoutMs = 15000;
    const fullUrl = authKey ? `${url}${url.includes('?') ? '&' : '?'}auth_key=${encodeURIComponent(authKey)}` : url;
    const beforeVv = vvOf(node);

    logLine(node, [
      { t: 'INFO', c: 'lvl' }, { t: ' watch     ', c: 'tag' },
      { t: `dial ${hostOf(url)}`, c: 'hl' }, { t: ' ws:// real peer · live · handshake…', c: 'dim' },
    ]);
    emit();

    return new Promise<boolean>((resolve) => {
      const logErr = (msg: string) => {
        logLine(node!, [{ t: 'WARN', c: 'lvl warn' }, { t: ' watch     ', c: 'tag' }, { t: hostOf(url), c: 'hl' }, { t: ` · ${msg}`, c: 'dim' }]);
        O.onToast(`${node!.name}: ${msg}`, 'warn');
      };
      let ws: WebSocket;
      try { ws = new WebSocket(fullUrl); } catch (e) { node!.syncing = false; logErr(`dial failed: ${String(e)}`); emit(); return resolve(false); }
      ws.binaryType = 'arraybuffer';
      node.liveWs = ws;
      let authedLogged = false;
      let settled = false;
      let convergeIdle: ReturnType<typeof setTimeout>;
      const hardStop = setTimeout(() => settle(false, 'handshake timed out'), timeoutMs);

      // Resolve once the INITIAL catch-up goes idle — but keep the socket OPEN.
      function settleConverged() {
        if (settled) return;
        settled = true;
        clearTimeout(hardStop);
        node!.syncing = false;
        const imported = (JSON.parse(node!.eng.rows_after(JSON.stringify(beforeVv))) as any[]).length;
        gossip(node!, 'catchup');
        logLine(node!, [
          { t: 'INFO', c: 'lvl' }, { t: ' live      ', c: 'tag' },
          { t: hostOf(url), c: 'k catchup' }, { t: ` · converged (+${imported} rows) · watching`, c: 'dim' },
        ]);
        emit();
        resolve(true);
      }
      function settle(ok: boolean, err?: string) {
        if (settled) return;
        settled = true;
        clearTimeout(hardStop); clearTimeout(convergeIdle);
        node!.syncing = false;
        try { ws.close(); } catch {}
        if (err) logErr(err);
        emit();
        resolve(ok);
      }
      const bumpConverge = () => { if (settled) return; clearTimeout(convergeIdle); convergeIdle = setTimeout(settleConverged, 900); };

      ws.onopen = () => {
        try { ws.send(node!.eng.connect_start() as any); } catch (e) { return settle(false, `handshake: ${String(e)}`); }
        bumpConverge();
      };
      ws.onmessage = (ev: MessageEvent) => {
        const frame = new Uint8Array(ev.data as ArrayBuffer);
        let r: any;
        try { r = JSON.parse(node!.eng.feed(frame)); } catch (e) { logErr(`feed: ${String(e)}`); try { ws.close(); } catch {} return; }
        for (const out of r.out) { try { ws.send(Uint8Array.from(out) as any); } catch {} }
        if (r.authed && !authedLogged) {
          authedLogged = true;
          node!.live = true; node!.liveAuthed = true;
          logLine(node!, [{ t: 'INFO', c: 'lvl' }, { t: ' handshake ', c: 'tag' }, { t: 'ed25519 mutual-auth ok', c: 'k handshake' }, { t: ' · admitted', c: 'dim' }]);
          emit();
        }
        if (r.integrated) {
          logLine(node!, [{ t: 'INFO', c: 'lvl' }, { t: ' catch-up  ', c: 'tag' }, { t: `← ${hostOf(url)} `, c: 'dim' }, { t: `+${r.integrated} rows`, c: 'k catchup' }, { t: ' folded', c: 'dim' }]);
          gossip(node!, 'catchup'); // propagate the external change through the in-page mesh
          emit();
        }
        if (r.closed) {
          const denied = String(r.closed).toLowerCase().includes('deni');
          if (!settled) return settle(!denied, denied ? `denied: ${r.closed}` : undefined);
          try { ws.close(); } catch {}
          return;
        }
        bumpConverge();
      };
      ws.onerror = () => { if (!settled) settle(false, 'ws error (peer not listening, or mixed-content/cert)'); };
      ws.onclose = () => {
        if (node!.liveWs === ws) { node!.liveWs = undefined; node!.live = false; node!.liveAuthed = false; }
        if (settled) {
          logLine(node!, [{ t: 'WARN', c: 'lvl warn' }, { t: ' watch     ', c: 'tag' }, { t: hostOf(url), c: 'hl' }, { t: ' · link closed', c: 'dim' }]);
          emit();
        } else {
          settle(false, 'closed before converge');
        }
      };
    });
  };

  api.disconnectPeer = (nodeId: string) => {
    const node = findNode(nodeId);
    if (!node || !node.liveWs) return;
    const ws = node.liveWs;
    node.liveWs = undefined; node.live = false; node.liveAuthed = false;
    try { ws.close(); } catch {}
    logLine(node, [{ t: 'INFO', c: 'lvl' }, { t: ' disconnect', c: 'tag' }, { t: hostOf(node.externalUrl || ''), c: 'hl' }, { t: ' · stopped watching', c: 'dim' }]);
    emit();
  };

  // ---- OPFS persistence: full-state serialize / restore --------------------
  function localOf(id: string): string | undefined { return findNode(id)?.localId; }

  api.serialize = () => ({
    nodes: nodes.map((n) => ({
      localId: n.localId,
      name: n.name,
      color: n.color,
      online: n.online,
      seedHex: n.seedHex,
      vaultId: n.eng.vault_id(),
      openFileId: n.openFileId,
      createdRemote: n.createdRemote,
      externalUrl: n.externalUrl,
      authKey: n.authKey,
      folders: [...n.folders],
      rows: JSON.parse(n.eng.rows_after('{}')), // every wire row this node holds
    })),
    edges: edges.map((e) => ({ a: localOf(e.a), b: localOf(e.b) })).filter((e) => e.a && e.b),
  });

  api.restore = (state: any) => {
    for (const n of nodes) { try { n.eng.free(); } catch {} }
    nodes.length = 0; edges.length = 0;
    const idMap: Record<string, string> = {};
    let maxLocal = -1;
    for (const sn of state.nodes || []) {
      // Re-author identity from the persisted seed; integrate the saved rows
      // (real fold). Note: MemEngine's per-vault authoring site is fresh per
      // instance, so post-restore edits author under a new site — convergence
      // (keyed per site in the VV) is preserved.
      const eng = new WasmEngine(hexToBytes(sn.seedHex), sn.vaultId || '');
      if (sn.rows && sn.rows.length) { try { eng.integrate(JSON.stringify(sn.rows)); } catch {} }
      const node: Node = {
        id: eng.node_id(),
        localId: sn.localId,
        seedHex: sn.seedHex,
        name: sn.name,
        color: sn.color,
        online: sn.online !== false,
        eng,
        openFileId: sn.openFileId ?? null,
        staged: {},
        folders: new Set(sn.folders || []),
        lines: [],
        lineSeq: 0,
        createdRemote: sn.createdRemote ?? null,
        externalUrl: sn.externalUrl,
        authKey: sn.authKey,
      };
      logLine(node, [
        { t: 'INFO', c: 'lvl' }, { t: ' restore   ', c: 'tag' },
        { t: `node ${node.name}`, c: 'hl' }, { t: ` ${node.eng.row_count()} rows from opfs`, c: 'dim' },
      ]);
      nodes.push(node);
      idMap[sn.localId] = node.id;
      const num = Number(String(sn.localId).replace(/^n/, ''));
      if (!Number.isNaN(num)) maxLocal = Math.max(maxLocal, num);
    }
    localSeq = maxLocal + 1;
    nodeSeq = nodes.length;
    for (const e of state.edges || []) if (idMap[e.a] && idMap[e.b]) edges.push({ a: idMap[e.a], b: idMap[e.b] });
    emit();
  };

  api.removeNode = (id: string) => {
    const i = nodes.findIndex((n) => n.id === id);
    if (i < 0) return;
    if (nodes[i].liveWs) { try { nodes[i].liveWs!.close(); } catch {} }
    for (let j = edges.length - 1; j >= 0; j--) if (edges[j].a === id || edges[j].b === id) edges.splice(j, 1);
    try { nodes[i].eng.free(); } catch {}
    nodes.splice(i, 1);
    emit();
  };

  api.renameNode = (id: string, name: string) => {
    const n = findNode(id);
    if (n && name.trim()) { n.name = name.trim(); emit(); }
  };

  api.setOnline = (id: string, online: boolean) => {
    const node = findNode(id);
    if (!node || node.online === online) return;
    node.online = online;
    if (!online) {
      if (node.liveWs) { const w = node.liveWs; node.liveWs = undefined; node.live = false; node.liveAuthed = false; try { w.close(); } catch {} }
      logLine(node, [
        { t: 'WARN', c: 'lvl warn' }, { t: ' offline   ', c: 'tag' },
        { t: 'link down', c: 'hl' }, { t: ' · edits queue locally (offline-first)', c: 'dim' },
      ]);
    } else {
      logLine(node, [
        { t: 'INFO', c: 'lvl' }, { t: ' online    ', c: 'tag' },
        { t: 'link up', c: 'hl' }, { t: ' · reconnecting peers', c: 'dim' },
      ]);
      for (const pid of peersOf(node.id)) {
        const peer = findNode(pid);
        if (peer && peer.online) {
          logLine(node, [
            { t: 'INFO', c: 'lvl' }, { t: ' handshake ', c: 'tag' },
            { t: `${peer.name} re-auth ok`, c: 'k handshake' }, { t: ' · exchange version vectors', c: 'dim' },
          ]);
          syncEdge(node, peer, 'catchup');  // anti-entropy, both directions
          syncEdge(peer, node, 'catchup');
        }
      }
    }
    emit();
  };

  api.openFile = (nodeId: string, fileId: string) => {
    const n = findNode(nodeId);
    if (n) { n.openFileId = fileId; emit(); }
  };

  // staged edit + debounced commit
  api.stageEdit = (nodeId: string, fileId: string, content: string) => {
    const node = findNode(nodeId);
    if (!node) return;
    node.staged[fileId] = content;
    const key = `${nodeId}|${fileId}`;
    if (debounceTimers[key]) clearTimeout(debounceTimers[key]);
    debounceTimers[key] = setTimeout(() => commitEdit(node, fileId), cfg.debounceMs);
    emit();
  };

  function commitEdit(node: Node, fileId: string) {
    const key = `${node.id}|${fileId}`;
    delete debounceTimers[key];
    const content = node.staged[fileId];
    if (content == null) return;
    delete node.staged[fileId];
    const view = filesView(node)[fileId];
    if (!view || view.deleted) return;
    if (content === view.content) return; // net-zero
    const before = vvOf(node);
    node.eng.record_write(view.path, enc.encode(content));
    const authoredJson = node.eng.rows_after(JSON.stringify(before));
    const authored = JSON.parse(authoredJson) as any[];
    const r = authored[authored.length - 1]?.row;
    logLine(node, [
      { t: 'INFO', c: 'lvl' }, { t: ' commit    ', c: 'tag' },
      { t: 'edit', c: 'k edit' }, { t: ` ${baseOf(view.path)}`, c: 'hl' },
      r ? { t: ` file_id=${fileId.slice(0, 8)} lamport=${r.lamport} seq=${r.seq} ${(r.base_hash || '∅').slice(0, 4)}→${(r.result_hash || '').slice(0, 4)}`, c: 'dim' } : { t: '', c: 'dim' },
    ]);
    gossip(node, 'row');
    pushLive(node, authoredJson);
    emit();
  }

  api.commitNow = (nodeId: string, fileId: string) => {
    const n = findNode(nodeId);
    const key = `${nodeId}|${fileId}`;
    if (n && debounceTimers[key]) { clearTimeout(debounceTimers[key]); commitEdit(n, fileId); }
  };

  api.createFile = (nodeId: string, dir: string, name: string) => {
    const node = findNode(nodeId);
    if (!node || !name.trim()) return;
    const path = (dir ? `${dir}/` : '') + name.trim();
    if (Object.values(filesView(node)).some((f) => !f.deleted && f.path === path)) {
      O.onToast(`path exists: ${path}`, 'warn');
      return;
    }
    const before = vvOf(node);
    node.eng.record_write(path, enc.encode(''));
    const authoredJson = node.eng.rows_after(JSON.stringify(before));
    const authored = JSON.parse(authoredJson) as any[];
    const r = authored[authored.length - 1]?.row;
    const fid = r ? r.file_id : null;
    if (fid) node.openFileId = fid;
    logLine(node, [
      { t: 'INFO', c: 'lvl' }, { t: ' commit    ', c: 'tag' },
      { t: 'create', c: 'k create' }, { t: ` ${path}`, c: 'hl' },
      { t: ` file_id=${(fid || '').slice(0, 8)} class=${r ? r.merge_class : ''} lamport=${r ? r.lamport : ''}`, c: 'dim' },
    ]);
    gossip(node, 'row');
    pushLive(node, authoredJson);
    emit();
  };

  api.createFolder = (nodeId: string, dir: string, name: string) => {
    const node = findNode(nodeId);
    if (!node || !name.trim()) return;
    const path = (dir ? `${dir}/` : '') + name.trim();
    node.folders.add(path);
    logLine(node, [
      { t: 'INFO', c: 'lvl' }, { t: ' mkdir     ', c: 'tag' },
      { t: `${path}/`, c: 'hl' }, { t: ' · local until it holds a synced file', c: 'dim' },
    ]);
    emit();
  };

  api.renameFile = (nodeId: string, fileId: string, newPath: string) => {
    const node = findNode(nodeId);
    if (!node) return;
    const view = filesView(node)[fileId];
    if (!view || view.deleted) return;
    const np = newPath.trim();
    if (!np || np === view.path) return;
    const before = vvOf(node);
    node.eng.record_rename(view.path, np);
    const authoredJson = node.eng.rows_after(JSON.stringify(before));
    const r = (JSON.parse(authoredJson) as any[])[0]?.row;
    logLine(node, [
      { t: 'INFO', c: 'lvl' }, { t: ' commit    ', c: 'tag' },
      { t: 'rename', c: 'k rename' }, { t: ` ${view.path} → ${np}`, c: 'hl' },
      { t: ` file_id=${fileId.slice(0, 8)} (stable) lamport=${r ? r.lamport : ''}`, c: 'dim' },
    ]);
    gossip(node, 'row');
    pushLive(node, authoredJson);
    emit();
  };

  api.moveFile = (nodeId: string, fileId: string, newDir: string) => {
    const node = findNode(nodeId);
    const view = node && filesView(node)[fileId];
    if (!view || view.deleted) return;
    const np = (newDir ? `${newDir}/` : '') + baseOf(view.path);
    if (np === view.path) return;
    api.renameFile(nodeId, fileId, np);
  };

  api.deleteFile = (nodeId: string, fileId: string) => {
    const node = findNode(nodeId);
    if (!node) return;
    const view = filesView(node)[fileId];
    if (!view || view.deleted) return;
    const before = vvOf(node);
    node.eng.record_remove(view.path);
    const authoredJson = node.eng.rows_after(JSON.stringify(before));
    const r = (JSON.parse(authoredJson) as any[])[0]?.row;
    const wasOpen = node.openFileId === fileId;
    if (wasOpen) {
      const live = Object.values(filesView(node)).find((x) => !x.deleted);
      node.openFileId = live ? live.file_id : null;
    }
    logLine(node, [
      { t: 'INFO', c: 'lvl' }, { t: ' commit    ', c: 'tag' },
      { t: 'delete', c: 'k delete' }, { t: ` ${view.path}`, c: 'hl' },
      { t: ` tombstone · remove-wins lamport=${r ? r.lamport : ''}`, c: 'dim' },
    ]);
    gossip(node, 'row');
    pushLive(node, authoredJson);
    emit();
  };

  // ---- read model for the UI -----------------------------------------
  function queuedFor(node: Node): number {
    // own + integrated rows that online peers still lack
    let max = 0;
    for (const pid of peersOf(node.id)) {
      const peer = findNode(pid);
      if (!peer) continue;
      const delta = JSON.parse(node.eng.rows_after(peer.eng.version_vector())) as any[];
      max = Math.max(max, delta.length);
    }
    return max;
  }

  function statusOf(node: Node, inflightByNode: Record<string, boolean>) {
    if (node.syncing) return { kind: 'syncing', label: 'Syncing', note: 'ws:// handshake' };
    if (!node.online) {
      const queued = queuedFor(node);
      return { kind: 'offline', label: 'Offline', note: queued ? `${queued} queued` : 'isolated' };
    }
    if (node.live) {
      if (inflightByNode[node.id]) return { kind: 'syncing', label: 'Syncing', note: 'frames in flight' };
      return { kind: 'insync', label: 'Live', note: hostOf(node.externalUrl || '') };
    }
    if (peersOf(node.id).length === 0) {
      if (node.externalUrl) return { kind: 'solo', label: 'Linked', note: `${hostOf(node.externalUrl)} (idle)` };
      return { kind: 'solo', label: 'Solo', note: 'no peers' };
    }
    if (inflightByNode[node.id]) return { kind: 'syncing', label: 'Syncing', note: 'frames in flight' };
    let behind = false;
    const myVv = vvOf(node);
    for (const pid of peersOf(node.id)) {
      const peer = findNode(pid);
      if (!peer || !peer.online) continue;
      const pVv = vvOf(peer);
      const sites = new Set([...Object.keys(myVv), ...Object.keys(pVv)]);
      for (const s of sites) if ((myVv[s] ?? -1) !== (pVv[s] ?? -1)) behind = true;
    }
    if (behind) return { kind: 'syncing', label: 'Syncing', note: 'converging' };
    return { kind: 'insync', label: 'In sync', note: 'vectors equal' };
  }

  api.getNodes = () => nodes;
  api.getEdges = () => edges;
  api.peersOf = peersOf;
  api.statusOf = statusOf;

  api.snapshot = () => ({
    nodes: nodes.map((n) => ({
      id: n.id,
      name: n.name,
      color: n.color,
      online: n.online,
      site: n.id.slice(0, 4),
      files: filesView(n),
      folders: n.folders,
      openFileId: n.openFileId,
      lines: n.lines,
      staged: n.staged,
      createdRemote: n.createdRemote,
      externalUrl: n.externalUrl,
      syncing: !!n.syncing,
      live: !!n.live,
      peers: peersOf(n.id).map((pid) => findNode(pid)?.name).filter(Boolean),
      rowCount: n.eng.row_count(),
      sshKey: n.eng.node_ssh(),
    })),
    edges: edges.slice(),
  });

  api.clearFresh = () => { for (const n of nodes) for (const l of n.lines) l.fresh = false; };

  return api;
}

export type ASPNetwork = ReturnType<typeof createNetwork>;
