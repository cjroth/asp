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
  mySites: Set<string>;  // authoring site_ids this node has used (change attribution)
  createdRemote: string | null;
  externalUrl?: string;  // a real `asp watch --listen` peer (its iroh ticket)
  authKey?: string;
  relayUrl?: string;     // optional relay override (self-hosted `asp relay`)
  syncing?: boolean;     // initial handshake/catch-up in flight
  pollTimer?: ReturnType<typeof setTimeout>; // the live-sync poll (iroh is one-shot)
  live?: boolean;        // the live (polled) link is up + authed
  liveAuthed?: boolean;
  wantLive?: boolean;    // the user asked for a live link — keep polling
  reconnectTimer?: ReturnType<typeof setTimeout>;
  reconnectDelayMs?: number; // backoff for the next auto-redial
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

  // Remember which authoring site_id(s) this node has written under, so change
  // events in the timeline can be coloured by the device that made them. The
  // MemEngine's authoring site is fresh per instance, so we capture it from the
  // rows just authored (and persist it across reload).
  function noteAuthored(node: Node, authoredJson: string) {
    try {
      for (const w of JSON.parse(authoredJson) as any[]) if (w?.row?.site_id) node.mySites.add(w.row.site_id);
    } catch { /* ignore */ }
  }
  // site_id -> {name, color} across every live node (own authored sites).
  function siteOwnerMap(): Record<string, { name: string; color: string }> {
    const m: Record<string, { name: string; color: string }> = {};
    for (const n of nodes) for (const s of n.mySites) m[s] = { name: n.name, color: n.color };
    return m;
  }

  // ---- the file map, on a diet -------------------------------------------
  // The old single `filesView` decoded EVERY file's bytes (files_json) on EVERY
  // call, and snapshot() runs it for every node on every render — so a large
  // project re-decoded its whole vault many times per sync and the UI crawled.
  // The UI actually only needs decoded CONTENT for the open file (the editor)
  // and any files with a staged edit (the tree's dirty marker); the tree itself
  // just needs paths + merge class + flags. So we split content out of metadata.

  // Per-file fold metadata — NO content. Cached on row_count: metadata only
  // changes when a row is authored/integrated, and row_count bumps on every such
  // row, so an unchanged count means the tree is byte-identical → reuse it.
  const metaCache = new WeakMap<Node, { rows: number; view: Record<string, FileView> }>();
  function filesMeta(node: Node): Record<string, FileView> {
    const rows = node.eng.row_count();
    const hit = metaCache.get(node);
    if (hit && hit.rows === rows) return hit.view;
    const detail = JSON.parse(node.eng.files_detail_json()) as {
      file_id: string; path: string; result_hash: string | null; merge_class: string; deleted: boolean; conflict: boolean;
    }[];
    const view: Record<string, FileView> = {};
    for (const f of detail) {
      view[f.file_id] = {
        file_id: f.file_id,
        path: f.path,
        content: '',
        merge_class: f.merge_class,
        result_hash: f.result_hash,
        deleted: f.deleted,
        collided: f.conflict,
      };
    }
    metaCache.set(node, { rows, view });
    return view;
  }

  // One file's text, decoded on demand (a single wasm read_file, not the whole
  // vault). Used for the open/staged files the UI renders, and the net-zero
  // check on commit.
  function fileText(node: Node, path: string): string {
    const b = node.eng.read_file(path);
    return b ? dec.decode(b) : '';
  }

  // The map handed to the UI: tree metadata for every file, plus decoded content
  // only for the open file and any files with a staged edit. When nothing is
  // open or staged we return the cached metadata object verbatim (stable
  // identity across frames).
  function filesForSnapshot(node: Node): Record<string, FileView> {
    const meta = filesMeta(node);
    const need = new Set<string>(Object.keys(node.staged));
    if (node.openFileId) need.add(node.openFileId);
    if (need.size === 0) return meta;
    const out: Record<string, FileView> = { ...meta };
    for (const fid of need) {
      const m = meta[fid];
      if (m && !m.deleted) out[fid] = { ...m, content: fileText(node, m.path) };
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

  // One bridge sync pass to the external iroh peer (the genuine Session: dial the
  // ticket via the relay, handshake, bidirectional version-vector catch-up). iroh
  // is one-shot, so "live" is a fast poll loop (below) plus an immediate sync when
  // this node authors a row (pushLive) — the connector's catch-up carries the new
  // rows to the peer, and the same pass pulls anything new back.
  async function bridgeSync(node: Node): Promise<void> {
    if (!node.wantLive || !node.externalUrl) return;
    const beforeVv = vvOf(node);
    try {
      await node.eng.sync(node.externalUrl, node.authKey, node.relayUrl);
      node.live = true;
      node.liveAuthed = true;
      node.syncing = false;
      node.reconnectDelayMs = undefined;
      const imported = (JSON.parse(node.eng.rows_after(JSON.stringify(beforeVv))) as any[]).length;
      if (imported > 0) {
        gossip(node, 'catchup'); // propagate the external change through the in-page mesh
        logLine(node, [
          { t: 'INFO', c: 'lvl' }, { t: ' catch-up  ', c: 'tag' },
          { t: `← ${hostOf(node.externalUrl)} `, c: 'dim' }, { t: `+${imported} rows`, c: 'k catchup' }, { t: ' folded', c: 'dim' },
        ]);
      }
      emit();
    } catch (e) {
      node.live = false;
      node.liveAuthed = false;
      node.syncing = false;
      throw e;
    }
  }

  const LIVE_POLL_MS = 1000;
  function scheduleLivePoll(node: Node, delay = LIVE_POLL_MS) {
    clearTimeout(node.pollTimer);
    node.pollTimer = setTimeout(async () => {
      node.pollTimer = undefined;
      try {
        await bridgeSync(node);
      } catch {
        /* transient — keep polling so a dropped link recovers */
      }
      if (node.wantLive) scheduleLivePoll(node);
    }, delay);
  }

  // A locally-authored row should reach the peer promptly: trigger an immediate
  // bridge sync (the poll-loop equivalent of the old optimistic socket push).
  function pushLive(node: Node, rowsJson: string) {
    if (!node.wantLive || !node.externalUrl) return;
    const rows = JSON.parse(rowsJson) as any[];
    if (rows.length === 0) return;
    clearTimeout(node.pollTimer);
    node.pollTimer = undefined;
    logLine(node, [
      { t: 'DEBUG', c: 'lvl' }, { t: ' push      ', c: 'tag' },
      { t: `→ ${hostOf(node.externalUrl)} `, c: 'dim' },
      { t: `${rows.length} row${rows.length > 1 ? 's' : ''}`, c: 'k push' }, { t: ' live', c: 'dim' },
    ]);
    bridgeSync(node)
      .catch(() => {})
      .finally(() => {
        if (node.wantLive) scheduleLivePoll(node);
      });
  }

  // ====================================================================
  // PUBLIC API
  // ====================================================================
  const api: any = {};

  api.setConfig = (patch: Partial<typeof cfg>) => Object.assign(cfg, patch);

  api.addNode = ({ name, remoteId, externalUrl, authKey, relayUrl }: { name?: string; remoteId?: string | null; externalUrl?: string; authKey?: string; relayUrl?: string }) => {
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
      mySites: new Set(),
      createdRemote: null,
    };
    nodes.push(node);

    logLine(node, [
      { t: 'INFO', c: 'lvl' }, { t: ' init      ', c: 'tag' },
      { t: `node ${node.name}`, c: 'hl' }, { t: ` site_id=${node.id.slice(0, 8)} ed25519`, c: 'dim' },
    ]);

    if (externalUrl) {
      // clone from a REAL `asp watch --listen` peer over iroh (by ticket) — the
      // genuine Session handshake + version-vector catch-up (asp clone <ticket>).
      node.createdRemote = hostOf(externalUrl);
      api.connectPeer(node.id, externalUrl, authKey, relayUrl).then(() => {
        const first = Object.values(filesMeta(node)).find((f) => !f.deleted);
        if (first && !node.openFileId) { node.openFileId = first.file_id; emit(); }
      });
    } else if (!remote) {
      // genesis vault: author the seed files (real create rows)
      for (const s of SEED) node.eng.record_write(s.path, enc.encode(s.body));
      noteAuthored(node, node.eng.rows_after('{}'));
      const view = filesMeta(node);
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
  api.connectPeer = (nodeId: string, ticket: string, authKey?: string, relayUrl?: string): Promise<boolean> => {
    const node = findNode(nodeId);
    if (!node) return Promise.resolve(false);
    clearTimeout(node.pollTimer); node.pollTimer = undefined;
    clearTimeout(node.reconnectTimer); node.reconnectTimer = undefined;
    node.wantLive = true;
    node.externalUrl = ticket;
    node.authKey = authKey || undefined;
    node.relayUrl = relayUrl || undefined;
    node.syncing = true;

    logLine(node, [
      { t: 'INFO', c: 'lvl' }, { t: ' watch     ', c: 'tag' },
      { t: `dial ${hostOf(ticket)}`, c: 'hl' }, { t: ' iroh real peer · live · handshake…', c: 'dim' },
    ]);
    emit();

    // iroh is one-shot: converge with a first bridge sync, then keep a fast poll
    // loop alive (+ an immediate sync on each local edit) for the "live" feel.
    return bridgeSync(node)
      .then(() => {
        logLine(node, [
          { t: 'INFO', c: 'lvl' }, { t: ' live      ', c: 'tag' },
          { t: hostOf(ticket), c: 'k catchup' }, { t: ' · converged · watching', c: 'dim' },
        ]);
        emit();
        scheduleLivePoll(node);
        return true;
      })
      .catch((e) => {
        const msg = String(e);
        const denied = /deni|invalid auth/i.test(msg);
        logLine(node, [{ t: 'WARN', c: 'lvl warn' }, { t: ' watch     ', c: 'tag' }, { t: hostOf(ticket), c: 'hl' }, { t: ` · ${denied ? `denied: ${msg}` : msg}`, c: 'dim' }]);
        O.onToast(`${node.name}: ${denied ? 'admission denied' : 'connect failed'}`, 'warn');
        node.syncing = false;
        emit();
        // A denial is terminal; a transient failure keeps retrying so the link recovers.
        if (denied) { node.wantLive = false; return false; }
        scheduleLivePoll(node, 2000);
        return false;
      });
  };

  api.disconnectPeer = (nodeId: string) => {
    const node = findNode(nodeId);
    if (!node) return;
    node.wantLive = false; // user said stop — no auto-redial / poll
    clearTimeout(node.reconnectTimer); node.reconnectTimer = undefined;
    clearTimeout(node.pollTimer); node.pollTimer = undefined;
    if (!node.live && !node.externalUrl) return;
    node.live = false; node.liveAuthed = false;
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
      relayUrl: n.relayUrl,
      wantLive: !!n.wantLive,
      folders: [...n.folders],
      mySites: [...n.mySites],
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
        mySites: new Set(sn.mySites || []),
        createdRemote: sn.createdRemote ?? null,
        externalUrl: sn.externalUrl,
        authKey: sn.authKey,
        relayUrl: sn.relayUrl,
        // Back-compat: state persisted by an older build has no `wantLive`
        // field, but an `externalUrl` means the user had configured a live
        // link — so honor it and auto-redial after the upgrade.
        wantLive: sn.wantLive ?? !!sn.externalUrl,
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
    // Re-establish live links the user had before the reload — otherwise a
    // restored node sits silently disconnected and local edits stop propagating
    // until a manual re-sync. (The redial's VV catch-up pushes anything authored
    // while the page was closed.)
    for (const n of nodes) {
      if (n.wantLive && n.externalUrl && n.online !== false) {
        api.connectPeer(n.id, n.externalUrl, n.authKey, n.relayUrl);
      }
    }
    emit();
  };

  api.removeNode = (id: string) => {
    const i = nodes.findIndex((n) => n.id === id);
    if (i < 0) return;
    nodes[i].wantLive = false;
    clearTimeout(nodes[i].reconnectTimer);
    clearTimeout(nodes[i].pollTimer);
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
      clearTimeout(node.reconnectTimer); node.reconnectTimer = undefined; // pause redial while offline
      clearTimeout(node.pollTimer); node.pollTimer = undefined; node.live = false; node.liveAuthed = false;
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
      // Back online: also re-dial the external live link if the user wants one.
      if (node.wantLive && node.externalUrl && !node.pollTimer) {
        api.connectPeer(node.id, node.externalUrl, node.authKey, node.relayUrl);
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
    const view = filesMeta(node)[fileId];
    if (!view || view.deleted) return;
    if (content === fileText(node, view.path)) return; // net-zero
    const before = vvOf(node);
    node.eng.record_write(view.path, enc.encode(content));
    const authoredJson = node.eng.rows_after(JSON.stringify(before));
    noteAuthored(node, authoredJson);
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
    if (Object.values(filesMeta(node)).some((f) => !f.deleted && f.path === path)) {
      O.onToast(`path exists: ${path}`, 'warn');
      return;
    }
    const before = vvOf(node);
    node.eng.record_write(path, enc.encode(''));
    const authoredJson = node.eng.rows_after(JSON.stringify(before));
    noteAuthored(node, authoredJson);
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
    const view = filesMeta(node)[fileId];
    if (!view || view.deleted) return;
    const np = newPath.trim();
    if (!np || np === view.path) return;
    const before = vvOf(node);
    node.eng.record_rename(view.path, np);
    const authoredJson = node.eng.rows_after(JSON.stringify(before));
    noteAuthored(node, authoredJson);
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
    const view = node && filesMeta(node)[fileId];
    if (!view || view.deleted) return;
    const np = (newDir ? `${newDir}/` : '') + baseOf(view.path);
    if (np === view.path) return;
    api.renameFile(nodeId, fileId, np);
  };

  api.renameFolder = (nodeId: string, oldPath: string, newPath: string) => {
    const node = findNode(nodeId);
    if (!node) return;
    const op = oldPath.trim().replace(/\/+$/, '');
    const np = newPath.trim().replace(/\/+$/, '');
    if (!op || !np || op === np) return;
    // Rename every live file under the folder prefix (file_ids stay stable).
    for (const f of Object.values(filesMeta(node))) {
      if (f.deleted) continue;
      if (f.path === op || f.path.startsWith(`${op}/`)) {
        api.renameFile(nodeId, f.file_id, np + f.path.slice(op.length));
      }
    }
    // Carry over local-only folder entries (mkdir'd but empty).
    const next = new Set<string>();
    for (const fp of node.folders) next.add(fp === op || fp.startsWith(`${op}/`) ? np + fp.slice(op.length) : fp);
    node.folders = next;
    logLine(node, [
      { t: 'INFO', c: 'lvl' }, { t: ' rename    ', c: 'tag' },
      { t: 'rename', c: 'k rename' }, { t: ` ${op}/ → ${np}/`, c: 'hl' }, { t: ' · folder', c: 'dim' },
    ]);
    emit();
  };

  api.deleteFolder = (nodeId: string, folderPath: string) => {
    const node = findNode(nodeId);
    if (!node) return;
    const fp = folderPath.trim().replace(/\/+$/, '');
    if (!fp) return;
    for (const f of Object.values(filesMeta(node))) {
      if (f.deleted) continue;
      if (f.path === fp || f.path.startsWith(`${fp}/`)) api.deleteFile(nodeId, f.file_id);
    }
    const next = new Set<string>();
    for (const x of node.folders) if (x !== fp && !x.startsWith(`${fp}/`)) next.add(x);
    node.folders = next;
    logLine(node, [
      { t: 'INFO', c: 'lvl' }, { t: ' delete    ', c: 'tag' },
      { t: 'delete', c: 'k delete' }, { t: ` ${fp}/`, c: 'hl' }, { t: ' · folder', c: 'dim' },
    ]);
    emit();
  };

  api.deleteFile = (nodeId: string, fileId: string) => {
    const node = findNode(nodeId);
    if (!node) return;
    const view = filesMeta(node)[fileId];
    if (!view || view.deleted) return;
    const before = vvOf(node);
    node.eng.record_remove(view.path);
    const authoredJson = node.eng.rows_after(JSON.stringify(before));
    noteAuthored(node, authoredJson);
    const r = (JSON.parse(authoredJson) as any[])[0]?.row;
    const wasOpen = node.openFileId === fileId;
    if (wasOpen) {
      const live = Object.values(filesMeta(node)).find((x) => !x.deleted);
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
  // Decode one file's text on demand. snapshot() ships content only for the
  // open/staged files (the diet), so callers that need any other file's bytes
  // (e.g. tests asserting convergence) read it through here.
  api.fileText = (nodeId: string, path: string): string => {
    const n = findNode(nodeId);
    return n ? fileText(n, path) : '';
  };
  api.peersOf = peersOf;
  api.statusOf = statusOf;

  // ---- change history (timeline + diff) ------------------------------
  // Every file-change row this node holds, in canonical (lamport, site, seq)
  // order, with before/after text resolved from the bundled content blobs and
  // the authoring device coloured via siteOwnerMap(). Fetched async via the
  // proxy (off the main thread); not part of the snapshot (would bloat it).
  api.history = (nodeId: string) => {
    const node = findNode(nodeId);
    if (!node) return [];
    const wire = JSON.parse(node.eng.rows_after('{}')) as { row: any; blobs: { hash: string; bytes: number[] }[] }[];
    const blobs: Record<string, string> = {};
    for (const w of wire) for (const b of w.blobs) if (!(b.hash in blobs)) blobs[b.hash] = dec.decode(Uint8Array.from(b.bytes));
    const owners = siteOwnerMap();
    const rows = wire.map((w) => w.row);
    rows.sort((a, b) => a.lamport - b.lamport || (a.site_id < b.site_id ? -1 : a.site_id > b.site_id ? 1 : 0) || a.seq - b.seq);
    // resolve each file_id's path as of each row (path is only set on create/rename)
    const pathOf: Record<string, string> = {};
    return rows.map((r) => {
      if (r.path) pathOf[r.file_id] = r.path;
      const owner = owners[r.site_id];
      return {
        rowId: r.id,
        siteId: r.site_id,
        ts: r.ts,
        lamport: r.lamport,
        fileId: r.file_id,
        kind: r.kind as string,
        path: pathOf[r.file_id] || r.path || r.file_id.slice(0, 8),
        ownerName: owner ? owner.name : `${r.site_id.slice(0, 6)}…`,
        ownerColor: owner ? owner.color : 'var(--faint)',
        before: r.base_hash ? (blobs[r.base_hash] ?? '') : '',
        after: r.result_hash ? (blobs[r.result_hash] ?? '') : '',
      };
    });
  };

  api.snapshot = () => ({
    nodes: nodes.map((n) => ({
      id: n.id,
      name: n.name,
      color: n.color,
      online: n.online,
      site: n.id.slice(0, 4),
      files: filesForSnapshot(n),
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
      // The engine-derived status, computed here so it can ride the snapshot
      // across the worker boundary (no live Node on the main thread). The
      // main thread overlays the "frames in flight" bit from packet animation,
      // which is purely main-thread timing — see App's statusFor.
      status: statusOf(n, {}),
    })),
    edges: edges.slice(),
  });

  api.clearFresh = () => { for (const n of nodes) for (const l of n.lines) l.fresh = false; };

  // Tear the whole mesh down to a clean slate (the "Reset" button). Frees every
  // engine + live socket and resets the sequence counters, so a rebuilt mesh
  // starts from n0 again. Symmetric across the in-page and worker-backed paths.
  api.reset = () => {
    for (const k of Object.keys(debounceTimers)) { clearTimeout(debounceTimers[k]); delete debounceTimers[k]; }
    for (const n of nodes) {
      n.wantLive = false;
      clearTimeout(n.reconnectTimer);
      clearTimeout(n.pollTimer);
      try { n.eng.free(); } catch {}
    }
    nodes.length = 0;
    edges.length = 0;
    nodeSeq = 0; localSeq = 0; packetSeq = 0;
    emit();
  };

  return api;
}

export type ASPNetwork = ReturnType<typeof createNetwork>;
