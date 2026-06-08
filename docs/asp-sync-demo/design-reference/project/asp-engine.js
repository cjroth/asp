/* ====================================================================
   asp-engine.js  ·  ASP simulation core (plain JS, no JSX)
   Models: event log, lamport/seq clocks, deterministic fold, file_id
   identity, debounced commits, real-time push + gossip forwarding,
   version-vector reconnect catch-up, offline queueing.
   Exposes window.ASPEngine.createNetwork(opts)
   ==================================================================== */
(function () {
  // ---- small deterministic helpers ---------------------------------
  let _rng = 0x2f6e1a;
  function rand() { _rng = (_rng * 1664525 + 1013904223) >>> 0; return _rng / 0xffffffff; }
  function hex(n) { let s = ""; for (let i = 0; i < n; i++) s += "0123456789abcdef"[(rand() * 16) | 0]; return s; }
  function shortHash(str) {
    let h = 0x811c9dc5;
    for (let i = 0; i < str.length; i++) { h ^= str.charCodeAt(i); h = (h * 0x01000193) >>> 0; }
    return (h >>> 0).toString(16).padStart(8, "0").slice(0, 4);
  }
  function nowClock() {
    const d = new Date();
    const p = (x, n = 2) => String(x).padStart(n, "0");
    return p(d.getHours()) + ":" + p(d.getMinutes()) + ":" + p(d.getSeconds()) + "." + p(d.getMilliseconds(), 3);
  }
  const NODE_NAMES = ["laptop", "desktop", "studio", "phone", "tablet", "server", "macbook", "workstation"];
  const NODE_COLORS = ["#5fb6d4", "#74cf9e", "#c9a6ee", "#e6c06a", "#e08a7a", "#7ab8e0"];

  function mergeClassFor(path) {
    const ext = (path.split(".").pop() || "").toLowerCase();
    if (["js", "ts", "jsx", "tsx", "rs", "py", "go", "json", "sh", "css", "html"].includes(ext)) return "code";
    if (["png", "jpg", "jpeg", "gif", "pdf", "bin"].includes(ext)) return "binary";
    return "text";
  }
  function dirOf(path) { const i = path.lastIndexOf("/"); return i < 0 ? "" : path.slice(0, i); }
  function baseOf(path) { const i = path.lastIndexOf("/"); return i < 0 ? path : path.slice(i + 1); }

  // ---- seed vault ---------------------------------------------------
  const SEED = [
    { path: "README.md", body: "# Vault\n\nShared context for agents + notes.\nSynced live by ASP — no commit, no push.\n" },
    { path: "notes/todo.md", body: "# Todo\n\n- [ ] draft the sync spec\n- [ ] wire up the fold\n- [ ] test offline catch-up\n" },
    { path: "notes/ideas.md", body: "# Ideas\n\n- content-addressed blobs\n- lamport ordering\n- rename keeps file_id\n" },
    { path: "journal/2026-06-07.md", body: "## 2026-06-07\n\nStarted the agent vault. It just works across devices.\n" },
    { path: "src/fold.rs", body: "// deterministic fold\nfn fold(log: &Log) -> State {\n    log.sorted().iter().fold(State::new(), apply)\n}\n" },
  ];

  // ====================================================================
  function createNetwork(opts) {
    const O = Object.assign({ latencyMs: 520, debounceMs: 850, onChange() {}, onPacket() {}, onToast() {} }, opts);
    let cfg = { latencyMs: O.latencyMs, debounceMs: O.debounceMs };

    const nodes = [];           // Node[]
    const edges = [];           // { a, b }  undirected peer links
    let nodeSeq = 0;
    let packetSeq = 0;
    const debounceTimers = {};  // nodeId|fileId -> timer

    function emit() { O.onChange(); }
    function findNode(id) { return nodes.find((n) => n.id === id); }
    function peersOf(id) {
      const out = [];
      for (const e of edges) { if (e.a === id) out.push(e.b); else if (e.b === id) out.push(e.a); }
      return out;
    }
    function edgeExists(a, b) { return edges.some((e) => (e.a === a && e.b === b) || (e.a === b && e.b === a)); }

    // ---- logging ----------------------------------------------------
    function logLine(node, parts) {
      node.lines.push({ id: node.lineSeq++, ts: nowClock(), parts, fresh: true });
      if (node.lines.length > 400) node.lines.splice(0, node.lines.length - 400);
    }
    // parts is an array of {t:'text', c:'class'} tokens we render in the UI

    // ---- row construction ------------------------------------------
    function makeRow(node, { file_id, kind, merge_class, parent, base_hash, content, path }) {
      node.lamport += 1;
      node.seqCtr += 1;
      const result_hash = content == null ? null : shortHash(content);
      const row = {
        id: hex(4),
        site_id: node.id,
        lamport: node.lamport,
        seq: node.seqCtr,
        ts: nowClock(),
        file_id,
        kind,
        merge_class,
        parent: parent || null,
        base_hash: base_hash || null,
        result_hash,
        path: path != null ? path : null,
        content: content != null ? content : null, // sim: carry blob inline
      };
      return row;
    }

    function lastRowFor(node, file_id) {
      let r = null;
      for (const row of node.log) if (row.file_id === file_id) { if (!r || row.lamport > r.lamport) r = row; }
      return r;
    }

    // ---- fold / materialize ----------------------------------------
    function materialize(node) {
      const rows = node.log.slice().sort((x, y) =>
        (x.lamport - y.lamport) || (x.site_id < y.site_id ? -1 : x.site_id > y.site_id ? 1 : 0) ||
        (x.id < y.id ? -1 : 1));
      const files = {};
      for (const row of rows) {
        let f = files[row.file_id];
        if (!f) f = files[row.file_id] = { file_id: row.file_id, path: null, content: "", merge_class: row.merge_class, deleted: false, result_hash: null, lamport: 0, site_id: null };
        if (row.kind === "create") { f.path = row.path; f.content = row.content || ""; f.merge_class = row.merge_class; }
        else if (row.kind === "edit") { if (!f.deleted) f.content = row.content || ""; }
        else if (row.kind === "rename") { f.path = row.path; }
        else if (row.kind === "delete") { f.deleted = true; }
        f.result_hash = row.result_hash || f.result_hash;
        f.lamport = row.lamport; f.site_id = row.site_id;
      }
      // resolve live-path collisions deterministically (lower fold-order keeps path)
      const live = Object.values(files).filter((f) => !f.deleted && f.path);
      const byPath = {};
      for (const f of live) (byPath[f.path] = byPath[f.path] || []).push(f);
      for (const p in byPath) {
        const group = byPath[p];
        if (group.length < 2) continue;
        group.sort((a, b) => (a.lamport - b.lamport) || (a.file_id < b.file_id ? -1 : 1));
        for (let i = 1; i < group.length; i++) {
          const ext = p.includes(".") ? "." + p.split(".").pop() : "";
          const stem = ext ? p.slice(0, -ext.length) : p;
          group[i].path = stem + " (" + i + ")" + ext;
          group[i].collided = true;
        }
      }
      node.files = files;
    }

    // ---- integrate an incoming row ---------------------------------
    function integrate(node, row, fromName, viaCatchup) {
      if (node.knownIds.has(row.id)) return false;
      node.knownIds.add(row.id);
      node.log.push(row);
      node.vv[row.site_id] = Math.max(node.vv[row.site_id] || 0, row.seq);
      node.lamport = Math.max(node.lamport, row.lamport);
      materialize(node);
      if (!viaCatchup) {
        logLine(node, [
          { t: "INFO", c: "lvl" }, { t: " integrate ", c: "tag" },
          { t: "← " + fromName + " ", c: "dim" },
          { t: row.kind, c: "k " + row.kind }, { t: " " + (row.path ? baseOf(row.path) : row.file_id), c: "hl" },
          { t: " id=" + row.id + " lamport=" + row.lamport, c: "dim" },
        ]);
      }
      return true;
    }

    // ---- push / gossip forwarding ----------------------------------
    function pushRow(node, row, excludeName) {
      const targets = peersOf(node.id).map(findNode).filter((p) => p && p.online && node.online && p.name !== excludeName);
      for (const peer of targets) {
        if (peer.knownIds.has(row.id)) continue;
        dispatchFrame(node, peer, [row], "row");
      }
    }

    function dispatchFrame(from, to, rows, kind) {
      const pid = ++packetSeq;
      const bytes = rows.reduce((a, r) => a + 60 + (r.content ? r.content.length : 0), 0);
      // visual packet
      O.onPacket({ id: pid, fromId: from.id, toId: to.id, kind, started: performance.now(), dur: cfg.latencyMs });
      if (kind === "row") {
        logLine(from, [
          { t: "DEBUG", c: "lvl" }, { t: " push      ", c: "tag" },
          { t: "→ " + to.name + " ", c: "dim" },
          { t: "id=" + rows[0].id, c: "k push" }, { t: " (" + rows.length + " row, " + bytes + "B)", c: "dim" },
        ]);
      }
      setTimeout(() => {
        if (!from.online || !to.online || !edgeExists(from.id, to.id)) return; // frame lost
        let n = 0;
        for (const r of rows) if (integrate(to, r, from.name, kind === "catchup")) n++;
        if (kind === "catchup" && n > 0) {
          logLine(to, [
            { t: "INFO", c: "lvl" }, { t: " catch-up  ", c: "tag" },
            { t: "← " + from.name + " ", c: "dim" },
            { t: "+" + n + " rows", c: "k catchup" }, { t: " folded → materialized", c: "dim" },
          ]);
        }
        // forward (gossip): peer re-pushes to its other peers
        for (const r of rows) pushRow(to, r, from.name);
        emit();
      }, cfg.latencyMs);
    }

    // ---- version-vector reconnect catch-up -------------------------
    function syncPair(a, b) {
      if (!a.online || !b.online) return;
      // a sends rows b is missing
      const aToB = a.log.filter((r) => (b.vv[r.site_id] || 0) < r.seq && !b.knownIds.has(r.id));
      const bToA = b.log.filter((r) => (a.vv[r.site_id] || 0) < r.seq && !a.knownIds.has(r.id));
      if (aToB.length) {
        logLine(a, [{ t: "INFO", c: "lvl" }, { t: " anti-entropy ", c: "tag" }, { t: "→ " + b.name + " ", c: "dim" }, { t: "send " + aToB.length + " rows", c: "k catchup" }, { t: " (vv diff)", c: "dim" }]);
        dispatchFrame(a, b, aToB, "catchup");
      }
      if (bToA.length) {
        logLine(b, [{ t: "INFO", c: "lvl" }, { t: " anti-entropy ", c: "tag" }, { t: "→ " + a.name + " ", c: "dim" }, { t: "send " + bToA.length + " rows", c: "k catchup" }, { t: " (vv diff)", c: "dim" }]);
        dispatchFrame(b, a, bToA, "catchup");
      }
    }

    // ====================================================================
    // PUBLIC API
    // ====================================================================
    const api = {};

    api.setConfig = (patch) => { Object.assign(cfg, patch); };

    api.addNode = ({ name, remoteId }) => {
      const idx = nodeSeq++;
      const node = {
        id: hex(4),
        name: name || NODE_NAMES[idx % NODE_NAMES.length] + (idx >= NODE_NAMES.length ? "-" + idx : ""),
        color: NODE_COLORS[idx % NODE_COLORS.length],
        online: true,
        log: [],
        knownIds: new Set(),
        vv: {},            // version vector: site_id -> highest seq seen
        lamport: 0,
        seqCtr: 0,
        files: {},
        folders: new Set(),
        openFileId: null,
        lines: [],
        lineSeq: 0,
        createdRemote: null,
      };
      nodes.push(node);

      logLine(node, [{ t: "INFO", c: "lvl" }, { t: " init      ", c: "tag" }, { t: "node " + node.name, c: "hl" }, { t: " site_id=" + node.id + " ed25519", c: "dim" }]);

      if (remoteId == null) {
        // genesis vault: seed files
        for (const s of SEED) {
          const file_id = "f" + hex(3);
          const mc = mergeClassFor(s.path);
          const row = makeRow(node, { file_id, kind: "create", merge_class: mc, content: s.body, path: s.path });
          node.knownIds.add(row.id); node.log.push(row); node.vv[node.id] = row.seq;
        }
        materialize(node);
        const first = Object.values(node.files).find((f) => f.path === "README.md");
        node.openFileId = first ? first.file_id : Object.values(node.files)[0]?.file_id || null;
        logLine(node, [{ t: "INFO", c: "lvl" }, { t: " commit    ", c: "tag" }, { t: "genesis", c: "k create" }, { t: " " + SEED.length + " files materialized", c: "dim" }]);
      } else {
        // clone from remote: handshake + full catch-up
        const remote = findNode(remoteId);
        node.createdRemote = remote.name;
        edges.push({ a: node.id, b: remote.id });
        logLine(node, [{ t: "INFO", c: "lvl" }, { t: " clone     ", c: "tag" }, { t: "dial " + remote.name, c: "hl" }, { t: " wss:// handshake…", c: "dim" }]);
        logLine(node, [{ t: "INFO", c: "lvl" }, { t: " handshake ", c: "tag" }, { t: "ed25519 mutual-auth ok", c: "k handshake" }, { t: " · admitted (authorized_keys)", c: "dim" }]);
        logLine(remote, [{ t: "INFO", c: "lvl" }, { t: " peer      ", c: "tag" }, { t: node.name + " connected", c: "k peer" }, { t: " · key authorized · catch-up", c: "dim" }]);
        // full catch-up: remote ships all its rows
        const all = remote.online ? remote.log.slice() : [];
        materialize(node);
        if (remote.online) dispatchFrame(remote, node, all, "catchup");
        node.openFileId = remote.openFileId;
      }
      emit();
      return node.id;
    };

    api.removeNode = (id) => {
      const i = nodes.findIndex((n) => n.id === id);
      if (i < 0) return;
      for (let j = edges.length - 1; j >= 0; j--) if (edges[j].a === id || edges[j].b === id) edges.splice(j, 1);
      nodes.splice(i, 1);
      emit();
    };

    api.renameNode = (id, name) => { const n = findNode(id); if (n && name.trim()) { n.name = name.trim(); emit(); } };

    api.setOnline = (id, online) => {
      const node = findNode(id);
      if (!node || node.online === online) return;
      node.online = online;
      if (!online) {
        logLine(node, [{ t: "WARN", c: "lvl warn" }, { t: " offline   ", c: "tag" }, { t: "link down", c: "hl" }, { t: " · edits queue locally (offline-first)", c: "dim" }]);
      } else {
        logLine(node, [{ t: "INFO", c: "lvl" }, { t: " online    ", c: "tag" }, { t: "link up", c: "hl" }, { t: " · reconnecting peers", c: "dim" }]);
        for (const pid of peersOf(node.id)) {
          const peer = findNode(pid);
          if (peer && peer.online) {
            logLine(node, [{ t: "INFO", c: "lvl" }, { t: " handshake ", c: "tag" }, { t: peer.name + " re-auth ok", c: "k handshake" }, { t: " · exchange version vectors", c: "dim" }]);
            syncPair(node, peer);
          }
        }
      }
      emit();
    };

    api.openFile = (nodeId, fileId) => { const n = findNode(nodeId); if (n) { n.openFileId = fileId; emit(); } };

    // staged edit + debounced commit
    api.stageEdit = (nodeId, fileId, content) => {
      const node = findNode(nodeId);
      if (!node) return;
      node._staged = node._staged || {};
      node._staged[fileId] = content;
      const key = nodeId + "|" + fileId;
      if (debounceTimers[key]) clearTimeout(debounceTimers[key]);
      debounceTimers[key] = setTimeout(() => { commitEdit(node, fileId); }, cfg.debounceMs);
      emit();
    };

    function commitEdit(node, fileId) {
      const key = node.id + "|" + fileId;
      delete debounceTimers[key];
      const content = node._staged && node._staged[fileId];
      if (content == null) return;
      delete node._staged[fileId];
      const f = node.files[fileId];
      if (!f || f.deleted) return;
      if (content === f.content) return; // net-zero
      const prev = lastRowFor(node, fileId);
      const row = makeRow(node, { file_id: fileId, kind: "edit", merge_class: f.merge_class, parent: prev ? prev.id : null, base_hash: f.result_hash, content, path: null });
      node.knownIds.add(row.id); node.log.push(row); node.vv[node.id] = row.seq;
      materialize(node);
      logLine(node, [
        { t: "INFO", c: "lvl" }, { t: " commit    ", c: "tag" },
        { t: "edit", c: "k edit" }, { t: " " + baseOf(f.path), c: "hl" },
        { t: " file_id=" + fileId + " lamport=" + row.lamport + " seq=" + row.seq + " " + (row.base_hash || "∅") + "→" + row.result_hash, c: "dim" },
      ]);
      pushRow(node, row);
      emit();
    }

    api.commitNow = (nodeId, fileId) => { const n = findNode(nodeId); if (n) { const key = nodeId + "|" + fileId; if (debounceTimers[key]) { clearTimeout(debounceTimers[key]); commitEdit(n, fileId); } } };

    api.createFile = (nodeId, dir, name) => {
      const node = findNode(nodeId);
      if (!node || !name.trim()) return;
      const path = (dir ? dir + "/" : "") + name.trim();
      if (Object.values(node.files).some((f) => !f.deleted && f.path === path)) { O.onToast("path exists: " + path, "warn"); return; }
      const file_id = "f" + hex(3);
      const mc = mergeClassFor(path);
      const body = mc === "code" ? "" : "";
      const row = makeRow(node, { file_id, kind: "create", merge_class: mc, content: body, path });
      node.knownIds.add(row.id); node.log.push(row); node.vv[node.id] = row.seq;
      materialize(node); node.openFileId = file_id;
      logLine(node, [{ t: "INFO", c: "lvl" }, { t: " commit    ", c: "tag" }, { t: "create", c: "k create" }, { t: " " + path, c: "hl" }, { t: " file_id=" + file_id + " class=" + mc + " lamport=" + row.lamport, c: "dim" }]);
      pushRow(node, row);
      emit();
    };

    api.createFolder = (nodeId, dir, name) => {
      const node = findNode(nodeId);
      if (!node || !name.trim()) return;
      const path = (dir ? dir + "/" : "") + name.trim();
      node.folders.add(path);
      logLine(node, [{ t: "INFO", c: "lvl" }, { t: " mkdir     ", c: "tag" }, { t: path + "/", c: "hl" }, { t: " · local until it holds a synced file", c: "dim" }]);
      emit();
    };

    api.renameFile = (nodeId, fileId, newPath) => {
      const node = findNode(nodeId);
      if (!node) return;
      const f = node.files[fileId];
      if (!f || f.deleted) return;
      newPath = newPath.trim(); if (!newPath || newPath === f.path) return;
      const old = f.path;
      const prev = lastRowFor(node, fileId);
      const row = makeRow(node, { file_id: fileId, kind: "rename", merge_class: f.merge_class, parent: prev ? prev.id : null, base_hash: f.result_hash, content: null, path: newPath });
      row.result_hash = f.result_hash; // rename keeps content
      node.knownIds.add(row.id); node.log.push(row); node.vv[node.id] = row.seq;
      materialize(node);
      logLine(node, [{ t: "INFO", c: "lvl" }, { t: " commit    ", c: "tag" }, { t: "rename", c: "k rename" }, { t: " " + old + " → " + newPath, c: "hl" }, { t: " file_id=" + fileId + " (stable) lamport=" + row.lamport, c: "dim" }]);
      pushRow(node, row);
      emit();
    };

    api.moveFile = (nodeId, fileId, newDir) => {
      const node = findNode(nodeId);
      const f = node && node.files[fileId];
      if (!f || f.deleted) return;
      const np = (newDir ? newDir + "/" : "") + baseOf(f.path);
      if (np === f.path) return;
      api.renameFile(nodeId, fileId, np);
    };

    api.deleteFile = (nodeId, fileId) => {
      const node = findNode(nodeId);
      if (!node) return;
      const f = node.files[fileId];
      if (!f || f.deleted) return;
      const prev = lastRowFor(node, fileId);
      const row = makeRow(node, { file_id: fileId, kind: "delete", merge_class: f.merge_class, parent: prev ? prev.id : null, base_hash: f.result_hash, content: null, path: f.path });
      node.knownIds.add(row.id); node.log.push(row); node.vv[node.id] = row.seq;
      const wasOpen = node.openFileId === fileId;
      materialize(node);
      if (wasOpen) node.openFileId = (Object.values(node.files).find((x) => !x.deleted) || {}).file_id || null;
      logLine(node, [{ t: "INFO", c: "lvl" }, { t: " commit    ", c: "tag" }, { t: "delete", c: "k delete" }, { t: " " + f.path, c: "hl" }, { t: " tombstone · remove-wins lamport=" + row.lamport, c: "dim" }]);
      pushRow(node, row);
      emit();
    };

    // ---- read model for the UI -------------------------------------
    function globalMaxVV() {
      const m = {};
      for (const n of nodes) for (const s in n.vv) m[s] = Math.max(m[s] || 0, n.vv[s]);
      return m;
    }

    function statusOf(node, inflightByNode) {
      if (!node.online) {
        // count rows others don't have yet
        const gmax = globalMaxVV();
        let queued = 0;
        for (const s in node.vv) { /* own authored beyond peers */ }
        for (const n of nodes) { if (n === node) continue; }
        // queued = own rows peers are missing
        for (const r of node.log) {
          if (r.site_id !== node.id) continue;
          const anyPeerHas = peersOf(node.id).map(findNode).some((p) => p && (p.vv[r.site_id] || 0) >= r.seq);
          if (!anyPeerHas) queued++;
        }
        return { kind: "offline", label: "Offline", note: queued ? queued + " queued" : "isolated" };
      }
      if (peersOf(node.id).length === 0) return { kind: "solo", label: "Solo", note: "no peers" };
      if (inflightByNode[node.id]) return { kind: "syncing", label: "Syncing", note: "frames in flight" };
      // compare vv with each online connected peer
      let behind = false;
      for (const pid of peersOf(node.id)) {
        const peer = findNode(pid);
        if (!peer || !peer.online) continue;
        const sites = new Set([...Object.keys(node.vv), ...Object.keys(peer.vv)]);
        for (const s of sites) if ((node.vv[s] || 0) !== (peer.vv[s] || 0)) behind = true;
      }
      if (behind) return { kind: "syncing", label: "Syncing", note: "converging" };
      return { kind: "insync", label: "In sync", note: "vectors equal" };
    }

    api.getNodes = () => nodes;
    api.getEdges = () => edges;
    api.peersOf = peersOf;
    api.statusOf = statusOf;
    api.snapshot = () => ({
      nodes: nodes.map((n) => ({
        id: n.id, name: n.name, color: n.color, online: n.online,
        files: n.files, folders: n.folders, openFileId: n.openFileId,
        lines: n.lines, log: n.log, vv: n.vv,
        staged: n._staged || {},
        createdRemote: n.createdRemote,
        peers: peersOf(n.id).map((pid) => findNode(pid)?.name).filter(Boolean),
        rowCount: n.log.length,
      })),
      edges: edges.slice(),
    });
    api.clearFresh = () => { for (const n of nodes) for (const l of n.lines) l.fresh = false; };

    return api;
  }

  window.ASPEngine = { createNetwork };
})();
