// The Vault Editor — a real Markdown editor over the asp engine. Ports the dc
// mockup's UI + wysiwyg markdown + history timeline to live engine data (via
// the VaultApi abstraction). Works on desktop (Tauri → asp-desktop-engine) and
// web (wasm + OPFS) — the api is selected at mount.
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { FONT_CSS } from '../fonts/fonts';
import { mdToHtml, renderLiveHtml } from './lib/markdown';
import type { VaultApi } from './lib/api';
import type { FileAtTime, HistoryEvent, TreeNode, VaultInfo } from './lib/types';

const DAY = 86400000;
const HOUR = 3600000;
const MIN = 60000;
const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];
const pad = (x: number) => (x < 10 ? '0' : '') + x;
const fmtFull = (ts: number) => {
  const d = new Date(ts);
  return `${MONTHS[d.getMonth()]} ${d.getDate()}, ${pad(d.getHours())}:${pad(d.getMinutes())}`;
};
const fmtTick = (ts: number, step: number) => {
  const d = new Date(ts);
  return step >= DAY ? `${MONTHS[d.getMonth()]} ${d.getDate()}` : `${pad(d.getHours())}:${pad(d.getMinutes())}`;
};

function hashStr(s: string): number {
  let h = 5381;
  for (let i = 0; i < s.length; i++) h = ((h << 5) + h + s.charCodeAt(i)) >>> 0;
  return h;
}

interface SavedVaultMeta {
  id: string;
  name: string;
  hue: number;
  kind: 'folder' | 'browser';
  path: string | null;
  lastSync: string;
}

export default function App({ api }: { api: VaultApi }) {
  const [vaults, setVaults] = useState<VaultInfo[]>([]);
  const [identity, setIdentity] = useState('');
  const [screen, setScreen] = useState<'connect' | 'editor'>('connect');
  const [activeId, setActiveId] = useState<string | null>(null);
  const [expanded, setExpanded] = useState<Record<string, boolean>>({});
  const [selectedPath, setSelectedPath] = useState<string | null>(null);
  const [content, setContent] = useState<string>('');
  const [tree, setTree] = useState<TreeNode[]>([]);
  const [saving, setSaving] = useState(false);
  const [ticket, setTicket] = useState('');
  const [authKey, setAuthKey] = useState('');
  const [connecting, setConnecting] = useState(false);
  const [share, setShare] = useState<{ code: string; requireKey: boolean; accessKey: string; copied: boolean } | null>(null);
  const [removeModal, setRemoveModal] = useState<{ id: string; name: string; path: string | null; kind: string; trash: boolean } | null>(null);
  const [ctxMenu, setCtxMenu] = useState<{ x: number; y: number; path: string; isDir: boolean; name: string } | null>(null);
  const [renaming, setRenaming] = useState<string | null>(null);
  const [renameValue, setRenameValue] = useState('');
  const [playhead, setPlayhead] = useState<number | null>(null);
  const [view, setView] = useState<{ start: number; end: number } | null>(null);
  const [history, setHistory] = useState<HistoryEvent[]>([]);
  const [mode, setMode] = useState<'live' | 'read'>('live');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [meta, setMeta] = useState<Record<string, SavedVaultMeta>>({});
  const liveRef = useRef<HTMLDivElement | null>(null);
  const saveTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const paintedKey = useRef<string>('');
  const composing = useRef(false);
  const trackRef = useRef<HTMLDivElement | null>(null);

  // Vault metadata (name/hue/path) persisted in localStorage so the home list
  // survives reloads even before the engine is queried.
  const metaKey = 'asp.editor.meta.v1';
  const loadMeta = useCallback(() => {
    try {
      const raw = localStorage.getItem(metaKey);
      if (raw) setMeta(JSON.parse(raw));
    } catch {
      /* ignore */
    }
  }, []);
  const saveMeta = useCallback((m: Record<string, SavedVaultMeta>) => {
    setMeta(m);
    try {
      localStorage.setItem(metaKey, JSON.stringify(m));
    } catch {
      /* ignore */
    }
  }, []);

  const reload = useCallback(async () => {
    try {
      const vs = await api.listVaults();
      setVaults(vs);
      // Ensure each has meta.
      setMeta((prev) => {
        const next = { ...prev };
        let changed = false;
        for (const v of vs) {
          if (!next[v.id]) {
            const name = v.path ? v.path.split('/').pop() || 'Vault' : 'Browser vault';
            next[v.id] = { id: v.id, name, hue: hashStr(v.id) % 360, kind: v.path ? 'folder' : 'browser', path: v.path || null, lastSync: 'just now' };
            changed = true;
          }
        }
        if (changed) {
          try {
            localStorage.setItem(metaKey, JSON.stringify(next));
          } catch {
            /* ignore */
          }
        }
        return changed ? next : prev;
      });
    } catch (e) {
      setError(String(e));
    }
  }, [api]);

  useEffect(() => {
    void loadMeta();
    void reload();
    void api.identity().then(setIdentity).catch(() => {});
  }, [api, loadMeta, reload]);

  const accent = '#3d63dd';
  const accentSoft = accent + '22';

  // ---- vault open ----
  const openVault = useCallback(async (id: string) => {
    setBusy(true);
    try {
      const t = await api.filesTree(id);
      setTree(t);
      const exp: Record<string, boolean> = {};
      const walk = (nodes: TreeNode[]) => {
        for (const n of nodes) {
          if (n.is_dir) {
            exp[n.path] = true;
            if (n.children) walk(n.children);
          }
        }
      };
      walk(t);
      setExpanded(exp);
      // pick README or first file
      let first: string | null = null;
      const findFirst = (nodes: TreeNode[]) => {
        for (const n of nodes) {
          if (!n.is_dir) {
            first = first || n.path;
          } else if (n.children) findFirst(n.children);
        }
      };
      findFirst(t);
      const pick = (function findReadme(nodes: TreeNode[]): string | null {
        for (const n of nodes) {
          if (!n.is_dir && /readme/i.test(n.name)) return n.path;
          if (n.children) {
            const r = findReadme(n.children);
            if (r) return r;
          }
        }
        return null;
      })(t);
      const sel = pick ?? first;
      setSelectedPath(sel);
      if (sel) {
        const c = await api.readFile(id, sel);
        setContent(c ?? '');
      } else {
        setContent('');
      }
      setActiveId(id);
      setScreen('editor');
      setPlayhead(null);
      const now = Date.now();
      setView({ start: now - 7 * DAY, end: now + 0.4 * DAY });
      try {
        setHistory(await api.history(id));
      } catch {
        setHistory([]);
      }
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }, [api]);

  // ---- file operations ----
  const selectFile = useCallback(async (path: string) => {
    if (!activeId) return;
    setSelectedPath(path);
    setPlayhead(null);
    setMode('live');
    try {
      const c = await api.readFile(activeId, path);
      setContent(c ?? '');
    } catch (e) {
      setError(String(e));
    }
  }, [activeId, api]);

  const commitContent = useCallback(async (src: string) => {
    if (!activeId || !selectedPath) return;
    setSaving(true);
    try {
      await api.writeFile(activeId, selectedPath, src);
      setContent(src);
      try {
        setHistory(await api.history(activeId));
      } catch {
        /* ignore */
      }
    } catch (e) {
      setError(String(e));
    } finally {
      setSaving(false);
    }
  }, [activeId, selectedPath, api]);

  const onNewFile = useCallback(async () => {
    if (!activeId) return;
    try {
      const path = await api.newFile(activeId, 'untitled.md', '# untitled\n\n');
      setTree(await api.filesTree(activeId));
      setSelectedPath(path);
      setContent('# untitled\n\n');
      setMode('live');
      setPlayhead(null);
    } catch (e) {
      setError(String(e));
    }
  }, [activeId, api]);

  const toggleDir = (path: string) => setExpanded((s) => ({ ...s, [path]: !s[path] }));

  const deleteNode = useCallback(async (path: string) => {
    if (!activeId) return;
    try {
      await api.deleteFile(activeId, path);
      setTree(await api.filesTree(activeId));
      if (selectedPath === path) {
        setSelectedPath(null);
        setContent('');
      }
      setHistory(await api.history(activeId));
    } catch (e) {
      setError(String(e));
    }
  }, [activeId, api, selectedPath]);

  const renameNode = useCallback(async (oldPath: string, newName: string) => {
    if (!activeId) return;
    const dir = oldPath.includes('/') ? oldPath.slice(0, oldPath.lastIndexOf('/')) : '';
    const newPath = (dir ? dir + '/' : '') + newName;
    if (newPath === oldPath) return;
    try {
      await api.renameFile(activeId, oldPath, newPath);
      setTree(await api.filesTree(activeId));
      if (selectedPath === oldPath) setSelectedPath(newPath);
      setHistory(await api.history(activeId));
    } catch (e) {
      setError(String(e));
    }
  }, [activeId, api, selectedPath]);

  const commitRename = useCallback(async () => {
    const path = renaming;
    const val = (renameValue || '').trim();
    setRenaming(null);
    if (path && val) await renameNode(path, val);
  }, [renaming, renameValue, renameNode]);

  // ---- connect / share / remove ----
  const onConnect = useCallback(async () => {
    if (connecting) return;
    const t = ticket.trim();
    if (!t) return;
    setConnecting(true);
    setError(null);
    try {
      await api.cloneRemote(null, t, authKey || undefined);
      setTicket('');
      setAuthKey('');
      await reload();
    } catch (e) {
      setError(String(e));
    } finally {
      setConnecting(false);
    }
  }, [api, connecting, ticket, authKey, reload]);

  const onShare = useCallback(async () => {
    if (!activeId) return;
    try {
      const code = await api.setAllowConnections(activeId, true, authKey || undefined);
      setShare({ code: code ?? '', requireKey: !!authKey, accessKey: authKey, copied: false });
    } catch (e) {
      setError(String(e));
    }
  }, [api, activeId, authKey]);

  const onSyncNow = useCallback(async () => {
    if (!activeId) return;
    setBusy(true);
    try {
      // On desktop, sync against the listening ticket if known; on web, the
      // vault already carries its peer ticket in metadata.
      const v = vaults.find((x) => x.id === activeId);
      const t = v?.listening_ticket ?? ticket;
      if (t) await api.syncNow(activeId, t, authKey || undefined);
      setTree(await api.filesTree(activeId));
      if (selectedPath) {
        const c = await api.readFile(activeId, selectedPath);
        setContent(c ?? '');
      }
      setHistory(await api.history(activeId));
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }, [api, activeId, ticket, authKey, vaults, selectedPath]);

  const onRemove = useCallback(async () => {
    if (!removeModal) return;
    try {
      await api.removeVault(removeModal.id, removeModal.trash);
      if (activeId === removeModal.id) {
        setActiveId(null);
        setScreen('connect');
      }
      await reload();
      setRemoveModal(null);
    } catch (e) {
      setError(String(e));
    }
  }, [api, removeModal, activeId, reload]);

  // ---- history / point-in-time ----
  const curView = view ?? { start: Date.now() - 7 * DAY, end: Date.now() + 0.4 * DAY };
  const now = Date.now();
  const playT = playhead == null ? now : playhead;
  const timeTravel = playhead != null && playhead < now;
  const colorOf = (ty: string) => (ty === 'create' ? '#3fa45a' : ty === 'edit' ? accent : ty === 'rename' ? '#d9a93d' : '#d96a6a');

  const resolveAt = useCallback(async (path: string, ts: number): Promise<FileAtTime> => {
    if (!activeId) return { exists: false, content: null, key: 'gone' };
    if (ts == null || ts >= now) {
      const c = await api.readFile(activeId, path);
      return { exists: c != null, content: c ?? null, key: 'live' };
    }
    return api.fileAtTime(activeId, path, ts);
  }, [activeId, api, now]);

  const paintEditor = useCallback((el: HTMLDivElement, r: FileAtTime) => {
    const tt = !r.exists || (playhead != null && playhead < now);
    el.contentEditable = tt ? 'false' : 'true';
    el.style.opacity = tt ? '0.92' : '1';
    if (!r.exists) el.innerHTML = '<div style="color:#b0aaa2;font-style:italic">This file did not exist at this point in time.</div>';
    else el.innerHTML = renderLiveHtml(r.content ?? '', accent);
    paintedKey.current = (selectedPath ?? '') + '|' + r.key;
  }, [playhead, now, selectedPath]);

  // Re-paint the live editor when selection or playhead changes.
  useEffect(() => {
    if (!liveRef.current || !selectedPath || mode !== 'live') return;
    let cancelled = false;
    void resolveAt(selectedPath, playhead ?? now).then((r) => {
      if (cancelled || !liveRef.current) return;
      if (selectedPath + '|' + r.key !== paintedKey.current) paintEditor(liveRef.current, r);
    });
    return () => {
      cancelled = true;
    };
  }, [selectedPath, playhead, mode, resolveAt, paintEditor, now]);

  // ---- live editor input ----
  const readLive = (el: HTMLDivElement): string => {
    const out: string[] = [];
    el.childNodes.forEach((n) => {
      if (n.nodeType === 3) out.push(n.nodeValue ?? '');
      else if (n.nodeName === 'BR') out.push('');
      else out.push((n.textContent ?? ''));
    });
    return out.join('\n');
  };
  const caretOffset = (el: HTMLDivElement): number | null => {
    const sel = getSelection();
    if (!sel || !sel.rangeCount) return null;
    const r = sel.getRangeAt(0);
    let offset = 0;
    const kids = [...el.childNodes];
    for (let i = 0; i < kids.length; i++) {
      const child = kids[i];
      if (i > 0) offset += 1;
      if (child === r.endContainer) {
        offset += child.nodeType === 3 ? r.endOffset : 0;
        return offset;
      }
      if (child.nodeType !== 3 && child.contains(r.endContainer)) {
        let acc = 0;
        const w = document.createTreeWalker(child, NodeFilter.SHOW_TEXT);
        let n: Node | null;
        while ((n = w.nextNode())) {
          if (n === r.endContainer) return offset + acc + r.endOffset;
          acc += n.nodeValue?.length ?? 0;
        }
        return offset + acc;
      }
      offset += child.nodeType === 3 ? (child.nodeValue?.length ?? 0) : (child.nodeName === 'BR' ? 0 : (child.textContent?.length ?? 0));
    }
    return null;
  };
  const setCaret = (el: HTMLDivElement, target: number) => {
    let remaining = target;
    const kids = [...el.childNodes];
    for (let i = 0; i < kids.length; i++) {
      const child = kids[i];
      if (i > 0) {
        if (remaining === 0) {
          placeInNode(child, 0);
          return;
        }
        remaining -= 1;
      }
      const len = child.nodeType === 3 ? (child.nodeValue?.length ?? 0) : (child.nodeName === 'BR' ? 0 : (child.textContent?.length ?? 0));
      if (remaining <= len) {
        placeInNode(child, remaining);
        return;
      }
      remaining -= len;
    }
    const last = kids[kids.length - 1];
    if (last) placeInNode(last, last.nodeType === 3 ? (last.nodeValue?.length ?? 0) : (last.textContent?.length ?? 0));
  };
  const placeInNode = (child: Node, pos: number) => {
    const sel = getSelection();
    if (!sel) return;
    const range = document.createRange();
    if (child.nodeType === 3) {
      range.setStart(child, Math.min(pos, child.nodeValue?.length ?? 0));
    } else {
      const w = document.createTreeWalker(child, NodeFilter.SHOW_TEXT);
      let n: Node | null;
      let acc = 0;
      let last: Node | null = null;
      let placed = false;
      while ((n = w.nextNode())) {
        last = n;
        if (pos <= acc + (n.nodeValue?.length ?? 0)) {
          range.setStart(n, pos - acc);
          placed = true;
          break;
        }
        acc += n.nodeValue?.length ?? 0;
      }
      if (!placed) {
        if (last) range.setStart(last, last.nodeValue?.length ?? 0);
        else range.setStart(child, 0);
      }
    }
    range.collapse(true);
    sel.removeAllRanges();
    sel.addRange(range);
  };

  const onLiveInput = (e: React.FormEvent<HTMLDivElement>) => {
    const el = e.currentTarget;
    if (playhead != null && playhead < now) return;
    if (composing.current) {
      void commitContent(readLive(el));
      return;
    }
    const off = caretOffset(el);
    const src = readLive(el);
    el.innerHTML = renderLiveHtml(src, accent);
    if (off != null) setCaret(el, off);
    if (saveTimer.current) clearTimeout(saveTimer.current);
    saveTimer.current = setTimeout(() => void commitContent(src), 650);
  };
  const onLivePaste = (e: React.ClipboardEvent) => {
    if (playhead != null && playhead < now) {
      e.preventDefault();
      return;
    }
    e.preventDefault();
    const t = (e.clipboardData).getData('text/plain');
    document.execCommand('insertText', false, t);
  };

  // ---- timeline geometry ----
  const span = curView.end - curView.start;
  const toPct = (ts: number) => ((ts - curView.start) / span) * 100;
  const STEPS = [5 * MIN, 15 * MIN, 30 * MIN, HOUR, 3 * HOUR, 6 * HOUR, 12 * HOUR, DAY, 2 * DAY, 7 * DAY, 14 * DAY, 30 * DAY];
  const raw = span / 6;
  let step = STEPS[STEPS.length - 1];
  for (const s of STEPS) {
    if (s >= raw) {
      step = s;
      break;
    }
  }
  const axisTicks: { label: string; left: string }[] = [];
  for (let t = Math.ceil(curView.start / step) * step; t <= curView.end; t += step) {
    axisTicks.push({ label: fmtTick(t, step), left: `${toPct(t)}%` });
  }
  const ticks = history
    .filter((e) => e.ts >= curView.start - span * 0.03 && e.ts <= curView.end + span * 0.03)
    .map((e) => ({ title: `${e.kind} · ${e.path ?? ''} · ${fmtFull(e.ts)}`, left: `${toPct(e.ts)}%`, bg: colorOf(e.kind), past: e.ts <= playT }));
  const playPct = Math.max(0, Math.min(100, toPct(playT)));
  const nowPct = Math.max(0, Math.min(100, toPct(now)));

  const onRestoreHere = useCallback(async () => {
    if (!activeId || !selectedPath || playhead == null) return;
    try {
      const ok = await api.restoreFileAt(activeId, selectedPath, playhead);
      if (ok) {
        setPlayhead(null);
        setTree(await api.filesTree(activeId));
        setContent((await api.readFile(activeId, selectedPath)) ?? '');
        setHistory(await api.history(activeId));
      } else {
        setPlayhead(null);
      }
    } catch (e) {
      setError(String(e));
    }
  }, [api, activeId, selectedPath, playhead]);

  // ---- render ----
  const isDesktop = api.isDesktop;
  const activeMeta = activeId ? meta[activeId] : undefined;
  const words = content.trim() ? content.trim().split(/\s+/).length : 0;
  const wordCount = words + (words === 1 ? ' word' : ' words');
  const crumb = selectedPath ? selectedPath.split('/') : [];
  const crumbFile = crumb[crumb.length - 1] ?? '';
  const crumbDir = crumb.length > 1 ? crumb.slice(0, -1).join(' / ') + ' / ' : '';

  // Flatten the tree for the sidebar rows.
  const rows: { name: string; isDir: boolean; path: string; depth: number; open: boolean; active: boolean; renaming: boolean }[] = [];
  const flat = (nodes: TreeNode[], depth: number) => {
    for (const n of nodes) {
      const open = !!expanded[n.path];
      const active = !n.is_dir && selectedPath === n.path;
      rows.push({ name: n.name, isDir: n.is_dir, path: n.path, depth, open, active, renaming: renaming === n.path });
      if (n.is_dir && open && n.children) flat(n.children, depth + 1);
    }
  };
  flat(tree, 0);

  return (
    <>
      <style>{FONT_CSS}</style>
      <style>{BASE_CSS}</style>
      {screen === 'connect' ? (
        <ConnectScreen
          vaults={vaults}
          meta={meta}
          identity={identity}
          isDesktop={isDesktop}
          ticket={ticket}
          authKey={authKey}
          connecting={connecting}
          error={error}
          onTicket={setTicket}
          onAuthKey={setAuthKey}
          onConnect={onConnect}
          onOpen={openVault}
          onCtx={(e, v) => {
            e.preventDefault();
            e.stopPropagation();
            setCtxMenu({ x: Math.min(e.clientX, window.innerWidth - 188), y: Math.min(e.clientY, window.innerHeight - 70), path: v.id, isDir: false, name: meta[v.id]?.name ?? 'Vault' });
          }}
          onNewVault={async () => {
            if (isDesktop) return; // desktop uses folder picker via onOpenFolder
            try {
              await (api as unknown as { createBrowserVault?: (n: string) => Promise<VaultInfo> }).createBrowserVault?.('Untitled vault');
              await reload();
            } catch (e) {
              setError(String(e));
            }
          }}
          onOpenFolder={async () => {
            if (!isDesktop) return;
            // Tauri folder picker via the dialog plugin.
            try {
              const mod = await import('@tauri-apps/plugin-dialog');
              const dir = await mod.open({ directory: true });
              if (typeof dir === 'string') {
                setBusy(true);
                try {
                  await api.addLocalFolder(dir);
                  await reload();
                } finally {
                  setBusy(false);
                }
              }
            } catch (e) {
              setError(String(e));
            }
          }}
          busy={busy}
        />
      ) : (
        <EditorScreen
          accent={accent}
          accentSoft={accentSoft}
          meta={activeMeta}
          identity={identity}
          rows={rows}
          expanded={expanded}
          selectedPath={selectedPath}
          content={content}
          wordCount={wordCount}
          crumbDir={crumbDir}
          crumbFile={crumbFile}
          saving={saving}
          liveRef={liveRef}
          composing={composing}
          onLiveInput={onLiveInput}
          onLivePaste={onLivePaste}
          onNewFile={onNewFile}
          onBack={() => { setScreen('connect'); setActiveId(null); }}
          onToggleDir={toggleDir}
          onSelectFile={selectFile}
          onCtx={(e, n) => { e.preventDefault(); e.stopPropagation(); setCtxMenu({ x: Math.min(e.clientX, window.innerWidth - 184), y: Math.min(e.clientY, window.innerHeight - 110), path: n.path, isDir: n.isDir, name: n.name }); }}
          renameValue={renameValue}
          onRenameChange={setRenameValue}
          onRenameKey={(e) => { if (e.key === 'Enter') { e.preventDefault(); void commitRename(); } else if (e.key === 'Escape') setRenaming(null); }}
          onRenameCommit={() => void commitRename()}
          onShare={onShare}
          onSyncNow={onSyncNow}
          isDesktop={isDesktop}
          busy={busy}
          mode={mode}
          setMode={setMode}
          // history timeline
          history={history}
          curView={curView}
          axisTicks={axisTicks}
          ticks={ticks}
          playPct={playPct}
          nowPct={nowPct}
          playT={playT}
          timeTravel={timeTravel}
          trackRef={trackRef}
          onTrackDown={(e) => {
            if (!trackRef.current) return;
            const r = trackRef.current.getBoundingClientRect();
            const startX = e.clientX;
            const v0 = curView;
            const span0 = v0.end - v0.start;
            let moved = false;
            const move = (ev: PointerEvent) => {
              const dx = ev.clientX - startX;
              if (Math.abs(dx) > 3) moved = true;
              if (moved) {
                const dt = -(dx / r.width) * span0;
                setView({ start: v0.start + dt, end: v0.end + dt });
              }
            };
            const up = (ev: PointerEvent) => {
              document.removeEventListener('pointermove', move);
              document.removeEventListener('pointerup', up);
              if (!moved) setPlayhead(Math.min(v0.start + ((ev.clientX - r.left) / r.width) * span0, now));
            };
            document.addEventListener('pointermove', move);
            document.addEventListener('pointerup', up);
          }}
          onHandleDown={(e) => {
            e.stopPropagation();
            const move = (ev: PointerEvent) => {
              if (!trackRef.current) return;
              const r = trackRef.current.getBoundingClientRect();
              const t = Math.max(now - 90 * DAY, Math.min(v0start + ((ev.clientX - r.left) / r.width) * span0, now));
              setPlayhead(t);
            };
            const v0start = curView.start;
            const span0 = span;
            const up = () => {
              document.removeEventListener('pointermove', move);
              document.removeEventListener('pointerup', up);
            };
            document.addEventListener('pointermove', move);
            document.addEventListener('pointerup', up);
          }}
          onZoom={(factor) => {
            const c = playhead != null ? playhead : now;
            const f = (c - curView.start) / span;
            const ns = Math.max(MIN * 10, Math.min(60 * DAY, span * factor));
            setView({ start: c - f * ns, end: c - f * ns + ns });
          }}
          onNow={() => setPlayhead(null)}
          onRestoreHere={onRestoreHere}
        />
      )}

      {/* context menu */}
      {ctxMenu && (
        <>
          <div style={{ position: 'fixed', inset: 0, zIndex: 60 }} onClick={() => setCtxMenu(null)} onContextMenu={(e) => { e.preventDefault(); setCtxMenu(null); }} />
          <div style={{ position: 'fixed', left: ctxMenu.x, top: ctxMenu.y, zIndex: 61, width: 172, background: '#fff', border: '1px solid #e7e5e4', borderRadius: 10, boxShadow: '0 10px 28px rgba(28,25,23,0.15)', padding: 4 }}>
            {screen === 'editor' && ctxMenu.path && (
              <>
                <CtxItem onClick={() => { setRenaming(ctxMenu.path); setRenameValue(ctxMenu.name); setCtxMenu(null); }}>Rename</CtxItem>
                <CtxItem onClick={() => { const c = ctxMenu; setCtxMenu(null); if (c) void deleteNode(c.path); }}>Delete</CtxItem>
              </>
            )}
            {screen === 'connect' && ctxMenu.path && (
              <CtxItem onClick={() => { const c = ctxMenu; setCtxMenu(null); if (c) setRemoveModal({ id: c.path, name: c.name, path: meta[c.path]?.path ?? null, kind: meta[c.path]?.kind ?? 'browser', trash: false }); }}>Remove vault</CtxItem>
            )}
          </div>
        </>
      )}

      {/* rename input row (inline in sidebar) — rendered via state; the sidebar reads it */}

      {/* share modal */}
      {share && (
        <Modal onClose={() => setShare(null)}>
          <h3 style={{ margin: 0, fontSize: 18, fontWeight: 600 }}>Share this vault</h3>
          <p style={{ color: '#57534e', fontSize: 13, margin: '6px 0 16px' }}>Anyone with this code can sync this vault.</p>
          <div style={{ display: 'flex', gap: 8 }}>
            <input readOnly value={share.code} style={{ flex: 1, fontFamily: 'JetBrains Mono, monospace', fontSize: 12.5, padding: '11px 13px', border: '1px solid #e7e5e4', borderRadius: 10, outline: 'none' }} />
            <button onClick={() => { try { navigator.clipboard?.writeText(share.code); } catch { /* ignore */ } setShare({ ...share, copied: true }); setTimeout(() => setShare((s) => (s ? { ...s, copied: false } : null)), 1400); }} style={{ padding: '0 14px', border: '1px solid #e0ddd8', borderRadius: 10, background: '#fff', cursor: 'pointer', fontSize: 12.5 }}>{share.copied ? 'Copied' : 'Copy'}</button>
          </div>
          <button onClick={() => setShare(null)} style={{ marginTop: 16, width: '100%', height: 40, border: 'none', borderRadius: 10, background: '#1c1917', color: '#fff', cursor: 'pointer', fontSize: 14, fontWeight: 500 }}>Done</button>
        </Modal>
      )}

      {/* remove modal */}
      {removeModal && (
        <Modal onClose={() => setRemoveModal(null)}>
          <h3 style={{ margin: 0, fontSize: 18, fontWeight: 600 }}>Remove {removeModal.name}?</h3>
          <p style={{ color: '#57534e', fontSize: 13, margin: '6px 0 16px' }}>
            {isDesktop && removeModal.kind === 'folder'
              ? removeModal.trash ? 'The folder and its notes will be moved to the Trash.' : 'The folder stays on your computer — it’s only removed from asp.'
              : 'This deletes the vault from this browser. This can’t be undone.'}
          </p>
          {isDesktop && removeModal.kind === 'folder' && (
            <label style={{ display: 'flex', alignItems: 'center', gap: 10, margin: '0 0 16px', cursor: 'pointer', fontSize: 13 }}>
              <input type="checkbox" checked={removeModal.trash} onChange={(e) => setRemoveModal({ ...removeModal, trash: e.target.checked })} />
              Move folder to Trash
            </label>
          )}
          <div style={{ display: 'flex', gap: 8, justifyContent: 'flex-end' }}>
            <button onClick={() => setRemoveModal(null)} style={{ padding: '8px 16px', border: '1px solid #e0ddd8', borderRadius: 9, background: '#fff', cursor: 'pointer', fontSize: 13 }}>Cancel</button>
            <button onClick={onRemove} style={{ padding: '8px 16px', border: 'none', borderRadius: 9, background: removeModal.trash ? '#c0392b' : '#1c1917', color: '#fff', cursor: 'pointer', fontSize: 13, fontWeight: 500 }}>{removeModal.trash ? 'Remove & Trash' : 'Remove'}</button>
          </div>
        </Modal>
      )}

      {error && (
        <Modal onClose={() => setError(null)}>
          <h3 style={{ margin: 0, fontSize: 16, fontWeight: 600, color: '#b00020' }}>Something went wrong</h3>
          <pre style={{ whiteSpace: 'pre-wrap', wordBreak: 'break-word', fontSize: 12.5, color: '#44403c', margin: '8px 0 0' }}>{error}</pre>
          <button onClick={() => setError(null)} style={{ marginTop: 16, padding: '8px 16px', border: 'none', borderRadius: 9, background: '#1c1917', color: '#fff', cursor: 'pointer' }}>Dismiss</button>
        </Modal>
      )}
    </>
  );
}

const BASE_CSS = `
  * { box-sizing: border-box; }
  html, body { margin: 0; padding: 0; height: 100%; }
  body { font-family: system-ui, -apple-system, 'Segoe UI', sans-serif; -webkit-font-smoothing: antialiased; }
  ::selection { background: rgba(61,99,221,0.16); }
  textarea::placeholder, input::placeholder { color: #b8b3ac; }
  .asp-scroll::-webkit-scrollbar { width: 10px; height: 10px; }
  .asp-scroll::-webkit-scrollbar-thumb { background: #00000016; border-radius: 8px; border: 3px solid transparent; background-clip: padding-box; }
  .asp-scroll::-webkit-scrollbar-thumb:hover { background: #00000026; background-clip: padding-box; }
  .cm-mark { display: none; }
  .cm-code { font-family: 'JetBrains Mono', monospace; font-size: 0.86em; background: #f3f1ec; padding: 1px 5px; border-radius: 5px; }
  .cm-link { color: var(--accent, #3d63dd); border-bottom: 1px solid var(--accent, #3d63dd); }
  .cm-ul { position: relative; padding-left: 1.5em; }
  .cm-ul::before { content: '•'; position: absolute; left: 0.4em; top: -0.02em; color: var(--accent, #3d63dd); font-weight: 700; }
  .cm-task { position: relative; padding-left: 1.85em; }
  .cm-task::before { content: ''; position: absolute; left: 0.1em; top: 0.28em; width: 16px; height: 16px; border: 1.6px solid #cfcbc3; border-radius: 4px; box-sizing: border-box; }
  .cm-task-done::before { background: var(--accent, #3d63dd); border-color: var(--accent, #3d63dd); }
  .cm-task-done::after { content: ''; position: absolute; left: 0.52em; top: 0.46em; width: 5px; height: 9px; border: solid #fff; border-width: 0 2px 2px 0; transform: rotate(43deg); }
  .cm-task-done .cm-body { color: #a8a29e; text-decoration: line-through; }
`;

function CtxItem({ children, onClick }: { children: React.ReactNode; onClick: () => void }) {
  return (
    <div onClick={onClick} style={{ padding: '7px 10px', fontSize: 13, cursor: 'pointer', borderRadius: 6, color: '#1c1917' }} onMouseEnter={(e) => (e.currentTarget.style.background = '#f5f3f0')} onMouseLeave={(e) => (e.currentTarget.style.background = 'transparent')}>
      {children}
    </div>
  );
}

function Modal({ children, onClose }: { children: React.ReactNode; onClose: () => void }) {
  return (
    <div style={{ position: 'fixed', inset: 0, background: 'rgba(28,25,23,0.35)', display: 'flex', alignItems: 'center', justifyContent: 'center', zIndex: 100 }} onClick={onClose}>
      <div style={{ background: '#fff', borderRadius: 14, padding: 22, width: 'min(440px, 92vw)', boxShadow: '0 20px 50px rgba(28,25,23,0.25)' }} onClick={(e) => e.stopPropagation()}>
        {children}
      </div>
    </div>
  );
}

// ---- connect screen ----
function ConnectScreen(props: {
  vaults: VaultInfo[];
  meta: Record<string, SavedVaultMeta>;
  identity: string;
  isDesktop: boolean;
  ticket: string;
  authKey: string;
  connecting: boolean;
  error: string | null;
  onTicket: (s: string) => void;
  onAuthKey: (s: string) => void;
  onConnect: () => void;
  onOpen: (id: string) => void;
  onCtx: (e: React.MouseEvent, v: VaultInfo) => void;
  onNewVault: () => void;
  onOpenFolder: () => void;
  busy: boolean;
}) {
  const connectDisabled = props.connecting || !props.ticket.trim();
  return (
    <div style={{ position: 'fixed', inset: 0, display: 'flex', flexDirection: 'column', alignItems: 'center', justifyContent: 'center', background: '#fafaf8', color: '#1c1917', padding: 32, overflow: 'auto' }}>
      <div style={{ width: 'min(452px, 94vw)', display: 'flex', flexDirection: 'column' }}>
        <div style={{ display: 'flex', alignItems: 'center', gap: 11, marginBottom: 34 }}>
          <div style={{ width: 26, height: 26, borderRadius: 7, background: '#3d63dd', display: 'flex', alignItems: 'center', justifyContent: 'center', flex: 'none' }}>
            <div style={{ width: 9, height: 9, borderRadius: '50%', background: '#fff' }} />
          </div>
          <div style={{ fontFamily: "'JetBrains Mono', monospace", fontWeight: 600, fontSize: 16, letterSpacing: '-0.01em' }}>asp</div>
          <span style={{ flex: 1 }} />
          <div style={{ fontSize: 12, color: '#a8a29e' }}>{props.isDesktop ? 'On this computer' : 'Saved in this browser'}</div>
        </div>
        <h1 style={{ fontSize: 25, fontWeight: 600, letterSpacing: '-0.02em', margin: '0 0 22px' }}>Your vaults</h1>
        <div style={{ display: 'flex', gap: 10 }}>
          <button onClick={props.isDesktop ? props.onOpenFolder : props.onNewVault} disabled={props.busy} style={{ flex: 1, display: 'flex', alignItems: 'center', justifyContent: 'center', gap: 9, height: 46, border: 'none', borderRadius: 12, background: '#1c1917', color: '#fff', fontSize: 14, fontWeight: 500, cursor: 'pointer' }}>
            {props.isDesktop ? 'Open a folder' : 'New vault'}
          </button>
        </div>
        <div style={{ marginTop: 14, background: '#fff', border: '1px solid #e7e5e4', borderRadius: 14, padding: 16, display: 'flex', flexDirection: 'column', gap: 13 }}>
          <label style={{ display: 'flex', flexDirection: 'column', gap: 7 }}>
            <span style={{ fontSize: 12, fontWeight: 500, color: '#57534e' }}>Invite code</span>
            <textarea value={props.ticket} onChange={(e) => props.onTicket(e.target.value)} rows={2} spellCheck={false} placeholder="Paste the code someone shared with you" style={{ fontFamily: "'JetBrains Mono', monospace", fontSize: 12.5, lineHeight: 1.5, color: '#1c1917', background: '#faf9f7', border: '1px solid #e7e5e4', borderRadius: 10, padding: '11px 13px', resize: 'none', outline: 'none', width: '100%' }} />
          </label>
          <label style={{ display: 'flex', flexDirection: 'column', gap: 7 }}>
            <span style={{ fontSize: 12, fontWeight: 500, color: '#57534e' }}>Access key <span style={{ color: '#a8a29e', fontWeight: 400 }}>— if required</span></span>
            <input value={props.authKey} onChange={(e) => props.onAuthKey(e.target.value)} type="password" spellCheck={false} placeholder="Leave blank if you weren't given one" style={{ fontFamily: "'JetBrains Mono', monospace", fontSize: 12.5, color: '#1c1917', background: '#faf9f7', border: '1px solid #e7e5e4', borderRadius: 10, padding: '11px 13px', outline: 'none', width: '100%' }} />
          </label>
          <button onClick={props.onConnect} disabled={connectDisabled} style={{ display: 'flex', alignItems: 'center', justifyContent: 'center', gap: 9, height: 44, border: 'none', borderRadius: 11, background: connectDisabled ? '#c9c5be' : '#1c1917', color: '#fff', fontSize: 14, fontWeight: 500, cursor: connectDisabled ? 'default' : 'pointer' }}>
            {props.connecting && <span style={{ width: 13, height: 13, border: '2px solid #ffffff66', borderTopColor: '#fff', borderRadius: '50%', display: 'inline-block', animation: 'aspSpin 0.7s linear infinite' }} />}
            {props.connecting ? 'Connecting…' : 'Connect'}
          </button>
        </div>
        {props.vaults.length > 0 && (
          <div style={{ marginTop: 26, display: 'flex', flexDirection: 'column', gap: 2 }}>
            {props.vaults.map((v) => {
              const m = props.meta[v.id] ?? { name: v.path ? v.path.split('/').pop() || 'Vault' : 'Browser vault', hue: hashStr(v.id) % 360, kind: v.path ? 'folder' : 'browser', path: v.path, lastSync: 'just now' };
              return (
                <div key={v.id} onClick={() => props.onOpen(v.id)} onContextMenu={(e) => props.onCtx(e, v)} style={{ display: 'flex', alignItems: 'center', gap: 13, padding: '11px 12px', borderRadius: 12, cursor: 'pointer' }} onMouseEnter={(e) => (e.currentTarget.style.background = '#f0efec')} onMouseLeave={(e) => (e.currentTarget.style.background = 'transparent')}>
                  <div style={{ width: 30, height: 30, borderRadius: 9, flex: 'none', background: `hsl(${m.hue} 52% 92%)`, border: `1px solid hsl(${m.hue} 40% 84%)` }} />
                  <div style={{ flex: 1, minWidth: 0 }}>
                    <div style={{ fontSize: 14, fontWeight: 500, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{m.name}</div>
                    <div style={{ fontSize: 11, color: '#a8a29e', fontFamily: m.kind === 'folder' ? "'JetBrains Mono', monospace" : 'inherit', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{m.path ?? 'In this browser'}</div>
                  </div>
                  <span style={{ fontSize: 11.5, color: '#a8a29e' }}>{m.lastSync}</span>
                </div>
              );
            })}
          </div>
        )}
        <div style={{ marginTop: 28, fontSize: 11.5, color: '#bdb8b0', display: 'flex', alignItems: 'center', gap: 7 }}>
          <span>This device · {props.identity}</span>
        </div>
        {props.error && <div style={{ marginTop: 16, fontSize: 12, color: '#b00020' }}>{props.error}</div>}
      </div>
    </div>
  );
}

// ---- editor screen ----
function EditorScreen(props: {
  accent: string;
  accentSoft: string;
  meta?: SavedVaultMeta;
  identity: string;
  rows: { name: string; isDir: boolean; path: string; depth: number; open: boolean; active: boolean; renaming: boolean }[];
  expanded: Record<string, boolean>;
  selectedPath: string | null;
  content: string;
  wordCount: string;
  crumbDir: string;
  crumbFile: string;
  saving: boolean;
  liveRef: React.MutableRefObject<HTMLDivElement | null>;
  composing: React.MutableRefObject<boolean>;
  onLiveInput: (e: React.FormEvent<HTMLDivElement>) => void;
  onLivePaste: (e: React.ClipboardEvent) => void;
  onNewFile: () => void;
  onBack: () => void;
  onToggleDir: (p: string) => void;
  onSelectFile: (p: string) => void;
  onCtx: (e: React.MouseEvent, n: { path: string; isDir: boolean; name: string }) => void;
  renameValue: string;
  onRenameChange: (s: string) => void;
  onRenameKey: (e: React.KeyboardEvent) => void;
  onRenameCommit: () => void;
  onShare: () => void;
  onSyncNow: () => void;
  isDesktop: boolean;
  busy: boolean;
  mode: 'live' | 'read';
  setMode: (m: 'live' | 'read') => void;
  history: HistoryEvent[];
  curView: { start: number; end: number };
  axisTicks: { label: string; left: string }[];
  ticks: { title: string; left: string; bg: string; past: boolean }[];
  playPct: number;
  nowPct: number;
  playT: number;
  timeTravel: boolean;
  trackRef: React.MutableRefObject<HTMLDivElement | null>;
  onTrackDown: (e: React.PointerEvent) => void;
  onHandleDown: (e: React.PointerEvent) => void;
  onZoom: (factor: number) => void;
  onNow: () => void;
  onRestoreHere: () => void;
}) {
  const m = props.meta ?? { name: 'Vault', hue: 222, kind: 'browser', path: null, lastSync: '' };
  const fontFam = "system-ui, -apple-system, 'Segoe UI', sans-serif";
  return (
    <div style={{ position: 'fixed', inset: 0, display: 'flex', flexDirection: 'column', background: '#fafaf8', color: '#1c1917' }}>
      {/* top bar */}
      <div style={{ display: 'flex', alignItems: 'center', gap: 10, padding: '10px 16px', borderBottom: '1px solid #ededea', flex: 'none' }}>
        <button onClick={props.onBack} style={{ border: '1px solid #e7e5e4', borderRadius: 8, background: '#fff', padding: '6px 10px', cursor: 'pointer', fontSize: 13 }}>‹ Back</button>
        <div style={{ width: 28, height: 28, borderRadius: 8, flex: 'none', background: `hsl(${m.hue} 52% 92%)`, border: `1px solid hsl(${m.hue} 40% 84%)` }} />
        <div style={{ fontWeight: 600, fontSize: 15 }}>{m.name}</div>
        <span style={{ flex: 1 }} />
        {props.isDesktop && <button onClick={props.onShare} style={{ border: '1px solid #e7e5e4', borderRadius: 8, background: '#fff', padding: '6px 12px', cursor: 'pointer', fontSize: 13 }}>Share</button>}
        <button onClick={props.onSyncNow} disabled={props.busy} style={{ border: '1px solid #e7e5e4', borderRadius: 8, background: '#fff', padding: '6px 12px', cursor: 'pointer', fontSize: 13 }}>{props.busy ? 'Syncing…' : 'Sync now'}</button>
        <button onClick={props.onNewFile} style={{ border: 'none', borderRadius: 8, background: '#1c1917', color: '#fff', padding: '6px 12px', cursor: 'pointer', fontSize: 13 }}>+ New</button>
      </div>
      <div style={{ flex: 1, display: 'flex', minHeight: 0 }}>
        {/* sidebar */}
        <div className="asp-scroll" style={{ width: 260, flex: 'none', borderRight: '1px solid #ededea', overflowY: 'auto', padding: '8px 8px' }}>
          {props.rows.map((r) => (
            <div key={r.path} onClick={r.isDir ? () => props.onToggleDir(r.path) : () => props.onSelectFile(r.path)} onContextMenu={(e) => props.onCtx(e, { path: r.path, isDir: r.isDir, name: r.name })} style={{ display: 'flex', alignItems: 'center', gap: 6, height: 29, paddingLeft: 7 + r.depth * 15, paddingRight: 8, borderRadius: 7, cursor: 'pointer', userSelect: 'none', fontSize: 13.5, fontWeight: r.isDir ? 500 : r.active ? 500 : 400, color: r.isDir ? '#44403c' : r.active ? '#1c1917' : '#57534e', background: r.active ? props.accentSoft : 'transparent' }}>
              <span style={{ display: 'inline-flex', color: '#a8a29e', transform: r.open ? 'rotate(90deg)' : 'rotate(0deg)', transition: 'transform .14s' }}>{r.isDir ? '▸' : ''}</span>
              <span style={{ color: r.active ? props.accent : '#b0aaa2' }}>{r.isDir ? '📁' : '📄'}</span>
              {r.renaming ? (
                <input
                  autoFocus
                  value={props.renameValue}
                  onChange={(e) => props.onRenameChange(e.target.value)}
                  onKeyDown={props.onRenameKey}
                  onBlur={props.onRenameCommit}
                  onFocus={(e) => e.currentTarget.select()}
                  onClick={(e) => e.stopPropagation()}
                  style={{ flex: 1, fontSize: 13, fontFamily: 'inherit', border: '1px solid ' + props.accent, borderRadius: 5, padding: '2px 5px', outline: 'none' }}
                />
              ) : (
                <span style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{r.name}</span>
              )}
            </div>
          ))}
          {props.rows.length === 0 && <div style={{ color: '#bdb8b0', fontSize: 12, padding: '12px 8px' }}>No files yet — click + New.</div>}
        </div>
        {/* editor */}
        <div style={{ flex: 1, display: 'flex', flexDirection: 'column', minWidth: 0 }}>
          <div className="asp-scroll" style={{ flex: 1, overflowY: 'auto', display: 'flex', justifyContent: 'center' }}>
            {props.selectedPath ? (
              props.mode === 'read' ? (
                <div style={{ width: 760, maxWidth: '100%', padding: '44px 40px 140px' }} dangerouslySetInnerHTML={{ __html: mdToHtml(props.content, props.accent) }} />
              ) : (
                <div
                  ref={props.liveRef}
                  onInput={props.onLiveInput}
                  onPaste={props.onLivePaste}
                  onCompositionStart={() => { props.composing.current = true; }}
                  onCompositionEnd={(e) => {
                    props.composing.current = false;
                    props.onLiveInput(e);
                  }}
                  contentEditable
                  suppressContentEditableWarning
                  style={{ width: 760, maxWidth: '100%', minHeight: '100%', outline: 'none', background: 'transparent', whiteSpace: 'pre-wrap', wordBreak: 'break-word', fontFamily: fontFam, fontSize: '15.5px', lineHeight: 1.8, color: '#1c1917', padding: '44px 40px 140px', ['--accent' as string]: props.accent }}
                />
              )
            ) : (
              <div style={{ color: '#bdb8b0', padding: 80, textAlign: 'center' }}>Select a file or create a new one.</div>
            )}
          </div>
          {/* footer */}
          <div style={{ display: 'flex', alignItems: 'center', gap: 12, padding: '8px 16px', borderTop: '1px solid #ededea', fontSize: 12, color: '#a8a29e', flex: 'none' }}>
            <span style={{ display: 'inline-flex', alignItems: 'center', gap: 6 }}>
              <span style={{ width: 7, height: 7, borderRadius: '50%', background: props.saving ? '#d9a93d' : '#3fa45a', transition: 'background .2s' }} />
              {props.saving ? 'Saving…' : 'Saved'}
            </span>
            <span>{props.crumbDir}<span style={{ color: '#1c1917', fontWeight: 500 }}>{props.crumbFile}</span></span>
            <span style={{ flex: 1 }} />
            <span>{props.wordCount}</span>
            <button onClick={() => props.setMode(props.mode === 'live' ? 'read' : 'live')} style={{ border: '1px solid #e7e5e4', borderRadius: 7, background: '#fff', padding: '3px 10px', cursor: 'pointer', fontSize: 12 }}>{props.mode === 'live' ? 'Preview' : 'Edit'}</button>
          </div>
          {/* history timeline */}
          <div style={{ padding: '8px 16px 12px', borderTop: '1px solid #ededea', flex: 'none' }}>
            <div style={{ display: 'flex', alignItems: 'center', gap: 10, marginBottom: 6 }}>
              <span style={{ fontSize: 11, fontFamily: "'JetBrains Mono', monospace", padding: '2px 9px', borderRadius: 20, background: props.timeTravel ? props.accentSoft : '#e9f2ec', color: props.timeTravel ? props.accent : '#3a9357', fontWeight: 500 }}>{props.timeTravel ? fmtFull(props.playT) : 'Live · now'}</span>
              <span style={{ fontSize: 11, color: '#a8a29e' }}>{props.history.length} rows</span>
              <span style={{ flex: 1 }} />
              <button onClick={() => props.onZoom(0.55)} style={{ border: '1px solid #e7e4df', borderRadius: 7, background: '#fff', padding: '4px 10px', cursor: 'pointer', fontSize: 12 }}>＋</button>
              <button onClick={() => props.onZoom(1.8)} style={{ border: '1px solid #e7e4df', borderRadius: 7, background: '#fff', padding: '4px 10px', cursor: 'pointer', fontSize: 12 }}>－</button>
              <button onClick={props.onNow} style={{ border: '1px solid #e7e4df', borderRadius: 7, background: '#fff', padding: '4px 12px', cursor: 'pointer', fontSize: 12, color: props.timeTravel ? '#57534e' : '#bdb8b0' }}>Now</button>
              {props.timeTravel && <button onClick={props.onRestoreHere} style={{ border: 'none', borderRadius: 7, background: props.accent, color: '#fff', padding: '4px 12px', cursor: 'pointer', fontSize: 12 }}>Restore here</button>}
            </div>
            <div ref={props.trackRef} onPointerDown={props.onTrackDown} style={{ position: 'relative', height: 44, marginLeft: 8, marginRight: 8, cursor: 'pointer' }}>
              {props.axisTicks.map((t, i) => (
                <div key={i} style={{ position: 'absolute', left: t.left, top: 0, bottom: 0, width: 1, background: '#efece7' }}>
                  <span style={{ position: 'absolute', left: 4, bottom: -2, fontSize: 9.5, color: '#bdb8b0', fontFamily: "'JetBrains Mono', monospace", whiteSpace: 'nowrap' }}>{t.label}</span>
                </div>
              ))}
              <div style={{ position: 'absolute', left: `${props.nowPct}%`, top: 0, bottom: 0, borderLeft: '1px dashed #cfcbc4' }} />
              {props.ticks.map((t, i) => (
                <div key={i} title={t.title} style={{ position: 'absolute', left: t.left, top: '16%', height: '68%', width: 2, marginLeft: -1, borderRadius: 1, background: t.bg, opacity: t.past ? 0.85 : 0.28 }} />
              ))}
              <div style={{ position: 'absolute', left: `${props.playPct}%`, top: -3, bottom: -3, width: 2, marginLeft: -1, background: props.timeTravel ? props.accent : '#1c1917', zIndex: 5 }}>
                <div onPointerDown={props.onHandleDown} style={{ position: 'absolute', left: -6, top: -8, width: 14, height: 14, borderRadius: 4, background: props.timeTravel ? props.accent : '#1c1917', cursor: 'ew-resize', boxShadow: '0 1px 4px rgba(28,25,23,0.3)' }} />
              </div>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
