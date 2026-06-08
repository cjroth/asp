/* ====================================================================
   asp-components.jsx · presentational pieces for the ASP demo
   Exports to window: StatusPill, FileTree, Editor, EventLog,
   NetworkMap, AddNodeDialog, NodePanel, NodeStrip
   ==================================================================== */
const { useState, useRef, useEffect, useLayoutEffect, useCallback } = React;

/* ---------- helpers ---------- */
function baseOf(p) { const i = p.lastIndexOf("/"); return i < 0 ? p : p.slice(i + 1); }
function dirOf(p) { const i = p.lastIndexOf("/"); return i < 0 ? "" : p.slice(0, i); }

function buildTree(files, folders) {
  const root = { type: "dir", name: "", path: "", children: {} };
  function ensureDir(path) {
    if (!path) return root;
    const segs = path.split("/");
    let cur = root, acc = "";
    for (const s of segs) {
      acc = acc ? acc + "/" + s : s;
      if (!cur.children[s]) cur.children[s] = { type: "dir", name: s, path: acc, children: {} };
      cur = cur.children[s];
    }
    return cur;
  }
  for (const path of folders) ensureDir(path);
  for (const f of Object.values(files)) {
    if (f.deleted || !f.path) continue;
    const d = ensureDir(dirOf(f.path));
    d.children[baseOf(f.path)] = { type: "file", name: baseOf(f.path), path: f.path, file: f };
  }
  function sort(node) {
    const kids = Object.values(node.children);
    kids.forEach((k) => k.type === "dir" && sort(k));
    kids.sort((a, b) => (a.type !== b.type ? (a.type === "dir" ? -1 : 1) : a.name.localeCompare(b.name)));
    node.sorted = kids;
  }
  sort(root);
  return root;
}

/* ==================================================================== */
function StatusPill({ status }) {
  return (
    <span className={"status " + status.kind} title={status.note}>
      <span className="led"></span>{status.label}
    </span>
  );
}
window.StatusPill = StatusPill;

/* ==================================================================== */
function TreeRow({ node, depth, snap, collapsed, toggle, api, dragRef, renaming, setRenaming }) {
  const [dragOver, setDragOver] = useState(false);
  const isDir = node.type === "dir";
  const isOpen = snap.openFileId && node.file && node.file.file_id === snap.openFileId;
  const isCollapsed = isDir && collapsed.has(node.path);
  const dirty = node.file && snap.staged[node.file.file_id] != null && snap.staged[node.file.file_id] !== node.file.content;
  const renamingThis = renaming && renaming.id === (isDir ? "d:" + node.path : node.file.file_id);

  function onDrop(e) {
    e.preventDefault(); e.stopPropagation(); setDragOver(false);
    const fid = dragRef.current;
    if (!fid) return;
    api.moveFile(snap.id, fid, isDir ? node.path : dirOf(node.path));
  }

  return (
    <div className="tnode">
      <div
        className={"trow " + (isDir ? "folder " : "") + (isOpen ? "active " : "") + (dragOver ? "dragover" : "")}
        style={{ paddingLeft: 8 + depth * 13 }}
        draggable={!isDir && !renamingThis}
        onDragStart={(e) => { dragRef.current = node.file.file_id; e.dataTransfer.effectAllowed = "move"; }}
        onDragEnd={() => { dragRef.current = null; }}
        onDragOver={(e) => { if (dragRef.current) { e.preventDefault(); setDragOver(true); } }}
        onDragLeave={() => setDragOver(false)}
        onDrop={onDrop}
        onClick={() => { if (isDir) toggle(node.path); else api.openFile(snap.id, node.file.file_id); }}
      >
        {isDir
          ? <span className="twist">{isCollapsed ? "▸" : "▾"}</span>
          : <span className="twist"></span>}
        {!isDir && <span className="ico">·</span>}
        {renamingThis
          ? <input className="rn-in" autoFocus defaultValue={node.name}
              onClick={(e) => e.stopPropagation()}
              onBlur={(e) => { commitRn(e.target.value); }}
              onKeyDown={(e) => { if (e.key === "Enter") commitRn(e.target.value); if (e.key === "Escape") setRenaming(null); }} />
          : <span className={"nm" + (dirty ? " dirty" : "")}>{node.name}{isDir ? "/" : ""}</span>}
        {!isDir && !renamingThis && (
          <span className="row-actions" style={{ marginLeft: "auto", display: "flex", gap: 1, opacity: 0.0 }}>
          </span>
        )}
        {node.file && node.file.collided && <span className="badge-dot" style={{ background: "var(--amber)" }} title="path collision — suffixed"></span>}
      </div>
      {isDir && !isCollapsed && node.sorted.map((c) => (
        <TreeRow key={c.path + c.type} node={c} depth={depth + 1} snap={snap} collapsed={collapsed} toggle={toggle} api={api} dragRef={dragRef} renaming={renaming} setRenaming={setRenaming} />
      ))}
    </div>
  );
  function commitRn(val) {
    if (isDir) { setRenaming(null); return; }
    const np = (dirOf(node.path) ? dirOf(node.path) + "/" : "") + val.trim();
    if (val.trim() && np !== node.path) api.renameFile(snap.id, node.file.file_id, np);
    setRenaming(null);
  }
}

function FileTree({ snap, api }) {
  const [collapsed, setCollapsed] = useState(new Set());
  const [renaming, setRenaming] = useState(null);
  const dragRef = useRef(null);
  const tree = buildTree(snap.files, snap.folders);
  const toggle = (path) => setCollapsed((s) => { const n = new Set(s); n.has(path) ? n.delete(path) : n.add(path); return n; });
  const liveCount = Object.values(snap.files).filter((f) => !f.deleted).length;

  function newFile() {
    const name = prompt("New file (path relative to vault root):", "notes/untitled.md");
    if (name) api.createFile(snap.id, dirOf(name), baseOf(name));
  }
  function newFolder() {
    const name = prompt("New folder (path):", "archive");
    if (name) api.createFolder(snap.id, dirOf(name), baseOf(name));
  }
  function renameOpen() {
    if (snap.openFileId) setRenaming({ id: snap.openFileId });
  }
  function deleteOpen() {
    if (snap.openFileId) { const f = snap.files[snap.openFileId]; if (f && confirm("Delete " + f.path + " ? (tombstone row, remove-wins)")) api.deleteFile(snap.id, snap.openFileId); }
  }

  const [rootOver, setRootOver] = useState(false);
  return (
    <div className="np-tree">
      <div className="tree-head">
        <span className="t">vault · {liveCount}</span>
        <span className="tools">
          <button className="icon-btn" title="New file" onClick={newFile}>＋</button>
          <button className="icon-btn" title="New folder" onClick={newFolder}>⊞</button>
          <button className="icon-btn" title="Rename open file" onClick={renameOpen}>✎</button>
          <button className="icon-btn" title="Delete open file" onClick={deleteOpen}>✕</button>
        </span>
      </div>
      <div className={"tree-scroll" + (rootOver ? " dragover" : "")}
        onDragOver={(e) => { if (dragRef.current) { e.preventDefault(); setRootOver(true); } }}
        onDragLeave={() => setRootOver(false)}
        onDrop={(e) => { e.preventDefault(); setRootOver(false); if (dragRef.current) api.moveFile(snap.id, dragRef.current, ""); }}>
        {tree.sorted.length === 0
          ? <div className="tree-empty">empty vault.<br />press ＋ to create a file.</div>
          : tree.sorted.map((c) => (
            <TreeRow key={c.path + c.type} node={c} depth={0} snap={snap} collapsed={collapsed} toggle={toggle} api={api} dragRef={dragRef} renaming={renaming} setRenaming={setRenaming} />
          ))}
      </div>
    </div>
  );
}
window.FileTree = FileTree;

/* ==================================================================== */
function Editor({ snap, api }) {
  const f = snap.openFileId ? snap.files[snap.openFileId] : null;
  const staged = f && snap.staged[f.file_id];
  const value = staged != null ? staged : (f ? f.content : "");
  const dirty = f && staged != null && staged !== f.content;
  const taRef = useRef(null);

  if (!f || f.deleted) {
    return (
      <div className="np-editor">
        <div className="ed-empty">no file open.<br />select a file in the tree, or press ＋ to create one.</div>
      </div>
    );
  }
  return (
    <div className="np-editor">
      <div className="ed-tab">
        <span className="path">{f.path}</span>
        <span className={"cls " + f.merge_class}>{f.merge_class}</span>
        <span className="spacer"></span>
        <span className="hash">@{f.result_hash || "∅"}</span>
      </div>
      <div className="ed-area">
        <textarea ref={taRef} spellCheck={false} value={value}
          placeholder="// empty file — type to edit, auto-commits on debounce"
          onChange={(e) => api.stageEdit(snap.id, f.file_id, e.target.value)} />
      </div>
      <div className="ed-foot">
        <span>file_id={f.file_id}</span>
        <span className="spacer"></span>
        {dirty
          ? <span className="commit-note"><span className="led"></span>staged · auto-commit…</span>
          : <span>{value.split("\n").length} lines · {value.length} B</span>}
      </div>
    </div>
  );
}
window.Editor = Editor;

/* ==================================================================== */
function EventLog({ snap }) {
  const scrollRef = useRef(null);
  const atBottom = useRef(true);
  useLayoutEffect(() => {
    const el = scrollRef.current;
    if (el && atBottom.current) el.scrollTop = el.scrollHeight;
  });
  function onScroll(e) {
    const el = e.target;
    atBottom.current = el.scrollHeight - el.scrollTop - el.clientHeight < 24;
  }
  return (
    <div className="np-log" style={{ "--logh": "152px" }}>
      <div className="log-head">
        <span className="t">event log</span>
        <span className="spacer"></span>
        <span className="count">{snap.rowCount} rows · {snap.lines.length} lines</span>
      </div>
      <div className="log-scroll" ref={scrollRef} onScroll={onScroll}>
        {snap.lines.map((l) => (
          <span key={l.id} className={"log-line" + (l.fresh ? " fresh" : "")}>
            <span className="ts">{l.ts}</span>{"  "}
            {l.parts.map((p, i) => <span key={i} className={p.c}>{p.t}</span>)}
          </span>
        ))}
      </div>
    </div>
  );
}
window.EventLog = EventLog;

/* ==================================================================== */
/* Network map — spatial p2p view with animated packets               */
function NetworkMap({ snap, packetsRef, selected, onSelect, statusFor }) {
  const wrapRef = useRef(null);
  const [w, setW] = useState(900);
  const [, force] = useState(0);
  const H = snap.nodes.length ? 150 : 0;

  useLayoutEffect(() => {
    const el = wrapRef.current; if (!el) return;
    const ro = new ResizeObserver(() => setW(el.clientWidth));
    ro.observe(el); setW(el.clientWidth);
    return () => ro.disconnect();
  }, []);

  // animate while packets exist
  useEffect(() => {
    let raf;
    const loop = () => {
      const now = performance.now();
      const arr = packetsRef.current;
      for (let i = arr.length - 1; i >= 0; i--) if (now - arr[i].started > arr[i].dur + 60) arr.splice(i, 1);
      force((x) => x + 1);
      raf = requestAnimationFrame(loop);
    };
    raf = requestAnimationFrame(loop);
    return () => cancelAnimationFrame(raf);
  }, []);

  // positions
  const n = snap.nodes.length;
  const margin = 90, top = 58;
  const usable = Math.max(1, w - margin * 2);
  const pos = {};
  snap.nodes.forEach((nd, i) => {
    const x = n === 1 ? w / 2 : margin + (usable * i) / (n - 1);
    const y = top + (i % 2 === 0 ? 0 : 14);
    pos[nd.id] = { x, y };
  });

  const colorFor = (st) => ({ insync: "var(--green)", syncing: "var(--cyan)", offline: "var(--red)", solo: "var(--muted)" }[st.kind] || "var(--muted)");

  const now = performance.now();
  return (
    <div className="netmap" ref={wrapRef} style={{ height: H }}>
      <span className="map-label">network · p2p mesh</span>
      <svg height={H} width={w} viewBox={`0 0 ${w} ${H}`}>
        {/* edges */}
        {snap.edges.map((e, i) => {
          const a = pos[e.a], b = pos[e.b];
          if (!a || !b) return null;
          const na = snap.nodes.find((x) => x.id === e.a), nb = snap.nodes.find((x) => x.id === e.b);
          const active = na && nb && na.online && nb.online;
          return <line key={i} className={"edge-line" + (active ? " active" : "")} x1={a.x} y1={a.y} x2={b.x} y2={b.y} strokeDasharray={active ? "none" : "4 4"} />;
        })}
        {/* packets */}
        {packetsRef.current.map((pk) => {
          const a = pos[pk.fromId], b = pos[pk.toId];
          if (!a || !b) return null;
          const t = Math.min(1, (now - pk.started) / pk.dur);
          const x = a.x + (b.x - a.x) * t, y = a.y + (b.y - a.y) * t;
          const col = pk.kind === "catchup" ? "var(--amber)" : "var(--cyan)";
          return <circle key={pk.id} className="packet" cx={x} cy={y} r={pk.kind === "catchup" ? 4.5 : 3.5} fill={col} style={{ filter: `drop-shadow(0 0 5px ${col})` }} />;
        })}
        {/* nodes */}
        {snap.nodes.map((nd) => {
          const p = pos[nd.id]; if (!p) return null;
          const st = statusFor(nd.id);
          const ring = colorFor(st);
          const sel = selected === nd.id;
          return (
            <g key={nd.id} className="node-dot-g" onClick={() => onSelect(nd.id)}>
              {st.kind === "syncing" && <circle cx={p.x} cy={p.y} r="20" fill="none" stroke={ring} strokeWidth="1.5" opacity="0.35">
                <animate attributeName="r" from="13" to="24" dur="1.1s" repeatCount="indefinite" />
                <animate attributeName="opacity" from="0.5" to="0" dur="1.1s" repeatCount="indefinite" />
              </circle>}
              <circle className={"node-dot" + (sel ? " sel" : "")} cx={p.x} cy={p.y} r="13" fill={nd.online ? "var(--panel-2)" : "var(--ink-2)"} stroke={sel ? "var(--cyan)" : ring} strokeWidth={sel ? 2.5 : 2} />
              <circle cx={p.x} cy={p.y} r="4.5" fill={ring} opacity={nd.online ? 1 : 0.5} />
              <text className="node-label" x={p.x} y={p.y - 22}>{nd.name}</text>
              <text className="node-sub" x={p.x} y={p.y + 30}>{st.label}{st.note && st.kind === "offline" ? " · " + st.note : ""}</text>
            </g>
          );
        })}
      </svg>
    </div>
  );
}
window.NetworkMap = NetworkMap;

/* ==================================================================== */
function AddNodeDialog({ snap, onCancel, onAdd }) {
  const existing = snap.nodes;
  const [remoteId, setRemoteId] = useState(existing.length ? existing[existing.length - 1].id : null);
  const [name, setName] = useState("");
  const isFirst = existing.length === 0;

  return (
    <div className="overlay" onMouseDown={(e) => { if (e.target.classList.contains("overlay")) onCancel(); }}>
      <div className="dialog">
        <div className="dialog-head">
          <span className="eyebrow">{isFirst ? "asp init" : "asp clone"}</span>
          <h3>{isFirst ? "Create the first node" : "Add a node to the mesh"}</h3>
          <p>{isFirst
            ? "Spins up a vault with this device's ed25519 identity and a few seed files."
            : "The new node clones from a remote peer: authenticate, full catch-up, then sync live. Pick which existing node to use as its remote."}</p>
        </div>
        <div className="dialog-body">
          <div className="field">
            <label>node name (optional)</label>
            <input type="text" value={name} placeholder={isFirst ? "laptop" : "auto"} onChange={(e) => setName(e.target.value)} />
          </div>
          {!isFirst && (
            <div className="field">
              <label>remote peer (asp clone &lt;url&gt;)</label>
              <div className="remote-opts">
                {existing.map((nd) => (
                  <div key={nd.id} className={"remote-opt" + (remoteId === nd.id ? " sel" : "")} onClick={() => setRemoteId(nd.id)}>
                    <span className="ro-id" style={{ background: nd.color }}>{nd.name.slice(0, 2).toUpperCase()}</span>
                    <div>
                      <div className="ro-name">{nd.name}</div>
                      <div className="ro-sub">site_id={nd.id} · wss://{nd.name}.local:9000{nd.online ? "" : " · OFFLINE"}</div>
                    </div>
                    <span className="ro-radio"></span>
                  </div>
                ))}
              </div>
            </div>
          )}
        </div>
        <div className="dialog-foot">
          <button className="btn ghost" onClick={onCancel}>Cancel</button>
          <button className="btn primary" onClick={() => onAdd({ name: name.trim() || undefined, remoteId: isFirst ? null : remoteId })}>
            <span className="glyph">+</span>{isFirst ? "Create node" : "Clone node"}
          </button>
        </div>
      </div>
    </div>
  );
}
window.AddNodeDialog = AddNodeDialog;

/* ==================================================================== */
function NodePanel({ snap, api, status, extraClass, onFocus, onRemove }) {
  const [editingName, setEditingName] = useState(false);
  return (
    <div className={"node-panel " + (snap.online ? "" : "offline ") + (extraClass || "")}>
      <div className="np-head">
        <div className="np-head-top">
          <div className="np-id" style={{ background: snap.color }}>{snap.name.slice(0, 2).toUpperCase()}</div>
          <div className="np-name">
            {editingName
              ? <input className="rename-in" autoFocus defaultValue={snap.name}
                  onBlur={(e) => { api.renameNode(snap.id, e.target.value); setEditingName(false); }}
                  onKeyDown={(e) => { if (e.key === "Enter") { api.renameNode(snap.id, e.target.value); setEditingName(false); } if (e.key === "Escape") setEditingName(false); }} />
              : <span className="nm-txt" onDoubleClick={() => setEditingName(true)} title="double-click to rename">{snap.name}</span>}
          </div>
          <StatusPill status={status} />
          <div className="head-actions">
            <button className={"btn tiny " + (snap.online ? "ghost" : "primary")} onClick={() => api.setOnline(snap.id, !snap.online)} title="toggle network link">
              {snap.online ? "Go offline" : "Reconnect"}
            </button>
            {onFocus && <button className="icon-btn" title="focus" onClick={onFocus}>⤢</button>}
            <button className="icon-btn" title="remove node" onClick={() => onRemove(snap.id)}>✕</button>
          </div>
        </div>
        <div className="np-meta">
          <span className="chip"><span className="lbl">site</span><span className="val">{snap.id}</span></span>
          {snap.peers.length > 0
            ? <span className="chip peers"><span className="lbl">peers</span><span className="val">{snap.peers.join(", ")}</span></span>
            : snap.createdRemote
              ? <span className="chip peers"><span className="lbl">cloned</span><span className="val">← {snap.createdRemote}</span></span>
              : <span className="chip peers"><span className="lbl">peers</span><span className="val">none</span></span>}
        </div>
      </div>
      <div className="np-body">
        <FileTree snap={snap} api={api} />
        <Editor snap={snap} api={api} />
      </div>
      <EventLog snap={snap} />
    </div>
  );
}
window.NodePanel = NodePanel;

/* ==================================================================== */
function NodeStrip({ snap, status, selected, onClick }) {
  return (
    <div className={"node-strip" + (selected ? " sel" : "")} onClick={onClick}>
      <div className="s-top">
        <div className="np-id" style={{ background: snap.color, width: 20, height: 20, fontSize: 9 }}>{snap.name.slice(0, 2).toUpperCase()}</div>
        <span className="s-name">{snap.name}</span>
      </div>
      <StatusPill status={status} />
      <div style={{ fontFamily: "var(--mono)", fontSize: 9.5, color: "var(--faint)" }}>{snap.rowCount} rows · {Object.values(snap.files).filter((f) => !f.deleted).length} files</div>
    </div>
  );
}
window.NodeStrip = NodeStrip;
