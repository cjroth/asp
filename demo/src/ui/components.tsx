/* ====================================================================
   components.tsx · presentational pieces for the ASP demo
   Ported from the design prototype (asp-components.jsx). Consumes the
   per-node snapshot produced by the real-engine network (network.ts).
   ==================================================================== */
import React, { useState, useRef, useEffect, useLayoutEffect, useMemo } from 'react';
import { confirmDialog } from './confirm.tsx';

/* ---------- helpers ---------- */
function baseOf(p: string) { const i = p.lastIndexOf('/'); return i < 0 ? p : p.slice(i + 1); }
function dirOf(p: string) { const i = p.lastIndexOf('/'); return i < 0 ? '' : p.slice(0, i); }

/* ---------- inline icons (14px stroke glyphs) ---------- */
const svgProps = { width: 14, height: 14, viewBox: '0 0 24 24', fill: 'none', stroke: 'currentColor', strokeWidth: 2, strokeLinecap: 'round' as const, strokeLinejoin: 'round' as const };
const FolderPlusIcon = () => (
  <svg {...svgProps}><path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z" /><line x1="12" y1="11" x2="12" y2="17" /><line x1="9" y1="14" x2="15" y2="14" /></svg>
);
const RefreshIcon = () => (
  <svg {...svgProps}><path d="M21 12a9 9 0 1 1-2.64-6.36" /><path d="M21 3v6h-6" /></svg>
);
const MaximizeIcon = () => (
  <svg {...svgProps}><path d="M8 3H4a1 1 0 0 0-1 1v4M16 3h4a1 1 0 0 1 1 1v4M8 21H4a1 1 0 0 1-1-1v-4M16 21h4a1 1 0 0 1 1-1v-4" /></svg>
);
const ColumnsIcon = () => (
  <svg {...svgProps}><rect x="3" y="4" width="5" height="16" rx="1" /><rect x="10" y="4" width="5" height="16" rx="1" /><rect x="17" y="4" width="4" height="16" rx="1" /></svg>
);
const CloseIcon = () => (
  <svg {...svgProps}><path d="M6 6l12 12M18 6L6 18" /></svg>
);
const CopyIcon = () => (
  <svg {...svgProps}><rect x="9" y="9" width="11" height="11" rx="2" /><path d="M5 15a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h8a2 2 0 0 1 2 2" /></svg>
);
const LogIcon = () => (
  <svg {...svgProps}><path d="M4 6h16M4 12h16M4 18h10" /></svg>
);
/* chevrons pointing outward = expand-all; inward = collapse-all */
const ExpandAllIcon = () => (
  <svg {...svgProps}><path d="M7 15l5 5 5-5" /><path d="M7 9l5-5 5 5" /></svg>
);
const CollapseAllIcon = () => (
  <svg {...svgProps}><path d="M7 20l5-5 5 5" /><path d="M7 4l5 5 5-5" /></svg>
);

function buildTree(files: any, folders: Set<string>) {
  const root: any = { type: 'dir', name: '', path: '', children: {} };
  function ensureDir(path: string) {
    if (!path) return root;
    const segs = path.split('/');
    let cur = root, acc = '';
    for (const s of segs) {
      acc = acc ? `${acc}/${s}` : s;
      if (!cur.children[s]) cur.children[s] = { type: 'dir', name: s, path: acc, children: {} };
      cur = cur.children[s];
    }
    return cur;
  }
  for (const path of folders) ensureDir(path);
  for (const f of Object.values<any>(files)) {
    if (f.deleted || !f.path) continue;
    const d = ensureDir(dirOf(f.path));
    d.children[baseOf(f.path)] = { type: 'file', name: baseOf(f.path), path: f.path, file: f };
  }
  function sort(node: any) {
    const kids = Object.values<any>(node.children);
    kids.forEach((k) => k.type === 'dir' && sort(k));
    kids.sort((a, b) => (a.type !== b.type ? (a.type === 'dir' ? -1 : 1) : a.name.localeCompare(b.name)));
    node.sorted = kids;
  }
  sort(root);
  return root;
}

/* ==================================================================== */
export function StatusPill({ status }: any) {
  return (
    <span className={`status ${status.kind}`} title={status.note}>
      <span className="led" />{status.label}
    </span>
  );
}

/* ==================================================================== */
/* One tree row. Non-recursive + absolutely positioned at `y`: the FileTree
   flattens the visible tree and virtualizes it, so only the rows in view are
   mounted — a 10k-file vault renders ~40 rows, not 10k. */
function TreeRow({ y, node, depth, snap, collapsed, toggle, api, dragRef, renaming, setRenaming, openMenu, readOnly }: any) {
  const [dragOver, setDragOver] = useState(false);
  const isDir = node.type === 'dir';
  const isOpen = snap.openFileId && node.file && node.file.file_id === snap.openFileId;
  const isCollapsed = isDir && collapsed.has(node.path);
  const dirty = !readOnly && node.file && snap.staged[node.file.file_id] != null && snap.staged[node.file.file_id] !== node.file.content;
  const renamingThis = renaming && renaming.id === (isDir ? `d:${node.path}` : node.file.file_id);

  function onDrop(e: React.DragEvent) {
    e.preventDefault(); e.stopPropagation(); setDragOver(false);
    const fid = dragRef.current;
    if (!fid) return;
    api.moveFile(snap.id, fid, isDir ? node.path : dirOf(node.path));
  }
  function commitRn(val: string) {
    const name = val.trim();
    if (!name) { setRenaming(null); return; }
    const parent = dirOf(node.path);
    const np = (parent ? `${parent}/` : '') + name;
    if (np !== node.path) {
      if (isDir) api.renameFolder(snap.id, node.path, np);
      else api.renameFile(snap.id, node.file.file_id, np);
    }
    setRenaming(null);
  }

  return (
    <div
      className={`trow ${isDir ? 'folder ' : ''}${isOpen ? 'active ' : ''}${dragOver ? 'dragover' : ''}`}
      style={{ position: 'absolute', top: y, left: 0, right: 0, paddingLeft: 8 + depth * 13 }}
      draggable={!readOnly && !isDir && !renamingThis}
      onDragStart={(e) => { dragRef.current = node.file.file_id; e.dataTransfer.effectAllowed = 'move'; }}
      onDragEnd={() => { dragRef.current = null; }}
      onDragOver={(e) => { if (!readOnly && dragRef.current) { e.preventDefault(); setDragOver(true); } }}
      onDragLeave={() => setDragOver(false)}
      onDrop={(e) => { if (!readOnly) onDrop(e); }}
      onContextMenu={(e) => openMenu(e, node)}
      onClick={() => { if (isDir) toggle(node.path); else api.openFile(snap.id, node.file.file_id); }}
    >
      {isDir ? <span className="twist">{isCollapsed ? '▸' : '▾'}</span> : <span className="twist" />}
      {!isDir && <span className="ico">·</span>}
      {renamingThis
        ? <input className="rn-in" autoFocus defaultValue={node.name}
            onClick={(e) => e.stopPropagation()}
            onBlur={(e) => commitRn((e.target as HTMLInputElement).value)}
            onKeyDown={(e) => { if (e.key === 'Enter') commitRn((e.target as HTMLInputElement).value); if (e.key === 'Escape') setRenaming(null); }} />
        : <span className={`nm${dirty ? ' dirty' : ''}`}>{node.name}{isDir ? '/' : ''}</span>}
      {node.file && node.file.collided && <span className="badge-dot" style={{ background: 'var(--amber)' }} title="conflict / path collision — surfaced" />}
    </div>
  );
}

// Flatten the visible (expanded) tree into a linear list — the input to the
// virtualizer. Collapsed folders contribute their own row but not their kids.
function flattenTree(tree: any, collapsed: Set<string>): { node: any; depth: number }[] {
  const out: { node: any; depth: number }[] = [];
  (function walk(node: any, depth: number) {
    for (const c of node.sorted) {
      out.push({ node: c, depth });
      if (c.type === 'dir' && !collapsed.has(c.path)) walk(c, depth + 1);
    }
  })(tree, 0);
  return out;
}

const TREE_ROW_H = 21; // measured: every .trow is a uniform 21px
const TREE_OVERSCAN = 8;

export function FileTree({ snap, api, filesOverride, readOnly }: any) {
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());
  const [renaming, setRenaming] = useState<any>(null);
  const [menu, setMenu] = useState<{ x: number; y: number; node: any } | null>(null);
  const dragRef = useRef<any>(null);
  // `filesOverride` is the historical (time-travel) file map; otherwise live.
  const files = filesOverride || snap.files;
  // Rebuild the tree only when the file map / folder set actually change (a new
  // ref); during a sync most snapshot pushes leave a given node's files alone.
  // History view derives folders from paths only (no live mkdirs).
  const tree = useMemo(() => buildTree(files, filesOverride ? new Set<string>() : snap.folders), [files, filesOverride, snap.folders]);
  const toggle = (path: string) => setCollapsed((s) => { const n = new Set(s); n.has(path) ? n.delete(path) : n.add(path); return n; });
  const liveCount = useMemo(() => Object.values<any>(files).filter((f) => !f.deleted).length, [files]);
  const [rootOver, setRootOver] = useState(false);

  // Every folder path in the tree, for expand-all / collapse-all.
  const allDirs = useMemo(() => {
    const acc: string[] = [];
    (function walk(node: any) { for (const c of node.sorted) if (c.type === 'dir') { acc.push(c.path); walk(c); } })(tree);
    return acc;
  }, [tree]);
  const allExpanded = allDirs.every((d) => !collapsed.has(d));
  const toggleAll = () => setCollapsed(allExpanded ? new Set(allDirs) : new Set());

  // Start every vault collapsed on first load. Runs once per node, after the
  // tree has arrived; later edits won't re-collapse what the user expanded.
  const didAutoCollapse = useRef(false);
  useLayoutEffect(() => {
    if (didAutoCollapse.current) return;
    if (allDirs.length === 0 && liveCount === 0) return; // tree hasn't arrived yet
    didAutoCollapse.current = true;
    if (allDirs.length > 0) setCollapsed(new Set(allDirs));
  }, [allDirs, liveCount]);

  // Virtualize: flatten the visible rows, then mount only the window around the
  // scroll position. Render cost is O(visible) regardless of vault size.
  const flat = useMemo(() => flattenTree(tree, collapsed), [tree, collapsed]);
  const scrollRef = useRef<HTMLDivElement>(null);
  const [scrollTop, setScrollTop] = useState(0);
  const [viewH, setViewH] = useState(480);
  useLayoutEffect(() => {
    const el = scrollRef.current; if (!el) return;
    const ro = new ResizeObserver(() => setViewH(el.clientHeight));
    ro.observe(el); setViewH(el.clientHeight);
    return () => ro.disconnect();
  }, []);
  const total = flat.length;
  const startRow = Math.max(0, Math.floor(scrollTop / TREE_ROW_H) - TREE_OVERSCAN);
  const endRow = Math.min(total, Math.ceil((scrollTop + viewH) / TREE_ROW_H) + TREE_OVERSCAN);
  const visible = flat.slice(startRow, endRow);

  // Close the context menu on any outside interaction.
  useEffect(() => {
    if (!menu) return;
    const close = () => setMenu(null);
    window.addEventListener('click', close);
    window.addEventListener('scroll', close, true);
    window.addEventListener('resize', close);
    return () => { window.removeEventListener('click', close); window.removeEventListener('scroll', close, true); window.removeEventListener('resize', close); };
  }, [menu]);

  function newFile() {
    const name = prompt('New file (path relative to vault root):', 'notes/untitled.md');
    if (name) api.createFile(snap.id, dirOf(name), baseOf(name));
  }
  function newFolder() {
    const name = prompt('New folder (path):', 'archive');
    if (name) api.createFolder(snap.id, dirOf(name), baseOf(name));
  }
  function openMenu(e: React.MouseEvent, node: any) {
    e.preventDefault(); e.stopPropagation();
    setMenu({ x: e.clientX, y: e.clientY, node });
  }
  function menuRename() {
    if (!menu) return;
    const node = menu.node;
    setRenaming({ id: node.type === 'dir' ? `d:${node.path}` : node.file.file_id });
    setMenu(null);
  }
  async function menuDelete() {
    if (!menu) return;
    const node = menu.node;
    setMenu(null);
    if (node.type === 'dir') {
      const ok = await confirmDialog({
        title: `Delete folder “${node.path}/”?`,
        message: <>Everything inside it is tombstoned (remove-wins) and the deletion propagates to every peer.</>,
        confirmLabel: 'Delete folder', danger: true,
      });
      if (ok) api.deleteFolder(snap.id, node.path);
    } else {
      const ok = await confirmDialog({
        title: `Delete “${node.path}”?`,
        message: <>Writes a tombstone row (remove-wins) and propagates the deletion to every peer.</>,
        confirmLabel: 'Delete file', danger: true,
      });
      if (ok) api.deleteFile(snap.id, node.file.file_id);
    }
  }

  const openMenuGated = readOnly ? () => {} : openMenu;
  return (
    <div className="np-tree">
      <div className="tree-head">
        <span className="t">vault · {liveCount}{readOnly ? ' · history' : ''}</span>
        <span className="tools">
          {allDirs.length > 0 && (
            <button className="icon-btn" title={allExpanded ? 'Collapse all' : 'Expand all'} onClick={toggleAll}>
              {allExpanded ? <CollapseAllIcon /> : <ExpandAllIcon />}
            </button>
          )}
          {!readOnly && <button className="icon-btn" title="New file" onClick={newFile}>＋</button>}
          {!readOnly && <button className="icon-btn" title="New folder" onClick={newFolder}><FolderPlusIcon /></button>}
        </span>
      </div>
      <div className={`tree-scroll${rootOver ? ' dragover' : ''}`} ref={scrollRef}
        onScroll={(e) => setScrollTop(e.currentTarget.scrollTop)}
        onDragOver={(e) => { if (!readOnly && dragRef.current) { e.preventDefault(); setRootOver(true); } }}
        onDragLeave={() => setRootOver(false)}
        onDrop={(e) => { if (readOnly) return; e.preventDefault(); setRootOver(false); if (dragRef.current) api.moveFile(snap.id, dragRef.current, ''); }}>
        {total === 0
          ? <div className="tree-empty">{readOnly ? 'no files existed at this point yet.' : <>empty vault.<br />press ＋ to create a file.</>}</div>
          : (
            <div style={{ height: total * TREE_ROW_H, position: 'relative' }}>
              {visible.map(({ node, depth }, i) => (
                <TreeRow key={node.path + node.type} y={(startRow + i) * TREE_ROW_H} node={node} depth={depth}
                  snap={snap} collapsed={collapsed} toggle={toggle} api={api} dragRef={dragRef} renaming={renaming} setRenaming={setRenaming} openMenu={openMenuGated} readOnly={readOnly} />
              ))}
            </div>
          )}
      </div>
      {menu && (
        <div className="ctx-menu" style={{ left: menu.x, top: menu.y }} onClick={(e) => e.stopPropagation()} onContextMenu={(e) => e.preventDefault()}>
          <button onClick={menuRename}>Rename</button>
          <button className="danger" onClick={menuDelete}>Delete</button>
        </div>
      )}
    </div>
  );
}

/* ==================================================================== */
export function Editor({ snap, api, readOnly, override }: any) {
  const f = snap.openFileId ? snap.files[snap.openFileId] : null;
  const fileId = f ? f.file_id : null;
  // The engine's authoritative text for the open file (staged overlay, else the
  // committed content).
  const engineVal = f ? (snap.staged[f.file_id] != null ? snap.staged[f.file_id] : f.content) : '';

  // Local buffer so keystrokes are instant. The engine runs in a worker, so
  // api.stageEdit round-trips asynchronously — a controlled <textarea> bound
  // straight to the round-tripped value drops characters and jumps the caret
  // under fast typing. We type into local state and inform the engine as a
  // side-effect, adopting the engine value only when the file switches or a
  // *remote* edit lands (so live sync still flows into an open editor).
  const [text, setText] = useState(engineVal);
  const pending = useRef<string | null>(null); // our last edit, until the engine echoes it back
  const lastFile = useRef<string | null>(fileId);
  useEffect(() => {
    if (fileId !== lastFile.current) {            // switched files → adopt its value
      lastFile.current = fileId;
      pending.current = null;
      setText(engineVal);
      return;
    }
    if (pending.current != null) {
      if (engineVal === pending.current) pending.current = null; // our edit echoed → synced
      return;                                     // else: still in flight; ignore stale renders
    }
    if (engineVal !== text) setText(engineVal);    // synced + engine moved → a remote edit; adopt it
  }, [fileId, engineVal]); // eslint-disable-line react-hooks/exhaustive-deps

  // Time-travel: when scrubbed back, show the open file's content as of that
  // point (read-only, "detached"); the live engine state is untouched. Runs
  // AFTER the hooks above so hook order stays stable.
  if (readOnly) {
    if (!f || !override || !override.found) {
      return (
        <div className="np-editor detached">
          <div className="ed-tab"><span className="path">{f ? f.path : 'history'}</span><span className="cls ro">read-only</span></div>
          <div className="ed-empty">this file did not exist at this point in history.</div>
        </div>
      );
    }
    if (override.deleted) {
      return (
        <div className="np-editor detached">
          <div className="ed-tab"><span className="path">{f.path}</span><span className="cls ro">read-only</span></div>
          <div className="ed-empty">file was deleted at this point (tombstone).</div>
        </div>
      );
    }
    const hist = override.content ?? '';
    return (
      <div className="np-editor detached">
        <div className="ed-tab">
          <span className="path">{f.path}</span>
          <span className="cls ro">read-only · history</span>
          <span className="spacer" />
        </div>
        <div className="ed-area">
          <textarea spellCheck={false} value={hist} readOnly />
        </div>
        <div className="ed-foot">
          <span>detached · drag the handle to the end to go live</span>
          <span className="spacer" />
          <span>{hist.split('\n').length} lines · {hist.length} B</span>
        </div>
      </div>
    );
  }

  const value = f ? text : '';
  const dirty = !!f && f.content !== value;

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
        <span className={`cls ${f.merge_class}`}>{f.merge_class}</span>
        <span className="spacer" />
        <span className="hash">@{f.result_hash ? f.result_hash.slice(0, 4) : '∅'}</span>
      </div>
      <div className="ed-area">
        <textarea spellCheck={false} value={value}
          placeholder="// empty file — type to edit, auto-commits on debounce"
          onChange={(e) => { const v = e.target.value; setText(v); pending.current = v; api.stageEdit(snap.id, f.file_id, v); }} />
      </div>
      <div className="ed-foot">
        <span>file_id={f.file_id.slice(0, 8)}</span>
        <span className="spacer" />
        {dirty
          ? <span className="commit-note"><span className="led" />staged · auto-commit…</span>
          : <span>{value.split('\n').length} lines · {value.length} B</span>}
      </div>
    </div>
  );
}

/* ==================================================================== */
function lineText(l: any) { return `${l.ts}  ${l.parts.map((p: any) => p.t).join('')}`; }
function copyText(text: string) {
  try { navigator.clipboard?.writeText(text); }
  catch { /* clipboard unavailable */ }
}

export function EventLog({ snap, onClose }: any) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const atBottom = useRef(true);
  const [menu, setMenu] = useState<{ x: number; y: number; line: any } | null>(null);
  useLayoutEffect(() => {
    const el = scrollRef.current;
    if (el && atBottom.current) el.scrollTop = el.scrollHeight;
  });
  useEffect(() => {
    if (!menu) return;
    const close = () => setMenu(null);
    window.addEventListener('click', close);
    window.addEventListener('scroll', close, true);
    return () => { window.removeEventListener('click', close); window.removeEventListener('scroll', close, true); };
  }, [menu]);
  function onScroll(e: React.UIEvent) {
    const el = e.target as HTMLElement;
    atBottom.current = el.scrollHeight - el.scrollTop - el.clientHeight < 24;
  }
  const allText = () => snap.lines.map(lineText).join('\n');
  return (
    <div className="np-log" style={{ ['--logh' as any]: '152px' }}>
      <div className="log-head">
        <span className="t">event log</span>
        <span className="spacer" />
        <span className="count">{snap.rowCount} rows · {snap.lines.length} lines</span>
        <button className="icon-btn sm" title="Copy entire log" onClick={() => copyText(allText())}><CopyIcon /></button>
        {onClose && <button className="icon-btn sm" title="Hide log" onClick={onClose}><CloseIcon /></button>}
      </div>
      <div className="log-scroll" ref={scrollRef} onScroll={onScroll}>
        {snap.lines.map((l: any) => (
          <span key={l.id} className={`log-line${l.fresh ? ' fresh' : ''}`}
            onContextMenu={(e) => { e.preventDefault(); e.stopPropagation(); setMenu({ x: e.clientX, y: e.clientY, line: l }); }}>
            <span className="ts">{l.ts}</span>{'  '}
            {l.parts.map((p: any, i: number) => <span key={i} className={p.c}>{p.t}</span>)}
          </span>
        ))}
      </div>
      {menu && (
        <div className="ctx-menu" style={{ left: menu.x, top: menu.y }} onClick={(e) => e.stopPropagation()} onContextMenu={(e) => e.preventDefault()}>
          <button onClick={() => { copyText(allText()); setMenu(null); }}>Copy all</button>
          <button onClick={() => { copyText(String(window.getSelection?.() ?? '')); setMenu(null); }}>Copy selected</button>
          <button onClick={() => { copyText(lineText(menu.line)); setMenu(null); }}>Copy row</button>
        </div>
      )}
    </div>
  );
}

/* ==================================================================== */
/* Change timeline — one marker per file-change row, positioned by wall-clock
   time (staggered), sized by change magnitude, coloured by the authoring
   device. Zoomable + horizontally scrollable. A playhead handle scrubs back:
   drop it anywhere and it floor-snaps to the change immediately to its left. */
export function Timeline({ events, fracs, scrubFrac, setScrubFrac, onPick }: any) {
  const viewportRef = useRef<HTMLDivElement>(null);
  const innerRef = useRef<HTMLDivElement>(null);
  const [vw, setVw] = useState(600);
  const [zoom, setZoom] = useState(1);
  const n = events.length;

  useLayoutEffect(() => {
    const el = viewportRef.current; if (!el) return;
    const ro = new ResizeObserver(() => setVw(el.clientWidth));
    ro.observe(el); setVw(el.clientWidth);
    return () => ro.disconnect();
  }, []);

  const contentW = vw * zoom;
  const handleFrac = scrubFrac == null ? 1 : scrubFrac;
  const shown = scrubFrac == null ? n : fracs.filter((f: number) => f <= scrubFrac + 1e-9).length;

  function magOf(e: any) {
    const b = (e.before || '').length, a = (e.after || '').length;
    const m = e.kind === 'create' ? a : e.kind === 'delete' ? b : Math.abs(a - b);
    return Math.round(Math.min(16, 7 + Math.sqrt(m) * 0.85));
  }
  function setFromX(clientX: number) {
    const el = innerRef.current; if (!el || n === 0) return;
    const rect = el.getBoundingClientRect();
    const f = Math.min(1, Math.max(0, (clientX - rect.left) / rect.width));
    setScrubFrac(f >= 0.999 ? null : f);
  }
  function startDrag(e: React.PointerEvent) {
    if (n === 0) return;
    e.preventDefault();
    setFromX(e.clientX);
    const move = (ev: PointerEvent) => setFromX(ev.clientX);
    const up = () => { window.removeEventListener('pointermove', move); window.removeEventListener('pointerup', up); };
    window.addEventListener('pointermove', move);
    window.addEventListener('pointerup', up);
  }

  return (
    <div className="np-timeline">
      <span className="tl-label">history</span>
      <div className="tl-scroll" ref={viewportRef}>
        <div className="tl-inner" ref={innerRef} style={{ width: contentW }} onPointerDown={startDrag}>
          <div className="tl-rail" />
          <div className="tl-fill" style={{ width: handleFrac * contentW }} />
          {events.map((e: any, i: number) => {
            const s = magOf(e);
            return (
              <button key={e.rowId} className={`tl-tick k-${e.kind}${fracs[i] > handleFrac + 1e-9 ? ' future' : ''}`}
                style={{ left: fracs[i] * contentW, width: s, height: s, ['--tc' as any]: e.ownerColor }}
                title={`${e.kind} ${baseOf(e.path)} · ${e.ownerName}`}
                onPointerDown={(ev) => ev.stopPropagation()}
                onClick={(ev) => { ev.stopPropagation(); onPick(e); }} />
            );
          })}
          <div className="tl-handle" style={{ left: handleFrac * contentW }} onPointerDown={(ev) => { ev.stopPropagation(); startDrag(ev); }} title="drag to scrub history" />
        </div>
      </div>
      <div className="tl-zoom">
        <button className="icon-btn sm" title="zoom out" disabled={zoom <= 1} onClick={() => setZoom((z) => Math.max(1, +(z / 1.6).toFixed(3)))}>−</button>
        <button className="icon-btn sm" title="zoom in" disabled={zoom >= 24} onClick={() => setZoom((z) => Math.min(24, +(z * 1.6).toFixed(3)))}>+</button>
      </div>
      <span className="tl-count">{shown}/{n}</span>
    </div>
  );
}

/* line-level diff: mark which lines survive (LCS); the rest are del/add */
function diffFlags(a: string[], b: string[]) {
  const n = a.length, m = b.length;
  if (n > 800 || m > 800) return { aKeep: a.map(() => false), bKeep: b.map(() => false) }; // guard
  const dp: number[][] = Array.from({ length: n + 1 }, () => new Array(m + 1).fill(0));
  for (let i = n - 1; i >= 0; i--) for (let j = m - 1; j >= 0; j--)
    dp[i][j] = a[i] === b[j] ? dp[i + 1][j + 1] + 1 : Math.max(dp[i + 1][j], dp[i][j + 1]);
  const aKeep = new Array(n).fill(false), bKeep = new Array(m).fill(false);
  let i = 0, j = 0;
  while (i < n && j < m) {
    if (a[i] === b[j]) { aKeep[i] = bKeep[j] = true; i++; j++; }
    else if (dp[i + 1][j] >= dp[i][j + 1]) i++; else j++;
  }
  return { aKeep, bKeep };
}

export function DiffModal({ event, onClose }: any) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => { if (e.key === 'Escape') onClose(); };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [onClose]);
  const a = (event.before ?? '').split('\n');
  const b = (event.after ?? '').split('\n');
  const { aKeep, bKeep } = diffFlags(a, b);
  const empty = event.before === '' && a.length === 1;
  const emptyB = event.after === '' && b.length === 1;
  const adds = bKeep.filter((k: boolean) => !k).length;
  const dels = aKeep.filter((k: boolean) => !k).length;
  return (
    <div className="overlay diff-overlay" onMouseDown={(e) => { if ((e.target as HTMLElement).classList.contains('diff-overlay')) onClose(); }}>
      <div className="diff-modal">
        <div className="diff-head">
          <span className={`cls k ${event.kind}`}>{event.kind}</span>
          <span className="diff-path">{event.path}</span>
          <span className="diff-by"><span className="dot" style={{ background: event.ownerColor }} />{event.ownerName}</span>
          <span className="spacer" />
          <span className="diff-stat"><span className="add">+{adds}</span> <span className="del">−{dels}</span></span>
          <button className="icon-btn" title="close (Esc)" onClick={onClose}><CloseIcon /></button>
        </div>
        <div className="diff-body">
          <div className="diff-col">
            <div className="diff-col-h">before {event.before ? '' : '· (none)'}</div>
            <div className="diff-lines">
              {empty ? <div className="diff-line none">∅ new file</div> : a.map((ln: string, i: number) => (
                <div key={i} className={`diff-line${aKeep[i] ? '' : ' del'}`}><span className="ln">{i + 1}</span><span className="tx">{ln || ' '}</span></div>
              ))}
            </div>
          </div>
          <div className="diff-col">
            <div className="diff-col-h">after {event.after ? '' : '· (deleted)'}</div>
            <div className="diff-lines">
              {emptyB ? <div className="diff-line none">∅ tombstoned</div> : b.map((ln: string, i: number) => (
                <div key={i} className={`diff-line${bKeep[i] ? '' : ' add'}`}><span className="ln">{i + 1}</span><span className="tx">{ln || ' '}</span></div>
              ))}
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}

/* ==================================================================== */
/* Network map — spatial p2p view with animated packets               */
export function NetworkMap({ snap, packetsRef, selected, onSelect, statusFor }: any) {
  const wrapRef = useRef<HTMLDivElement>(null);
  const [w, setW] = useState(900);
  const [, force] = useState(0);
  const H = snap.nodes.length ? 150 : 0;

  useLayoutEffect(() => {
    const el = wrapRef.current; if (!el) return;
    const ro = new ResizeObserver(() => setW(el.clientWidth));
    ro.observe(el); setW(el.clientWidth);
    return () => ro.disconnect();
  }, []);

  useEffect(() => {
    let raf: number;
    let wasAnimating = false;
    const loop = () => {
      const now = performance.now();
      const arr = packetsRef.current;
      for (let i = arr.length - 1; i >= 0; i--) if (now - arr[i].started > arr[i].dur + 60) arr.splice(i, 1);
      // Only re-render while packets are in flight (plus one final frame to clear
      // them). Otherwise the map re-rendered every frame forever — constant
      // main-thread work that made the whole UI feel laggy even while idle.
      const animating = arr.length > 0;
      if (animating || wasAnimating) force((x) => x + 1);
      wasAnimating = animating;
      raf = requestAnimationFrame(loop);
    };
    raf = requestAnimationFrame(loop);
    return () => cancelAnimationFrame(raf);
  }, []);

  const n = snap.nodes.length;
  const margin = 90, top = 58;
  const usable = Math.max(1, w - margin * 2);
  const pos: any = {};
  snap.nodes.forEach((nd: any, i: number) => {
    const x = n === 1 ? w / 2 : margin + (usable * i) / (n - 1);
    const y = top + (i % 2 === 0 ? 0 : 14);
    pos[nd.id] = { x, y };
  });

  const colorFor = (st: any) => (({ insync: 'var(--green)', syncing: 'var(--cyan)', offline: 'var(--red)', solo: 'var(--muted)' } as any)[st.kind] || 'var(--muted)');
  const now = performance.now();

  return (
    <div className="netmap" ref={wrapRef} style={{ height: H }}>
      <span className="map-label">network · p2p mesh</span>
      <svg height={H} width={w} viewBox={`0 0 ${w} ${H}`}>
        {snap.edges.map((e: any, i: number) => {
          const a = pos[e.a], b = pos[e.b];
          if (!a || !b) return null;
          const na = snap.nodes.find((x: any) => x.id === e.a), nb = snap.nodes.find((x: any) => x.id === e.b);
          const active = na && nb && na.online && nb.online;
          return <line key={i} className={`edge-line${active ? ' active' : ''}`} x1={a.x} y1={a.y} x2={b.x} y2={b.y} strokeDasharray={active ? 'none' : '4 4'} />;
        })}
        {packetsRef.current.map((pk: any) => {
          const a = pos[pk.fromId], b = pos[pk.toId];
          if (!a || !b) return null;
          const t = Math.min(1, (now - pk.started) / pk.dur);
          const x = a.x + (b.x - a.x) * t, y = a.y + (b.y - a.y) * t;
          const col = pk.kind === 'catchup' ? 'var(--amber)' : 'var(--cyan)';
          return <circle key={pk.id} className="packet" cx={x} cy={y} r={pk.kind === 'catchup' ? 4.5 : 3.5} fill={col} style={{ filter: `drop-shadow(0 0 5px ${col})` }} />;
        })}
        {snap.nodes.map((nd: any) => {
          const p = pos[nd.id]; if (!p) return null;
          const st = statusFor(nd.id);
          const ring = colorFor(st);
          const sel = selected === nd.id;
          return (
            <g key={nd.id} className="node-dot-g" onClick={() => onSelect(nd.id)}>
              {st.kind === 'syncing' && <circle cx={p.x} cy={p.y} r="20" fill="none" stroke={ring} strokeWidth="1.5" opacity="0.35">
                <animate attributeName="r" from="13" to="24" dur="1.1s" repeatCount="indefinite" />
                <animate attributeName="opacity" from="0.5" to="0" dur="1.1s" repeatCount="indefinite" />
              </circle>}
              <circle className={`node-dot${sel ? ' sel' : ''}`} cx={p.x} cy={p.y} r="13" fill={nd.online ? 'var(--panel-2)' : 'var(--ink-2)'} stroke={sel ? 'var(--cyan)' : ring} strokeWidth={sel ? 2.5 : 2} />
              <circle cx={p.x} cy={p.y} r="4.5" fill={ring} opacity={nd.online ? 1 : 0.5} />
              <text className="node-label" x={p.x} y={p.y - 22}>{nd.name}</text>
              <text className="node-sub" x={p.x} y={p.y + 30}>{st.label}{st.note && st.kind === 'offline' ? ` · ${st.note}` : ''}</text>
            </g>
          );
        })}
      </svg>
    </div>
  );
}

/* ==================================================================== */
export function AddNodeDialog({ snap, onCancel, onAdd }: any) {
  const existing = snap.nodes;
  const isFirst = existing.length === 0;
  const [remoteId, setRemoteId] = useState<string | null>(existing.length ? existing[existing.length - 1].id : null);
  const [name, setName] = useState('');
  const [source, setSource] = useState<'local' | 'external'>('local');
  const [url, setUrl] = useState('wss://127.0.0.1:9000');
  const [authKey, setAuthKey] = useState('');
  const external = source === 'external';

  function submit() {
    const base: any = { name: name.trim() || undefined };
    if (external) onAdd({ ...base, externalUrl: url.trim(), authKey: authKey.trim() || undefined });
    else onAdd({ ...base, remoteId: isFirst ? null : remoteId });
  }

  return (
    <div className="overlay" onMouseDown={(e) => { if ((e.target as HTMLElement).classList.contains('overlay')) onCancel(); }}>
      <div className="dialog">
        <div className="dialog-head">
          <span className="eyebrow">{external ? 'asp clone <url>' : isFirst ? 'asp init' : 'asp clone'}</span>
          <h3>{isFirst ? 'Create the first node' : 'Add a node to the mesh'}</h3>
          <p>{external
            ? 'Clone from a real `asp watch --listen` peer (CLI / Obsidian / Desktop) over wss://. Genuine ed25519 handshake + version-vector catch-up — the live interop bridge.'
            : isFirst
              ? "Spins up a vault with this device's ed25519 identity and a few seed files."
              : 'The new node clones from a remote peer: authenticate, full catch-up, then sync live. Pick which existing node to use as its remote.'}</p>
        </div>
        <div className="dialog-body">
          <div className="field">
            <label>source</label>
            <div className="seg" style={{ maxWidth: 240 }}>
              <button className={!external ? 'on' : ''} onClick={() => setSource('local')}>{isFirst ? 'new vault' : 'in-page peer'}</button>
              <button className={external ? 'on' : ''} onClick={() => setSource('external')}>real wss:// peer</button>
            </div>
          </div>
          <div className="field">
            <label>node name (optional)</label>
            <input type="text" value={name} placeholder={isFirst ? 'laptop' : 'auto'} onChange={(e) => setName(e.target.value)} />
          </div>
          {external ? (
            <>
              <div className="field">
                <label>peer url (wss://)</label>
                <input type="text" value={url} placeholder="wss://127.0.0.1:9000" onChange={(e) => setUrl(e.target.value)} />
              </div>
              <div className="field">
                <label>auth key (AUTH_KEY enrollment secret)</label>
                <input type="text" value={authKey} placeholder="optional once enrolled" onChange={(e) => setAuthKey(e.target.value)} />
              </div>
            </>
          ) : !isFirst ? (
            <div className="field">
              <label>remote peer (asp clone &lt;url&gt;)</label>
              <div className="remote-opts">
                {existing.map((nd: any) => (
                  <div key={nd.id} className={`remote-opt${remoteId === nd.id ? ' sel' : ''}`} onClick={() => setRemoteId(nd.id)}>
                    <span className="ro-id" style={{ background: nd.color }}>{nd.name.slice(0, 2).toUpperCase()}</span>
                    <div>
                      <div className="ro-name">{nd.name}</div>
                      <div className="ro-sub">site_id={nd.site} · wss://{nd.name}.local:9000{nd.online ? '' : ' · OFFLINE'}</div>
                    </div>
                    <span className="ro-radio" />
                  </div>
                ))}
              </div>
            </div>
          ) : null}
        </div>
        <div className="dialog-foot">
          <button className="btn ghost" onClick={onCancel}>Cancel</button>
          <button className="btn primary" onClick={submit} disabled={external && !url.trim()}>
            <span className="glyph">+</span>{external ? 'Clone over wss://' : isFirst ? 'Create node' : 'Clone node'}
          </button>
        </div>
      </div>
    </div>
  );
}

/* ==================================================================== */
export function ConnectPeerDialog({ snap, onCancel, onConnect, onDisconnect }: any) {
  const [url, setUrl] = useState(snap.externalUrl || 'wss://127.0.0.1:9000');
  const [authKey, setAuthKey] = useState('');
  return (
    <div className="overlay" onMouseDown={(e) => { if ((e.target as HTMLElement).classList.contains('overlay')) onCancel(); }}>
      <div className="dialog">
        <div className="dialog-head">
          <span className="eyebrow">asp watch --peer &lt;url&gt;</span>
          <h3>{snap.live ? `“${snap.name}” is live` : `Bridge “${snap.name}” to a real peer`}</h3>
          <p>Open a <b>live</b> connection to an <code>asp watch --listen</code> node (CLI / Obsidian / Desktop):
            ed25519 handshake + version-vector catch-up, then the socket stays open — edits propagate
            <b> both ways in real time</b>, and through the in-page mesh too.</p>
        </div>
        <div className="dialog-body">
          <div className="field">
            <label>peer url (wss://)</label>
            <input type="text" value={url} autoFocus placeholder="wss://127.0.0.1:9000" onChange={(e) => setUrl(e.target.value)} />
          </div>
          <div className="field">
            <label>auth key (optional once enrolled)</label>
            <input type="text" value={authKey} placeholder="AUTH_KEY enrollment secret" onChange={(e) => setAuthKey(e.target.value)} />
          </div>
        </div>
        <div className="dialog-foot">
          {snap.live && <button className="btn ghost" onClick={() => { onDisconnect(snap.id); onCancel(); }}>Disconnect</button>}
          <button className="btn ghost" onClick={onCancel}>Cancel</button>
          <button className="btn primary" onClick={() => onConnect(url.trim(), authKey.trim() || undefined)} disabled={!url.trim()}>
            <span className="glyph">⇄</span>{snap.live ? 'Reconnect' : 'Watch peer'}
          </button>
        </div>
      </div>
    </div>
  );
}

/* ==================================================================== */
export function NodePanel({ snap, api, status, extraClass, onMaximize, onColumns, onRemove, onConnect }: any) {
  const [editingName, setEditingName] = useState(false);
  const [treeW, setTreeW] = useState(166);
  const [logOpen, setLogOpen] = useState(true);
  const [scrubFrac, setScrubFrac] = useState<number | null>(null); // null = live (head)
  const [diffEvent, setDiffEvent] = useState<any>(null);
  const [events, setEvents] = useState<any[]>([]);
  const bodyRef = useRef<HTMLDivElement>(null);
  const host = (u: string) => { try { return new URL(u).host; } catch { return u; } };

  // The engine lives in a worker, so history is fetched async (not shipped in
  // every snapshot — that would undo the diet). Refetch when this node's rows
  // grow; reset the scrubber to live when the node changes.
  useEffect(() => { setScrubFrac(null); }, [snap.id]);
  useEffect(() => {
    let alive = true;
    Promise.resolve(api.history(snap.id)).then((h: any[]) => { if (alive) setEvents(h || []); });
    return () => { alive = false; };
  }, [api, snap.id, snap.rowCount]);

  // Position each change along a 0..1 axis by wall-clock time (staggered),
  // falling back to even spacing when every row shares a timestamp.
  const fracs = useMemo(() => {
    if (events.length === 0) return [] as number[];
    const ts = events.map((e: any) => e.ts);
    const min = Math.min(...ts), max = Math.max(...ts), span = max - min;
    return events.map((e: any, i: number) => (span > 0 ? (e.ts - min) / span : events.length <= 1 ? 1 : i / (events.length - 1)));
  }, [events]);

  const shown = scrubFrac == null ? events.length : fracs.filter((f) => f <= scrubFrac + 1e-9).length;
  const detached = scrubFrac != null && shown < events.length;

  // The vault as of the scrubbed point: every change at-or-before the playhead,
  // applied in order (read-only time-travel; the live engine is untouched).
  const historyFiles = useMemo(() => {
    if (!detached) return undefined;
    const map: Record<string, any> = {};
    events.forEach((e: any, i: number) => {
      if (fracs[i] > (scrubFrac as number) + 1e-9) return;
      if (e.kind === 'delete') { if (map[e.fileId]) map[e.fileId].deleted = true; }
      else map[e.fileId] = { file_id: e.fileId, path: e.path, content: e.after, deleted: false, merge_class: '', result_hash: null, collided: false };
    });
    return map;
  }, [detached, scrubFrac, events, fracs]);

  const override = useMemo(() => {
    if (!detached) return null;
    const hf = snap.openFileId && historyFiles ? historyFiles[snap.openFileId] : null;
    return { found: !!hf && !hf.deleted, deleted: !!hf?.deleted, content: hf?.content ?? '' };
  }, [detached, historyFiles, snap.openFileId]);

  function startResize(e: React.PointerEvent) {
    e.preventDefault();
    const onMove = (ev: PointerEvent) => {
      const rect = bodyRef.current?.getBoundingClientRect();
      if (!rect) return;
      setTreeW(Math.min(Math.max(ev.clientX - rect.left, 110), rect.width - 180));
    };
    const onUp = () => {
      window.removeEventListener('pointermove', onMove);
      window.removeEventListener('pointerup', onUp);
      document.body.classList.remove('col-resizing');
    };
    window.addEventListener('pointermove', onMove);
    window.addEventListener('pointerup', onUp);
    document.body.classList.add('col-resizing');
  }

  return (
    <div className={`node-panel ${snap.online ? '' : 'offline '}${detached ? 'detached ' : ''}${extraClass || ''}`}>
      <div className="np-head">
        <div className="np-head-top">
          <div className="np-id" style={{ background: snap.color }}>{snap.name.slice(0, 2).toUpperCase()}</div>
          <div className="np-name">
            {editingName
              ? <input className="rename-in" autoFocus defaultValue={snap.name}
                  onBlur={(e) => { api.renameNode(snap.id, (e.target as HTMLInputElement).value); setEditingName(false); }}
                  onKeyDown={(e) => { if (e.key === 'Enter') { api.renameNode(snap.id, (e.target as HTMLInputElement).value); setEditingName(false); } if (e.key === 'Escape') setEditingName(false); }} />
              : <span className="nm-txt" onDoubleClick={() => setEditingName(true)} title="double-click to rename">{snap.name}</span>}
          </div>
          <div className="head-actions">
            {onMaximize && <button className="icon-btn" title="maximize" onClick={() => onMaximize(snap.id)}><MaximizeIcon /></button>}
            {onColumns && <button className="icon-btn" title="column view" onClick={onColumns}><ColumnsIcon /></button>}
            {onConnect && <button className="icon-btn" title={snap.externalUrl ? `re-sync ${host(snap.externalUrl)}` : 'connect to a real wss:// peer'} onClick={() => onConnect(snap.id)}><RefreshIcon /></button>}
            {onRemove && <button className="icon-btn" title="remove node" onClick={() => onRemove(snap.id)}>✕</button>}
          </div>
        </div>
        <div className="np-meta">
          <span className="chip"><span className="lbl">site</span><span className="val">{snap.site}</span></span>
          {snap.peers.length > 0
            ? <span className="chip peers"><span className="lbl">peers</span><span className="val">{snap.peers.join(', ')}</span></span>
            : snap.externalUrl
              ? <span className="chip peers"><span className="lbl">ws</span><span className="val">{host(snap.externalUrl)}</span></span>
              : snap.createdRemote
                ? <span className="chip peers"><span className="lbl">cloned</span><span className="val">← {snap.createdRemote}</span></span>
                : <span className="chip peers"><span className="lbl">peers</span><span className="val">none</span></span>}
        </div>
      </div>
      <div className="np-body" ref={bodyRef} style={{ gridTemplateColumns: `minmax(0,${treeW}px) 1px minmax(0,1fr)` }}>
        <FileTree snap={snap} api={api} filesOverride={historyFiles} readOnly={detached} />
        <div className="np-divider" onPointerDown={startResize} title="drag to resize" />
        <Editor snap={snap} api={api} readOnly={detached} override={override} />
      </div>
      {events.length > 0 && <Timeline events={events} fracs={fracs} scrubFrac={scrubFrac} setScrubFrac={setScrubFrac} onPick={setDiffEvent} />}
      {logOpen && <EventLog snap={snap} onClose={() => setLogOpen(false)} />}
      <div className="np-status">
        <button className={`set-toggle${snap.online ? ' on' : ''}`} role="switch" aria-checked={snap.online}
          title={snap.online ? 'online — click to go offline' : 'offline — click to reconnect'}
          onClick={() => api.setOnline(snap.id, !snap.online)}><i /></button>
        <span className="np-net">{snap.online ? 'online' : 'offline'}</span>
        <StatusPill status={status} />
        {!logOpen && <button className="btn tiny ghost log-toggle" onClick={() => setLogOpen(true)} title="show event log"><LogIcon />log</button>}
        {logOpen && <button className="icon-btn sm" title="hide event log" onClick={() => setLogOpen(false)}><LogIcon /></button>}
        {detached && <button className="btn tiny ghost go-live" onClick={() => setScrubFrac(null)} title="return to the live head"><span className="dot" />live</button>}
        <span className="spacer" />
      </div>
      {diffEvent && <DiffModal event={diffEvent} onClose={() => setDiffEvent(null)} />}
    </div>
  );
}

/* ==================================================================== */
/* Maximize modal — the node panel almost full-screen, overlaid          */
export function MaxModal({ snap, api, status, onClose, onConnect }: any) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => { if (e.key === 'Escape') onClose(); };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [onClose]);
  return (
    <div className="overlay np-max-overlay" onMouseDown={(e) => { if ((e.target as HTMLElement).classList.contains('np-max-overlay')) onClose(); }}>
      <div className="np-max">
        <button className="np-max-close icon-btn" title="close (Esc)" onClick={onClose}><CloseIcon /></button>
        <NodePanel snap={snap} api={api} status={status} extraClass="is-max" onConnect={onConnect} />
      </div>
    </div>
  );
}

/* ==================================================================== */
export function NodeStrip({ snap, status, selected, onClick }: any) {
  return (
    <div className={`node-strip${selected ? ' sel' : ''}`} onClick={onClick}>
      <div className="s-top">
        <div className="np-id" style={{ background: snap.color, width: 20, height: 20, fontSize: 9 }}>{snap.name.slice(0, 2).toUpperCase()}</div>
        <span className="s-name">{snap.name}</span>
      </div>
      <StatusPill status={status} />
      <div style={{ fontFamily: 'var(--mono)', fontSize: 9.5, color: 'var(--faint)' }}>{snap.rowCount} rows · {Object.values<any>(snap.files).filter((f) => !f.deleted).length} files</div>
    </div>
  );
}
