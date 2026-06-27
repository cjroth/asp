// Vault Editor — the Context Desktop app. A faithful React port of the new
// "Vault Editor" design canvas, wired to the real backend (Tauri commands →
// asp-desktop-engine → asp-core). No protocol logic lives here; every vault,
// file, history and sync behavior is a command call. Cosmetic vault metadata
// (name/color/emoji) and view prefs (theme, font, sidebar, hidden/pretty) are
// local-only and never touch the protocol.
import { open } from '@tauri-apps/plugin-dialog';
import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { api, type FileEntry, type HistEvent, type VaultInfo, type VaultStatus } from './lib/api';
import CustomizeModal, { type CustomizeInit } from './vault/CustomizeModal';
import FileTree from './vault/FileTree';
import HistoryBar from './vault/HistoryBar';
import { buildEvents, createTsByPath, defaultView, type TrackEvent, type View, viewForNow } from './vault/history';
import * as Icon from './vault/icons';
import LiveEditor from './vault/LiveEditor';
import { countLabel } from './vault/markdown';
import { isDesktop } from './lib/platform';
import { basename, freeName, makeAccessKey, relTime, shortFingerprint } from './vault/format';
import { applyTheme, clampHistBar, clampSidebar, fontFamilyOf, HISTBAR_COLLAPSE, loadPrefs, type Prefs, savePrefs } from './vault/prefs';
import { isHidden } from './vault/prettyNames';
import { allDirPaths, buildTree, firstSelectable, flatten } from './vault/tree';
import { avatarStyle, glyphOf, hueForId, loadVaultMeta, resolveMeta, saveVaultMeta, type VaultMetaMap } from './vault/vaultMeta';

interface VaultMeta extends VaultInfo {
  displayName: string;
  hue: number;
  emoji: string | null;
  peers: number;
  lastTs: number | null;
  ticket: string | null;
}

interface Paint {
  source: string;
  readOnly: boolean;
  notExist: boolean;
  key: string;
}

interface CtxMenu {
  x: number;
  y: number;
  root?: boolean;
  path?: string;
  isDir?: boolean;
  name?: string;
}

export default function App() {
  const desktop = isDesktop();
  const [prefs, setPrefsState] = useState<Prefs>(loadPrefs);
  const accent = prefs.accent;
  const accentSoft = accent + '22';
  const fontFamily = fontFamilyOf(prefs);
  const centered = prefs.writingColumn !== false;
  const updatePrefs = useCallback((patch: Partial<Prefs>) => {
    setPrefsState((p) => {
      const next = { ...p, ...patch };
      savePrefs(next);
      return next;
    });
  }, []);

  // Apply the persisted theme to <html> on mount.
  useEffect(() => applyTheme(prefs.theme), []); // eslint-disable-line react-hooks/exhaustive-deps

  const [metaMap, setMetaMap] = useState<VaultMetaMap>(loadVaultMeta);
  const updateMeta = useCallback((vaultId: string, entry: { name?: string; hue: number; emoji?: string | null }) => {
    setMetaMap((m) => {
      const next = { ...m, [vaultId]: entry };
      saveVaultMeta(next);
      return next;
    });
  }, []);

  const [identity, setIdentity] = useState('');
  const [screen, setScreen] = useState<'connect' | 'editor'>('connect');
  const [vaults, setVaults] = useState<VaultInfo[]>([]);
  const [statuses, setStatuses] = useState<Record<string, VaultStatus>>({});
  const [activeId, setActiveId] = useState<string | null>(null);

  const [files, setFiles] = useState<FileEntry[]>([]);
  const [expanded, setExpanded] = useState<Record<string, boolean>>({});
  const [selectedPath, setSelectedPath] = useState<string | null>(null);
  // Multi-selection: every file path that's currently highlighted (always
  // includes `selectedPath`, the active/editor file). `anchorPath` is the last
  // plainly-clicked file — the fixed end of a shift-range.
  const [selectedPaths, setSelectedPaths] = useState<Set<string>>(new Set());
  const [anchorPath, setAnchorPath] = useState<string | null>(null);

  const [paint, setPaint] = useState<Paint | null>(null);
  const [docText, setDocText] = useState('');
  const [saving, setSaving] = useState(false);

  const [histRaw, setHistRaw] = useState<HistEvent[]>([]);
  const [now, setNow] = useState(() => Date.now());
  const [view, setView] = useState<View | null>(null);
  const [playhead, setPlayhead] = useState<number | null>(null);
  const [histOpen, setHistOpen] = useState(false);
  const [logOpen, setLogOpen] = useState(false);

  const [vaultMenuOpen, setVaultMenuOpen] = useState(false);
  const [newMenuOpen, setNewMenuOpen] = useState(false);
  const [filesMenuOpen, setFilesMenuOpen] = useState(false);
  const [ctxMenu, setCtxMenu] = useState<CtxMenu | null>(null);
  const [renaming, setRenaming] = useState<string | null>(null);
  const [renameValue, setRenameValue] = useState('');
  const [crumbEditing, setCrumbEditing] = useState(false);

  const [sidebarW, setSidebarW] = useState(prefs.sidebarW);
  const [histBarH, setHistBarH] = useState(prefs.histBarH);
  const [resizingBar, setResizingBar] = useState(false);

  const [entry, setEntry] = useState<'new' | 'connect' | null>(null);
  const [newVaultName, setNewVaultName] = useState('');
  const [ticket, setTicket] = useState('');
  const [authKey, setAuthKey] = useState('');
  const [connecting, setConnecting] = useState(false);
  const [connectDest, setConnectDest] = useState<string | null>(null);

  const [share, setShare] = useState<{ id: string; code: string; requireKey: boolean; accessKey: string; copied: boolean } | null>(null);
  const [vaultCtx, setVaultCtx] = useState<{ x: number; y: number; id: string; vaultId: string; name: string } | null>(null);
  const [customize, setCustomize] = useState<CustomizeInit | null>(null);
  const [removeVaultState, setRemoveVaultState] = useState<{ id: string; name: string; path: string; trash: boolean } | null>(null);

  // refs for values used inside imperative handlers / async flows
  const activeIdRef = useRef<string | null>(null);
  const selectedRef = useRef<string | null>(null);
  const selectedPathsRef = useRef<Set<string>>(new Set());
  const bufferRef = useRef('');
  const playheadRef = useRef<number | null>(null);
  const viewRef = useRef<View | null>(null);
  const nowRef = useRef(now);
  const saveTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const histTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const dirtyRef = useRef(false);
  const filesRef = useRef<FileEntry[]>([]);
  const contentRef = useRef<Record<string, string>>({});
  const paintSeq = useRef(0);
  activeIdRef.current = activeId;
  selectedRef.current = selectedPath;
  selectedPathsRef.current = selectedPaths;
  playheadRef.current = playhead;
  viewRef.current = view;
  nowRef.current = now;

  const curView = useCallback((): View => view || defaultView(now), [view, now]);
  const timeTravel = playhead != null && playhead < now;

  const events: TrackEvent[] = useMemo(() => buildEvents(histRaw), [histRaw]);

  // ---------- data loading ----------
  const refreshVaults = useCallback(async () => {
    const vs = await api.listVaults();
    setVaults(vs);
    return vs;
  }, []);

  const refreshStatuses = useCallback(async (vs: VaultInfo[]) => {
    const entries = await Promise.all(
      vs.map(async (v) => {
        try {
          return [v.id, await api.getStatus(v.id)] as const;
        } catch {
          return null;
        }
      }),
    );
    setStatuses((prev) => {
      const next = { ...prev };
      for (const e of entries) if (e) next[e[0]] = e[1];
      return next;
    });
  }, []);

  const refreshHistory = useCallback(async (id: string) => {
    try {
      setHistRaw(await api.history(id));
    } catch {
      setHistRaw([]);
    }
    setNow(Date.now());
  }, []);

  const scheduleHistory = useCallback(
    (id: string) => {
      if (histTimer.current) clearTimeout(histTimer.current);
      histTimer.current = setTimeout(() => void refreshHistory(id), 700);
    },
    [refreshHistory],
  );

  const refreshFiles = useCallback(async (id: string) => {
    const fs = await api.listFiles(id);
    filesRef.current = fs;
    setFiles(fs);
    return fs;
  }, []);

  useEffect(() => {
    void api.getIdentity().then(setIdentity).catch(() => {});
    void (async () => {
      const vs = await refreshVaults();
      await refreshStatuses(vs);
    })();
  }, [refreshVaults, refreshStatuses]);

  useEffect(() => {
    const t = setInterval(() => {
      if (screen === 'editor' && activeIdRef.current) {
        const id = activeIdRef.current;
        void api.getStatus(id).then((st) => setStatuses((p) => ({ ...p, [id]: st }))).catch(() => {});
      } else {
        void refreshVaults().then(refreshStatuses);
      }
    }, 10000);
    return () => clearInterval(t);
  }, [screen, refreshVaults, refreshStatuses]);

  const metaOf = useCallback(
    (v: VaultInfo) => resolveMeta(metaMap, v.vault_id, basename(v.path)),
    [metaMap],
  );

  const vaultMetas: VaultMeta[] = useMemo(
    () =>
      vaults.map((v) => {
        const st = statuses[v.id];
        const m = resolveMeta(metaMap, v.vault_id, basename(v.path));
        return {
          ...v,
          displayName: m.name,
          hue: m.hue,
          emoji: m.emoji,
          peers: st?.peers.length ?? 0,
          lastTs: st?.last_ts ?? null,
          ticket: st?.listening_ticket ?? v.listening_ticket,
        };
      }),
    [vaults, statuses, metaMap],
  );
  const activeMeta = vaultMetas.find((v) => v.id === activeId) || null;
  const activeStatus = activeId ? statuses[activeId] : undefined;

  // ---------- selection + content resolution ----------
  const flushSave = useCallback(async () => {
    if (saveTimer.current) {
      clearTimeout(saveTimer.current);
      saveTimer.current = null;
    }
    const id = activeIdRef.current;
    const path = selectedRef.current;
    if (dirtyRef.current && id && path && !(playheadRef.current != null && playheadRef.current < nowRef.current)) {
      try {
        await api.writeFile(id, path, bufferRef.current);
      } catch {
        /* ignore */
      }
    }
    dirtyRef.current = false;
    setSaving(false);
  }, []);

  useEffect(() => {
    const id = activeId;
    const path = selectedPath;
    if (!id || !path) {
      setPaint(null);
      return;
    }
    const seq = ++paintSeq.current;
    const ph = playhead;
    const live = ph == null || ph >= nowRef.current;
    const key = `${id}::${path}`;

    if (live && key in contentRef.current) {
      const content = contentRef.current[key];
      bufferRef.current = content;
      setDocText(content);
      setPaint({ source: content, readOnly: false, notExist: false, key: `${path}#live#${seq}` });
      return;
    }

    let cancelled = false;
    void (async () => {
      try {
        if (live) {
          const content = await api.readFile(id, path);
          if (cancelled || seq !== paintSeq.current) return;
          contentRef.current[key] = content;
          bufferRef.current = content;
          dirtyRef.current = false;
          setDocText(content);
          setPaint({ source: content, readOnly: false, notExist: false, key: `${path}#live#${seq}` });
        } else {
          const at = await api.readFileAt(id, path, Math.floor(ph / 1000));
          if (cancelled || seq !== paintSeq.current) return;
          setDocText(at.exists ? at.content : '');
          setPaint({ source: at.content, readOnly: true, notExist: !at.exists, key: `${path}#tt${ph}#${seq}` });
        }
      } catch {
        if (!cancelled) setPaint(null);
      }
    })();
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeId, selectedPath, playhead]);

  const onEditorChange = useCallback(
    (src: string) => {
      bufferRef.current = src;
      if (activeIdRef.current && selectedRef.current) contentRef.current[`${activeIdRef.current}::${selectedRef.current}`] = src;
      dirtyRef.current = true;
      setSaving(true);
      if (saveTimer.current) clearTimeout(saveTimer.current);
      saveTimer.current = setTimeout(() => {
        const id = activeIdRef.current;
        const path = selectedRef.current;
        if (!id || !path) return;
        void api
          .writeFile(id, path, bufferRef.current)
          .then(() => {
            dirtyRef.current = false;
            setSaving(false);
            setDocText(bufferRef.current);
            scheduleHistory(id);
          })
          .catch(() => setSaving(false));
      }, 650);
    },
    [scheduleHistory],
  );

  // ---------- vault open / switch ----------
  const openVault = useCallback(
    async (id: string) => {
      await flushSave();
      contentRef.current = {};
      setActiveId(id);
      setScreen('editor');
      setVaultMenuOpen(false);
      setPlayhead(null);
      setView(defaultView(Date.now()));
      setNow(Date.now());
      const fs = await refreshFiles(id);
      const tree = buildTree(fs);
      const sel = firstSelectable(tree);
      const exp: Record<string, boolean> = {};
      if (sel) {
        const parts = sel.split('/');
        for (let i = 1; i < parts.length; i++) exp[parts.slice(0, i).join('/')] = true;
      }
      setExpanded(exp);
      setSelectedPath(sel);
      setSelectedPaths(sel ? new Set([sel]) : new Set());
      setAnchorPath(sel);
      scheduleHistory(id);
    },
    [flushSave, refreshFiles, scheduleHistory],
  );

  const selectFile = useCallback(
    async (path: string) => {
      await flushSave();
      setSelectedPath(path);
    },
    [flushSave],
  );

  const toggleDir = useCallback((path: string) => {
    setExpanded((e) => ({ ...e, [path]: !e[path] }));
  }, []);

  const onToggleExpandAll = useCallback(() => {
    setExpanded((e) => {
      const anyOpen = Object.keys(e).some((k) => e[k]);
      if (anyOpen) return {};
      const out: Record<string, boolean> = {};
      for (const p of allDirPaths(buildTree(filesRef.current))) out[p] = true;
      return out;
    });
  }, []);

  // ---------- file ops ----------
  // Create a file (optionally inside `parent`). Reserves the name + shows the row
  // SYNCHRONOUSLY (before any await) so a rapid second click picks a distinct name.
  const createFile = useCallback(
    async (parent = '') => {
      const id = activeIdRef.current;
      if (!id) return;
      setNewMenuOpen(false);
      setCtxMenu(null);
      const prefix = parent ? parent + '/' : '';
      const siblings = new Set(
        filesRef.current
          .map((f) => f.path)
          .filter((p) => (parent ? p.startsWith(prefix) && !p.slice(prefix.length).includes('/') : !p.includes('/')))
          .map((p) => p.slice(prefix.length)),
      );
      const name = freeName(siblings, '.md');
      const path = prefix + name;
      const prevPath = selectedRef.current;
      const prevDirty = dirtyRef.current;
      const prevBuf = bufferRef.current;
      const content = `# ${name.replace(/\.md$/, '')}\n\n`;
      const next = [...filesRef.current, { path, file_id: path, is_dir: false, merge_class: 'text' }];
      filesRef.current = next;
      setFiles(next);
      contentRef.current[`${id}::${path}`] = content;
      if (parent) setExpanded((e) => ({ ...e, [parent]: true }));
      setSelectedPath(path);
      setSelectedPaths(new Set([path]));
      setAnchorPath(path);
      dirtyRef.current = false;
      bufferRef.current = content;
      try {
        if (prevDirty && prevPath) await api.writeFile(id, prevPath, prevBuf);
        await api.writeFile(id, path, content);
      } catch (err) {
        console.error('new file failed', err);
      }
      scheduleHistory(id);
    },
    [scheduleHistory],
  );

  // Create an empty folder (first-class dir entity) and inline-rename it.
  const createFolder = useCallback(
    async (parent = '') => {
      const id = activeIdRef.current;
      if (!id) return;
      setNewMenuOpen(false);
      setCtxMenu(null);
      const prefix = parent ? parent + '/' : '';
      const siblings = new Set(
        filesRef.current
          .map((f) => f.path)
          .filter((p) => (parent ? p.startsWith(prefix) && !p.slice(prefix.length).includes('/') : !p.includes('/')))
          .map((p) => p.slice(prefix.length)),
      );
      const name = freeName(siblings, '');
      const path = prefix + name;
      const next = [...filesRef.current, { path, file_id: path, is_dir: true, merge_class: 'dir' }];
      filesRef.current = next;
      setFiles(next);
      setExpanded((e) => ({ ...e, ...(parent ? { [parent]: true } : {}), [path]: true }));
      setRenaming(path);
      setRenameValue(name);
      try {
        await api.createDir(id, path);
      } catch (err) {
        console.error('new folder failed', err);
      }
      scheduleHistory(id);
    },
    [scheduleHistory],
  );

  const commitRename = useCallback(
    (oldPath: string, rawName: string) => {
      const id = activeIdRef.current;
      setRenaming(null);
      setCrumbEditing(false);
      const name = rawName.trim();
      if (!id || !name) return;
      const parts = oldPath.split('/');
      parts[parts.length - 1] = name;
      const newPath = parts.join('/');
      if (newPath === oldPath) return;
      const remap = (p: string) => newPath + p.slice(oldPath.length);
      const affected = filesRef.current.filter((f) => f.path === oldPath || f.path.startsWith(oldPath + '/')).map((f) => f.path);
      const pairs: [string, string][] = (affected.length ? affected : [oldPath]).map((p) => [p, remap(p)]);
      const flushOld = dirtyRef.current && selectedRef.current === oldPath ? bufferRef.current : null;
      dirtyRef.current = false;

      const next = filesRef.current.map((f) => (f.path === oldPath || f.path.startsWith(oldPath + '/') ? { ...f, path: remap(f.path) } : f));
      filesRef.current = next;
      setFiles(next);
      for (const [o, n] of pairs) {
        const ok = `${id}::${o}`;
        if (ok in contentRef.current) {
          contentRef.current[`${id}::${n}`] = contentRef.current[ok];
          delete contentRef.current[ok];
        }
      }
      setExpanded((e) => {
        const out: Record<string, boolean> = {};
        for (const k of Object.keys(e)) {
          if (k === oldPath) out[newPath] = e[k];
          else if (k.startsWith(oldPath + '/')) out[remap(k)] = e[k];
          else out[k] = e[k];
        }
        return out;
      });
      if (selectedRef.current === oldPath) setSelectedPath(newPath);
      else if (selectedRef.current && selectedRef.current.startsWith(oldPath + '/')) setSelectedPath(remap(selectedRef.current));
      const remapSel = (p: string) => (p === oldPath || p.startsWith(oldPath + '/') ? remap(p) : p);
      setSelectedPaths((prev) => new Set(Array.from(prev, remapSel)));
      setAnchorPath((p) => (p ? remapSel(p) : p));

      void (async () => {
        try {
          if (flushOld != null) await api.writeFile(id, oldPath, flushOld);
          for (const [o, n] of pairs) await api.renameFile(id, o, n);
        } catch (err) {
          console.error('rename failed', err);
        }
        scheduleHistory(id);
      })();
    },
    [scheduleHistory],
  );

  // Move one or more paths into `destDir` (''=vault root). A move is a rename that
  // swaps the PARENT directory while keeping the base name — so the heavy lifting
  // mirrors commitRename (remap files/content/expanded/selection for whole folder
  // subtrees), just across several sources at once. Guards drop: no-ops (already
  // in dest), a folder dropped into itself/its own descendant, and name
  // collisions at the destination. Skipped sources are silently ignored.
  const movePaths = useCallback(
    (srcPaths: string[], destDir: string) => {
      const id = activeIdRef.current;
      if (!id) return;
      // Drop sources nested under another source — the ancestor's move carries
      // them — and de-dupe.
      const uniq = Array.from(new Set(srcPaths));
      const roots = uniq.filter((p) => !uniq.some((q) => q !== p && p.startsWith(q + '/')));
      const existing = new Set(filesRef.current.map((f) => f.path));
      const moves: { src: string; dst: string }[] = [];
      for (const src of roots) {
        const base = src.includes('/') ? src.slice(src.lastIndexOf('/') + 1) : src;
        const dst = destDir ? destDir + '/' + base : base;
        if (dst === src) continue; // already lives in destDir — no-op
        if (destDir === src || destDir.startsWith(src + '/')) continue; // into itself / a descendant
        if (existing.has(dst)) continue; // name already taken at the destination
        moves.push({ src, dst });
      }
      if (moves.length === 0) return;
      setCtxMenu(null);

      const remap = (p: string): string => {
        for (const { src, dst } of moves) {
          if (p === src || p.startsWith(src + '/')) return dst + p.slice(src.length);
        }
        return p;
      };
      const affected = filesRef.current.filter((f) => remap(f.path) !== f.path).map((f) => f.path);
      const pairs: [string, string][] = affected.map((p) => [p, remap(p)]);
      const flushSel = selectedRef.current;
      const flushOld = dirtyRef.current && flushSel && remap(flushSel) !== flushSel ? bufferRef.current : null;
      dirtyRef.current = false;

      const next = filesRef.current.map((f) => (remap(f.path) !== f.path ? { ...f, path: remap(f.path) } : f));
      filesRef.current = next;
      setFiles(next);
      for (const [o, n] of pairs) {
        const ok = `${id}::${o}`;
        if (ok in contentRef.current) {
          contentRef.current[`${id}::${n}`] = contentRef.current[ok];
          delete contentRef.current[ok];
        }
      }
      setExpanded((e) => {
        const out: Record<string, boolean> = {};
        for (const k of Object.keys(e)) out[remap(k)] = e[k];
        if (destDir) out[destDir] = true; // reveal where things landed
        return out;
      });
      if (flushSel) setSelectedPath(remap(flushSel));
      setSelectedPaths((prev) => new Set(Array.from(prev, remap)));
      setAnchorPath((p) => (p ? remap(p) : p));

      void (async () => {
        try {
          if (flushOld != null && flushSel) await api.writeFile(id, flushSel, flushOld);
          for (const [o, n] of pairs) await api.renameFile(id, o, n);
        } catch (err) {
          console.error('move failed', err);
        }
        scheduleHistory(id);
      })();
    },
    [scheduleHistory],
  );

  // Drag-and-drop entry point from the file tree: move the dragged node (or the
  // whole multi-selection if it's part of one) into `destDir`.
  const onMove = useCallback(
    (srcPaths: string[], destDir: string) => movePaths(srcPaths, destDir),
    [movePaths],
  );

  // Delete one or more paths (folders delete their whole subtree). Drives both the
  // single-file delete and the batch delete of a multi-selection.
  const deletePaths = useCallback(
    (paths: string[]) => {
      const id = activeIdRef.current;
      if (!id || paths.length === 0) return;
      setCtxMenu(null);
      const victimSet = new Set<string>();
      for (const p of paths) {
        victimSet.add(p);
        for (const f of filesRef.current) if (f.path === p || f.path.startsWith(p + '/')) victimSet.add(f.path);
      }
      const next = filesRef.current.filter((f) => !victimSet.has(f.path));
      filesRef.current = next;
      setFiles(next);
      for (const p of victimSet) delete contentRef.current[`${id}::${p}`];
      const sel = selectedRef.current;
      if (sel && victimSet.has(sel)) {
        const fallback = firstSelectable(buildTree(next));
        setSelectedPath(fallback);
        setSelectedPaths(fallback ? new Set([fallback]) : new Set());
        setAnchorPath(fallback);
      } else {
        setSelectedPaths((prev) => {
          const np = new Set(prev);
          for (const p of victimSet) np.delete(p);
          return np;
        });
      }
      void (async () => {
        for (const p of victimSet) {
          try {
            await api.deleteFile(id, p);
          } catch (err) {
            console.error('delete failed', p, err);
          }
        }
        scheduleHistory(id);
      })();
    },
    [scheduleHistory],
  );

  // Context-menu / programmatic delete of a single node. If the node is a FILE
  // that's part of a multi-selection, delete the whole selection (batch delete);
  // otherwise just this node (today's behavior).
  const deleteNode = useCallback(
    (path: string, isDir: boolean) => {
      const sel = selectedPathsRef.current;
      if (!isDir && sel.size > 1 && sel.has(path)) deletePaths(Array.from(sel));
      else deletePaths([path]);
    },
    [deletePaths],
  );

  // Keyboard: Delete/Backspace removes the whole current selection; Escape
  // collapses a multi-selection back to just the active file. Guarded so it never
  // fires while typing in an input/textarea or the contenteditable editor.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const t = e.target as HTMLElement | null;
      if (t && (t.tagName === 'INPUT' || t.tagName === 'TEXTAREA' || t.isContentEditable)) return;
      if (e.key === 'Escape') {
        const active = selectedRef.current;
        setSelectedPaths(active ? new Set([active]) : new Set());
        return;
      }
      if (e.key === 'Delete' || e.key === 'Backspace') {
        const sel = selectedPathsRef.current;
        if (sel.size === 0) return;
        e.preventDefault();
        deletePaths(Array.from(sel));
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [deletePaths]);

  const openCtx = useCallback((e: React.MouseEvent, node: { path: string; isDir: boolean; name: string }) => {
    e.preventDefault();
    e.stopPropagation();
    setVaultMenuOpen(false);
    setCtxMenu({ x: Math.min(e.clientX, window.innerWidth - 184), y: Math.min(e.clientY, window.innerHeight - 110), path: node.path, isDir: node.isDir, name: node.name });
  }, []);

  const openTreeCtx = useCallback((e: React.MouseEvent) => {
    e.preventDefault();
    setVaultMenuOpen(false);
    setCtxMenu({ x: Math.min(e.clientX, window.innerWidth - 184), y: Math.min(e.clientY, window.innerHeight - 130), root: true });
  }, []);

  // The directory a context-menu "New …" should create into.
  const ctxTargetDir = useCallback((c: CtxMenu): string => {
    if (c.root || !c.path) return '';
    if (c.isDir) return c.path;
    return c.path.includes('/') ? c.path.slice(0, c.path.lastIndexOf('/')) : '';
  }, []);

  // ---------- history track ----------
  const onNow = useCallback(() => {
    setPlayhead(null);
    setView((v) => viewForNow(v || defaultView(nowRef.current), nowRef.current));
  }, []);

  const onRestoreHere = useCallback(async () => {
    const id = activeIdRef.current;
    const path = selectedRef.current;
    const ph = playheadRef.current;
    if (!id || !path || ph == null) {
      setPlayhead(null);
      return;
    }
    try {
      await api.restoreFileAt(id, path, Math.floor(ph / 1000));
    } catch {
      /* ignore */
    }
    delete contentRef.current[`${id}::${path}`];
    setPlayhead(null);
    await refreshFiles(id);
    scheduleHistory(id);
  }, [refreshFiles, scheduleHistory]);

  const onTabHistory = useCallback(() => {
    setHistOpen((h) => !h);
    setLogOpen(false);
  }, []);
  const onTabLog = useCallback(() => {
    setLogOpen((l) => !l);
    setHistOpen(false);
  }, []);

  // ---------- sidebar resize ----------
  const onSidebarResize = useCallback(
    (e: React.PointerEvent) => {
      e.preventDefault();
      const startX = e.clientX;
      const w0 = sidebarW;
      let latest = w0;
      const move = (ev: PointerEvent) => {
        latest = clampSidebar(w0 + (ev.clientX - startX));
        setSidebarW(latest);
      };
      const up = () => {
        document.removeEventListener('pointermove', move);
        document.removeEventListener('pointerup', up);
        document.body.style.cursor = '';
        updatePrefs({ sidebarW: latest });
      };
      document.body.style.cursor = 'col-resize';
      document.addEventListener('pointermove', move);
      document.addEventListener('pointerup', up);
    },
    [sidebarW, updatePrefs],
  );

  // ---------- history/log bar resize ----------
  // The bar lives at the bottom and grows UPWARD, so dragging up (clientY
  // decreasing) makes it taller. One shared height drives whichever panel is
  // open. Drag below the collapse threshold → snap fully shut; dragging back up
  // within the same gesture re-opens the tab we started from.
  const onHistBarResize = useCallback(
    (e: React.PointerEvent) => {
      e.preventDefault();
      const startY = e.clientY;
      const h0 = histBarH;
      const wasHist = histOpen;
      const wasLog = logOpen;
      let collapsed = false;
      let latest = h0;
      setResizingBar(true);
      const move = (ev: PointerEvent) => {
        const proposed = h0 - (ev.clientY - startY);
        if (proposed < HISTBAR_COLLAPSE) {
          collapsed = true;
          setHistOpen(false);
          setLogOpen(false);
        } else {
          if (collapsed) {
            collapsed = false;
            setHistOpen(wasHist);
            setLogOpen(wasLog);
          }
          latest = clampHistBar(proposed);
          setHistBarH(latest);
        }
      };
      const up = () => {
        document.removeEventListener('pointermove', move);
        document.removeEventListener('pointerup', up);
        document.body.style.cursor = '';
        setResizingBar(false);
        if (!collapsed) updatePrefs({ histBarH: latest });
      };
      document.body.style.cursor = 'row-resize';
      document.addEventListener('pointermove', move);
      document.addEventListener('pointerup', up);
    },
    [histBarH, histOpen, logOpen, updatePrefs],
  );

  // ---------- theme / font ----------
  const onToggleTheme = useCallback(() => {
    const next = prefs.theme === 'dark' ? 'light' : 'dark';
    applyTheme(next);
    updatePrefs({ theme: next });
  }, [prefs.theme, updatePrefs]);
  const onToggleFont = useCallback(() => {
    const cur = prefs.fontOverride || prefs.font;
    updatePrefs({ fontOverride: cur === 'Serif' ? 'Sans' : 'Serif' });
  }, [prefs.font, prefs.fontOverride, updatePrefs]);
  const fontIsSerif = (prefs.fontOverride || prefs.font) === 'Serif';

  // ---------- connect / new / share / remove / customize ----------
  const onOpenFolder = useCallback(async () => {
    try {
      const dir = await open({ directory: true });
      if (typeof dir === 'string') {
        const info = await api.addLocalFolder(dir);
        await refreshVaults();
        await openVault(info.id);
      }
    } catch (err) {
      console.error('open folder failed', err);
      alert('Could not open that folder: ' + String((err as Error)?.message ?? err));
    }
  }, [openVault, refreshVaults]);

  const onChooseDest = useCallback(async () => {
    const dir = await open({ directory: true });
    if (typeof dir === 'string') setConnectDest(dir);
  }, []);

  const onEntrySubmit = useCallback(async () => {
    if (entry === 'connect') {
      if (connecting) return;
      const t = ticket.trim();
      // Desktop needs a destination folder; web clones straight into OPFS.
      if (!t || (desktop && !connectDest)) return;
      setConnecting(true);
      try {
        const info = await api.cloneRemote(connectDest || '', t, authKey || undefined);
        setTicket('');
        setAuthKey('');
        setConnectDest(null);
        setEntry(null);
        await refreshVaults();
        await openVault(info.id);
      } catch (err) {
        console.error('clone failed', err);
      } finally {
        setConnecting(false);
      }
    } else {
      // New vault: desktop adds a chosen folder; web creates a browser (OPFS) vault.
      if (desktop && !connectDest) return;
      try {
        const nm = newVaultName.trim();
        const info = desktop ? await api.addLocalFolder(connectDest!) : await api.createVault(nm || 'Untitled vault');
        if (nm) updateMeta(info.vault_id, { name: nm, hue: hueForId(info.vault_id), emoji: null });
        setNewVaultName('');
        setConnectDest(null);
        setEntry(null);
        await refreshVaults();
        await openVault(info.id);
      } catch (err) {
        console.error('create vault failed', err);
      }
    }
  }, [entry, connecting, ticket, connectDest, authKey, newVaultName, desktop, updateMeta, openVault, refreshVaults]);

  const onShareVault = useCallback(async (id: string) => {
    setVaultMenuOpen(false);
    setShare({ id, code: '', requireKey: false, accessKey: '', copied: false });
    try {
      const tkt = await api.setAllowConnections(id, true);
      setShare((s) => (s && s.id === id ? { ...s, code: tkt || '' } : s));
      await api.getStatus(id).then((st) => setStatuses((p) => ({ ...p, [id]: st })));
    } catch (err) {
      console.error('share failed', err);
    }
  }, []);

  const onToggleRequireKey = useCallback(async () => {
    const s = share;
    if (!s) return;
    if (!s.requireKey) {
      const key = makeAccessKey();
      setShare({ ...s, requireKey: true, accessKey: key });
      try {
        await api.setAllowConnections(s.id, false);
        const tkt = await api.setAllowConnections(s.id, true, key);
        setShare((x) => (x && x.id === s.id ? { ...x, code: tkt || '', accessKey: key, requireKey: true } : x));
      } catch (err) {
        console.error(err);
      }
    } else {
      setShare({ ...s, requireKey: false, accessKey: '' });
      try {
        await api.setAllowConnections(s.id, false);
        const tkt = await api.setAllowConnections(s.id, true);
        setShare((x) => (x && x.id === s.id ? { ...x, code: tkt || '', accessKey: '', requireKey: false } : x));
      } catch (err) {
        console.error(err);
      }
    }
  }, [share]);

  const onCopyCode = useCallback(async () => {
    const s = share;
    if (!s) return;
    const text = s.requireKey ? `${s.code}\nAccess key: ${s.accessKey}` : s.code;
    try {
      await navigator.clipboard.writeText(text);
    } catch {
      /* ignore */
    }
    setShare((x) => (x ? { ...x, copied: true } : x));
    setTimeout(() => setShare((x) => (x ? { ...x, copied: false } : x)), 1400);
  }, [share]);

  const openCustomize = useCallback(
    (v: VaultInfo) => {
      const m = metaOf(v);
      setVaultMenuOpen(false);
      setVaultCtx(null);
      setCustomize({ id: v.vault_id, name: m.name, hue: m.hue, emoji: m.emoji });
    },
    [metaOf],
  );

  const confirmRemove = useCallback(async () => {
    const rm = removeVaultState;
    if (!rm) return;
    setRemoveVaultState(null);
    try {
      await api.removeVault(rm.id, rm.trash);
    } catch (err) {
      console.error(err);
    }
    await refreshVaults();
    if (rm.id === activeIdRef.current) {
      setActiveId(null);
      setScreen('connect');
      setSelectedPath(null);
      setSelectedPaths(new Set());
      setAnchorPath(null);
    }
  }, [removeVaultState, refreshVaults]);

  // ---------- derived view-model ----------
  const tree = useMemo(() => buildTree(files), [files]);
  const view2 = curView();
  const playT = playhead == null ? now : playhead;
  const filterTs = timeTravel ? playhead : null;
  const createTs = useMemo(() => createTsByPath(events), [events]);

  const fileVisible = (path: string) => filterTs == null || (createTs[path] != null && createTs[path] <= filterTs);
  const dirVisible = (path: string) => filterTs == null || files.some((f) => !f.is_dir && (f.path === path || f.path.startsWith(path + '/')) && fileVisible(f.path));

  const rows = useMemo(() => {
    const flat = flatten(tree, expanded);
    return flat.filter((r) => {
      if (!prefs.showHidden && isHidden(r.node.name)) return false;
      return r.node.type === 'dir' ? dirVisible(r.node.path) : fileVisible(r.node.path);
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [tree, expanded, filterTs, files, prefs.showHidden]);

  // Click on a FILE row, honoring the keyboard modifiers (like a normal file
  // manager / IDE). Plain → select just this file + open it. Cmd/Ctrl → toggle it
  // in/out of the multi-selection. Shift → range-select from the anchor through
  // this row across the flattened visible FILE order. The clicked file always
  // becomes the active/editor file (except a cmd-click that DESELECTS it).
  // Declared after `rows` because the shift-range slices the visible file order.
  const onFileClick = useCallback(
    (path: string, e: { shiftKey: boolean; metaKey: boolean; ctrlKey: boolean }) => {
      if (e.shiftKey && anchorPath) {
        const fileRows = rows.filter((r) => r.node.type === 'file').map((r) => r.node.path);
        const a = fileRows.indexOf(anchorPath);
        const b = fileRows.indexOf(path);
        if (a >= 0 && b >= 0) {
          const [lo, hi] = a <= b ? [a, b] : [b, a];
          setSelectedPaths(new Set(fileRows.slice(lo, hi + 1)));
          void selectFile(path);
          return; // keep the existing anchor for further shift-clicks
        }
      }
      if (e.metaKey || e.ctrlKey) {
        const has = selectedPaths.has(path);
        const next = new Set(selectedPaths);
        if (has) next.delete(path);
        else next.add(path);
        setSelectedPaths(next);
        setAnchorPath(path);
        if (!has) void selectFile(path);
        else if (path === selectedPath) {
          // Deselected the active file → move the editor to another selected file.
          const remaining = Array.from(next);
          if (remaining.length) void selectFile(remaining[remaining.length - 1]);
        }
        return;
      }
      setSelectedPaths(new Set([path]));
      setAnchorPath(path);
      void selectFile(path);
    },
    [rows, anchorPath, selectedPaths, selectedPath, selectFile],
  );

  const ctxTargetPath = ctxMenu && !ctxMenu.root ? ctxMenu.path ?? null : null;

  const selParts = selectedPath ? selectedPath.split('/') : [];
  const crumbFile = selParts.length ? selParts[selParts.length - 1] : '';
  const crumbDir = selParts.length > 1 ? selParts.slice(0, -1).join(' / ') + ' / ' : '';
  const count = selectedPath ? countLabel(docText, selectedPath) : '';
  // Submit is blocked until the modal has what it needs — desktop additionally
  // requires a chosen destination folder; web needs none (it writes to OPFS).
  const entryBlocked = entry === 'connect' ? connecting || !ticket.trim() || (desktop && !connectDest) : desktop && !connectDest;

  // ===================================================================
  // RENDER
  // ===================================================================
  const themeBtn = (style: React.CSSProperties) => (
    <button onClick={onToggleTheme} title="Toggle theme" className="asp-icon-btn" style={style}>
      <Icon.ThemeIcon dark={prefs.theme === 'dark'} />
    </button>
  );

  const renderConnect = () => {
    const saved = vaultMetas;
    return (
      <div style={{ position: 'fixed', inset: 0, display: 'flex', flexDirection: 'column', alignItems: 'center', justifyContent: 'center', background: 'var(--bg-sub)', color: 'var(--text)', padding: 32, overflow: 'auto' }}>
        <div style={{ width: 'min(452px, 94vw)', display: 'flex', flexDirection: 'column' }}>
          <div style={{ display: 'flex', alignItems: 'center', gap: 11, marginBottom: 34 }}>
            <div style={{ width: 26, height: 26, borderRadius: 7, background: accent, display: 'flex', alignItems: 'center', justifyContent: 'center', flex: 'none' }}>
              <div style={{ width: 9, height: 9, borderRadius: '50%', background: 'var(--bg)' }} />
            </div>
            <div style={{ fontFamily: "'JetBrains Mono', monospace", fontWeight: 600, fontSize: 16, letterSpacing: '-0.01em' }}>asp</div>
            <span style={{ flex: 1 }} />
            <div style={{ display: 'flex', alignItems: 'center', gap: 6, fontSize: 12, color: 'var(--faint)' }}>
              <span style={{ width: 8, height: 8, borderRadius: 2, background: desktop ? accent : 'var(--faint2)', display: 'inline-block', flex: 'none' }} />
              <span>{desktop ? 'On this computer' : 'Saved in this browser'}</span>
            </div>
            {themeBtn({ display: 'flex', alignItems: 'center', justifyContent: 'center', width: 28, height: 28, flex: 'none', border: '1px solid var(--line)', background: 'var(--bg)', color: 'var(--text3)', borderRadius: 8, cursor: 'pointer', padding: 0 })}
          </div>

          <h1 style={{ fontSize: 25, fontWeight: 600, letterSpacing: '-0.02em', margin: '0 0 22px' }}>Your vaults</h1>

          <div style={{ display: 'flex', gap: 10 }}>
            <button onClick={() => { setEntry('new'); setNewVaultName(''); setConnectDest(null); }} style={{ flex: 1, display: 'flex', alignItems: 'center', justifyContent: 'center', gap: 8, height: 46, border: 'none', borderRadius: 11, background: 'var(--text)', color: 'var(--bg)', fontSize: 14, fontWeight: 500, fontFamily: 'inherit', cursor: 'pointer', boxShadow: '0 1px 2px rgba(28,25,23,0.18)' }}>
              <Icon.PlusIcon size={16} stroke="currentColor" />
              <span>New Vault</span>
            </button>
            <button onClick={() => { setEntry('connect'); setTicket(''); setAuthKey(''); setConnectDest(null); }} style={{ flex: 1, display: 'flex', alignItems: 'center', justifyContent: 'center', gap: 8, height: 46, padding: '0 14px', border: '1px solid var(--line)', borderRadius: 11, background: 'var(--bg)', color: 'var(--text2)', fontSize: 14, fontWeight: 500, fontFamily: 'inherit', cursor: 'pointer' }}>
              <Icon.ConnectIcon size={15} stroke="currentColor" />
              <span>Connect Vault</span>
            </button>
          </div>

          {saved.length > 0 && (
            <>
              <div style={{ marginTop: 26, marginBottom: 9, paddingLeft: 3, fontSize: 11, fontWeight: 600, letterSpacing: '0.07em', textTransform: 'uppercase', color: 'var(--faint2)' }}>Recent vaults</div>
              <div style={{ border: '1px solid var(--line)', borderRadius: 14, overflow: 'hidden', background: 'var(--bg)' }}>
                {saved.map((v, i) => (
                  <div key={v.id} className="asp-hover-list" onClick={() => void openVault(v.id)} onContextMenu={(e) => { e.preventDefault(); setVaultCtx({ x: Math.min(e.clientX, window.innerWidth - 188), y: Math.min(e.clientY, window.innerHeight - 70), id: v.id, vaultId: v.vault_id, name: v.displayName }); }} style={{ display: 'flex', alignItems: 'center', gap: 13, padding: '13px 15px', cursor: 'pointer', borderTop: i > 0 ? '1px solid var(--line)' : 'none' }}>
                    <div style={avatarStyle({ hue: v.hue, emoji: v.emoji }, 34, 10)}>{glyphOf({ emoji: v.emoji, name: v.displayName })}</div>
                    <div style={{ flex: 1, minWidth: 0 }}>
                      <div style={{ fontSize: 14.5, fontWeight: 500, color: 'var(--text)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{v.displayName}</div>
                      <div style={{ display: 'flex', alignItems: 'center', gap: 6, marginTop: 3, minWidth: 0 }}>
                        {desktop ? <Icon.FolderIcon size={12} stroke="var(--faint2)" /> : <Icon.GlobeIcon size={12} stroke="var(--faint2)" />}
                        <span style={{ fontFamily: desktop ? "'JetBrains Mono', monospace" : 'inherit', fontSize: 11, color: 'var(--faint)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{desktop ? v.path : 'Using browser storage'}</span>
                      </div>
                    </div>
                    <span style={{ fontSize: 11.5, color: 'var(--faint)', flex: 'none' }}>{relTime(v.lastTs)}</span>
                    <Icon.ChevronRight size={15} stroke="#cfc9c1" style={{ flex: 'none' }} />
                  </div>
                ))}
              </div>
            </>
          )}

          <div style={{ marginTop: 28, fontSize: 11.5, color: 'var(--faint2)', display: 'flex', alignItems: 'center', gap: 7 }}>
            <Icon.UserIcon />
            <span>This device · {shortFingerprint(identity)}</span>
          </div>
        </div>
      </div>
    );
  };

  const renderEditor = () => {
    const hasSelection = !!selectedPath;
    const syncSummary = activeMeta && activeMeta.peers > 0 ? `Synced · ${activeMeta.peers} peer${activeMeta.peers === 1 ? '' : 's'}` : 'Synced';
    const anyExpanded = Object.keys(expanded).some((k) => expanded[k]);

    return (
      <div style={{ position: 'fixed', inset: 0, display: 'flex', flexDirection: 'column', background: 'var(--bg)', color: 'var(--text)', fontSize: 14 }}>
        <div style={{ flex: 1, minHeight: 0, display: 'flex' }}>
          {/* sidebar */}
          <aside style={{ width: sidebarW, flex: 'none', display: 'flex', flexDirection: 'column', background: 'var(--bg-sub)' }}>
            <div style={{ position: 'relative', borderBottom: '1px solid var(--line)' }}>
              <div className="asp-hover-row" data-testid="vault-switcher" onClick={() => setVaultMenuOpen((v) => !v)} style={{ display: 'flex', alignItems: 'center', gap: 11, height: 47, padding: '0 14px', boxSizing: 'border-box', cursor: 'pointer' }}>
                <div style={avatarStyle({ hue: activeMeta?.hue ?? 222, emoji: activeMeta?.emoji }, 28, 8)}>{glyphOf({ emoji: activeMeta?.emoji, name: activeMeta?.displayName })}</div>
                <div style={{ flex: 1, minWidth: 0 }}>
                  <div style={{ fontSize: 14, fontWeight: 600, letterSpacing: '-0.01em', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{activeMeta?.displayName || 'Vault'}</div>
                  <div style={{ display: 'flex', alignItems: 'center', gap: 6, marginTop: 2 }}>
                    <span style={{ width: 6, height: 6, borderRadius: '50%', background: accent, animation: 'aspPulse 2.4s ease-in-out infinite', flex: 'none' }} />
                    <span style={{ fontSize: 11, color: 'var(--faint)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{syncSummary}</span>
                  </div>
                </div>
                <Icon.CaretDown style={{ flex: 'none', transition: 'transform .15s', transform: vaultMenuOpen ? 'rotate(180deg)' : 'rotate(0deg)' }} />
              </div>

              {vaultMenuOpen && (
                <>
                  <div onClick={() => setVaultMenuOpen(false)} style={{ position: 'fixed', inset: 0, zIndex: 40 }} />
                  <div style={{ position: 'absolute', top: 'calc(100% - 4px)', left: 8, right: 8, zIndex: 41, background: 'var(--bg)', border: '1px solid var(--line)', borderRadius: 12, boxShadow: '0 12px 32px rgba(28,25,23,0.13)', padding: 6, display: 'flex', flexDirection: 'column', gap: 2 }}>
                    <div style={{ fontSize: 10.5, fontWeight: 600, letterSpacing: '0.06em', textTransform: 'uppercase', color: 'var(--faint2)', padding: '7px 9px 4px' }}>Switch vault</div>
                    {vaultMetas.map((v) => (
                      <div key={v.id} className="asp-hover-soft" onClick={() => void openVault(v.id)} style={{ display: 'flex', alignItems: 'center', gap: 11, padding: '8px 9px', borderRadius: 8, cursor: 'pointer' }}>
                        <div style={avatarStyle({ hue: v.hue, emoji: v.emoji }, 26, 8)}>{glyphOf({ emoji: v.emoji, name: v.displayName })}</div>
                        <div style={{ flex: 1, minWidth: 0 }}>
                          <div style={{ fontSize: 13.5, fontWeight: 500, color: 'var(--text)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{v.displayName}</div>
                          <div style={{ display: 'flex', alignItems: 'center', gap: 5, marginTop: 1, minWidth: 0 }}>
                            <Icon.FolderIcon size={12} stroke="var(--faint2)" />
                            <span style={{ fontFamily: "'JetBrains Mono', monospace", fontSize: 10.5, color: 'var(--faint2)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{v.path}</span>
                          </div>
                        </div>
                        {v.id === activeId && <Icon.CheckIcon stroke={accent} style={{ flex: 'none' }} />}
                      </div>
                    ))}
                    <div style={{ height: 1, background: 'var(--line)', margin: '4px 6px' }} />
                    <div className="asp-hover-soft" onClick={() => activeMeta && openCustomize(activeMeta)} style={{ display: 'flex', alignItems: 'center', gap: 10, padding: '8px 9px', borderRadius: 8, cursor: 'pointer', color: 'var(--text2)' }}>
                      <Icon.WandIcon style={{ flex: 'none' }} />
                      <span style={{ fontSize: 13.5 }}>Customize this vault…</span>
                    </div>
                    <div className="asp-hover-soft" onClick={() => activeId && void onShareVault(activeId)} style={{ display: 'flex', alignItems: 'center', gap: 10, padding: '8px 9px', borderRadius: 8, cursor: 'pointer', color: 'var(--text2)' }}>
                      <Icon.ShareIcon style={{ flex: 'none' }} />
                      <span style={{ fontSize: 13.5 }}>Share this vault…</span>
                    </div>
                    <div className="asp-hover-soft" onClick={() => { setVaultMenuOpen(false); if (activeMeta) setRemoveVaultState({ id: activeMeta.id, name: activeMeta.displayName, path: activeMeta.path, trash: false }); }} style={{ display: 'flex', alignItems: 'center', gap: 10, padding: '8px 9px', borderRadius: 8, cursor: 'pointer', color: 'var(--text2)' }}>
                      <Icon.TrashIcon stroke="var(--text2)" style={{ flex: 'none' }} />
                      <span style={{ fontSize: 13.5 }}>Remove this vault…</span>
                    </div>
                    <div style={{ height: 1, background: 'var(--line)', margin: '4px 6px' }} />
                    <div className="asp-hover-soft" onClick={() => { setVaultMenuOpen(false); if (desktop) void onOpenFolder(); else { setEntry('new'); setNewVaultName(''); setConnectDest(null); } }} style={{ display: 'flex', alignItems: 'center', gap: 10, padding: '8px 9px', borderRadius: 8, cursor: 'pointer', color: 'var(--text2)' }}>
                      {desktop ? <Icon.FolderIcon stroke="var(--text2)" /> : <Icon.PlusIcon size={15} stroke="var(--text2)" />}
                      <span style={{ fontSize: 13.5 }}>{desktop ? 'Open another folder…' : 'New vault…'}</span>
                    </div>
                  </div>
                </>
              )}
            </div>

            <div style={{ display: 'flex', alignItems: 'center', gap: 1, padding: '9px 9px 7px', position: 'relative' }}>
              <span style={{ fontSize: 11, fontWeight: 600, letterSpacing: '0.06em', textTransform: 'uppercase', color: 'var(--faint2)', flex: 1, paddingLeft: 3 }}>Files</span>
              <button className="asp-icon-btn" onClick={() => setNewMenuOpen((v) => !v)} title="New note" style={{ display: 'flex', alignItems: 'center', justifyContent: 'center', width: 24, height: 24, border: 'none', background: newMenuOpen ? 'var(--line)' : 'transparent', color: newMenuOpen ? 'var(--text)' : 'var(--text3)', borderRadius: 6, cursor: 'pointer', padding: 0 }}>
                <Icon.PlusIcon />
              </button>
              {newMenuOpen && (
                <>
                  <div onClick={() => setNewMenuOpen(false)} style={{ position: 'fixed', inset: 0, zIndex: 44 }} />
                  <div style={{ position: 'absolute', top: 34, right: 38, zIndex: 45, width: 168, background: 'var(--bg)', border: '1px solid var(--line)', borderRadius: 11, boxShadow: '0 12px 32px rgba(28,25,23,0.14)', padding: 5 }}>
                    <div className="asp-hover-soft" onClick={() => void createFile('')} style={{ display: 'flex', alignItems: 'center', gap: 9, padding: '8px 10px', borderRadius: 7, cursor: 'pointer', fontSize: 13, color: 'var(--text)' }}>
                      <Icon.NewFileIcon style={{ flex: 'none' }} />
                      <span>New file</span>
                    </div>
                    <div className="asp-hover-soft" onClick={() => void createFolder('')} style={{ display: 'flex', alignItems: 'center', gap: 9, padding: '8px 10px', borderRadius: 7, cursor: 'pointer', fontSize: 13, color: 'var(--text)' }}>
                      <Icon.NewFolderIcon style={{ flex: 'none' }} />
                      <span>New folder</span>
                    </div>
                  </div>
                </>
              )}
              <button className="asp-icon-btn" onClick={onToggleExpandAll} title={anyExpanded ? 'Collapse all' : 'Expand all'} style={{ display: 'flex', alignItems: 'center', justifyContent: 'center', width: 24, height: 24, border: 'none', background: 'transparent', color: 'var(--text3)', borderRadius: 6, cursor: 'pointer', padding: 0 }}>
                <Icon.ExpandCollapseIcon expanded={anyExpanded} />
              </button>
              <button className="asp-icon-btn" onClick={() => setFilesMenuOpen((v) => !v)} title="More" style={{ display: 'flex', alignItems: 'center', justifyContent: 'center', width: 24, height: 24, border: 'none', background: filesMenuOpen ? 'var(--line)' : 'transparent', color: filesMenuOpen ? 'var(--text)' : 'var(--text3)', borderRadius: 6, cursor: 'pointer', padding: 0 }}>
                <Icon.DotsIcon />
              </button>
              {filesMenuOpen && (
                <>
                  <div onClick={() => setFilesMenuOpen(false)} style={{ position: 'fixed', inset: 0, zIndex: 44 }} />
                  <div style={{ position: 'absolute', top: 34, right: 8, zIndex: 45, width: 186, background: 'var(--bg)', border: '1px solid var(--line)', borderRadius: 11, boxShadow: '0 12px 32px rgba(28,25,23,0.14)', padding: 5 }}>
                    <div className="asp-hover-soft" onClick={() => { updatePrefs({ showHidden: !prefs.showHidden }); setFilesMenuOpen(false); }} style={{ display: 'flex', alignItems: 'center', gap: 10, padding: '8px 10px', borderRadius: 7, cursor: 'pointer', fontSize: 13, color: 'var(--text)' }}>
                      <span style={{ width: 15, display: 'inline-flex', justifyContent: 'center', flex: 'none' }}>
                        {prefs.showHidden ? <Icon.EyeIcon off={false} stroke={accent} /> : <Icon.EyeIcon off stroke="var(--text2)" />}
                      </span>
                      <span>Show hidden files</span>
                    </div>
                    <div className="asp-hover-soft" onClick={() => { updatePrefs({ prettyNames: !prefs.prettyNames }); setFilesMenuOpen(false); }} style={{ display: 'flex', alignItems: 'center', gap: 10, padding: '8px 10px', borderRadius: 7, cursor: 'pointer', fontSize: 13, color: 'var(--text)' }}>
                      <span style={{ width: 15, display: 'inline-flex', justifyContent: 'center', flex: 'none' }}>
                        {prefs.prettyNames ? <Icon.CheckIcon size={15} stroke={accent} /> : <span style={{ width: 15 }} />}
                      </span>
                      <span>Pretty filenames</span>
                    </div>
                  </div>
                </>
              )}
            </div>

            <FileTree
              rows={rows}
              selectedPath={selectedPath}
              selectedPaths={selectedPaths}
              expanded={expanded}
              renaming={renaming}
              renameValue={renameValue}
              accent={accent}
              accentSoft={accentSoft}
              prettyNames={prefs.prettyNames}
              ctxTargetPath={ctxTargetPath}
              onMove={onMove}
              onEmptyContext={openTreeCtx}
              onRowClick={({ node }, e) => {
                if (renaming === node.path) return;
                if (node.type === 'dir') toggleDir(node.path);
                else onFileClick(node.path, e);
              }}
              onRowContext={openCtx}
              onRenameChange={setRenameValue}
              onRenameKey={(e, path) => {
                if (e.key === 'Enter') { e.preventDefault(); void commitRename(path, renameValue); }
                else if (e.key === 'Escape') setRenaming(null);
              }}
              onRenameCommit={(path) => void commitRename(path, renameValue)}
            />
          </aside>

          {/* resize handle */}
          <div onPointerDown={onSidebarResize} className="sb-resize" style={{ width: 7, flex: 'none', cursor: 'col-resize', margin: '0 -3px', zIndex: 6, position: 'relative', display: 'flex', justifyContent: 'center' }}>
            <div className="sb-line" style={{ width: 1, alignSelf: 'stretch', background: 'var(--line)' }} />
          </div>

          {/* main */}
          <main style={{ flex: 1, display: 'flex', flexDirection: 'column', minWidth: 0 }}>
            {hasSelection ? (
              <>
                <div style={{ height: 48, flex: 'none', display: 'flex', alignItems: 'center', gap: 10, padding: '0 16px', borderBottom: '1px solid var(--line)' }}>
                  <div style={{ flex: 1, minWidth: 0, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', fontSize: 13 }}>
                    <span style={{ color: 'var(--faint2)' }}>{crumbDir}</span>
                    {crumbEditing ? (
                      <input
                        autoFocus
                        value={renameValue}
                        spellCheck={false}
                        onChange={(e) => setRenameValue(e.target.value)}
                        onKeyDown={(e) => { if (e.key === 'Enter') { e.preventDefault(); if (selectedPath) void commitRename(selectedPath, renameValue); } else if (e.key === 'Escape') setCrumbEditing(false); }}
                        onBlur={() => { if (selectedPath) void commitRename(selectedPath, renameValue); }}
                        style={{ fontSize: 13, fontWeight: 500, fontFamily: 'inherit', color: 'var(--text)', background: 'var(--bg)', border: `1px solid ${accent}`, borderRadius: 5, padding: '1px 6px', outline: 'none', minWidth: 180 }}
                      />
                    ) : (
                      <span onDoubleClick={() => { setRenameValue(crumbFile); setCrumbEditing(true); }} title="Double-click to rename" style={{ color: 'var(--text)', fontWeight: 500, cursor: 'text' }}>{crumbFile}</span>
                    )}
                  </div>
                  <div style={{ display: 'flex', alignItems: 'center', gap: 6, flex: 'none' }}>
                    <span style={{ width: 7, height: 7, borderRadius: '50%', flex: 'none', background: saving ? '#d9a93d' : '#3fa45a', transition: 'background .2s' }} />
                    <span style={{ fontSize: 12, color: 'var(--faint)', whiteSpace: 'nowrap' }}>{saving ? 'Saving…' : 'Saved'}</span>
                  </div>
                  <div style={{ width: 1, height: 16, background: 'var(--line)', flex: 'none' }} />
                  <span style={{ fontSize: 12, color: 'var(--faint2)', fontVariantNumeric: 'tabular-nums', flex: 'none' }}>{count}</span>
                  <div style={{ width: 1, height: 16, background: 'var(--line)', flex: 'none', marginLeft: 2 }} />
                  <div style={{ display: 'flex', alignItems: 'center', gap: 1, flex: 'none' }}>
                    <button className="asp-icon-btn" onClick={onToggleFont} title={fontIsSerif ? 'Reading font: Serif' : 'Reading font: Sans'} style={{ display: 'flex', alignItems: 'center', justifyContent: 'center', width: 28, height: 26, flex: 'none', border: 'none', background: 'transparent', borderRadius: 6, cursor: 'pointer', padding: 0, color: 'var(--text3)', opacity: fontIsSerif ? 1 : 0.45 }}>
                      <span style={{ fontFamily: "'Newsreader',Georgia,serif", fontSize: 16, fontWeight: 500, lineHeight: 1, display: 'block' }}>A</span>
                    </button>
                    {themeBtn({ display: 'flex', alignItems: 'center', justifyContent: 'center', width: 28, height: 26, flex: 'none', border: 'none', background: 'transparent', color: 'var(--text3)', borderRadius: 7, cursor: 'pointer', padding: 0 })}
                  </div>
                </div>

                {timeTravel && (
                  <div style={{ flex: 'none', display: 'flex', alignItems: 'center', gap: 12, padding: '9px 18px', background: accentSoft, borderBottom: `1px solid ${accent}33` }}>
                    <Icon.ClockIcon stroke={accent} style={{ flex: 'none' }} />
                    <div style={{ flex: 1, minWidth: 0, fontSize: 12.5, color: 'var(--text2)' }}>
                      Viewing this vault as it was on <b style={{ fontWeight: 600, color: 'var(--text)' }}>{new Date(playT).toLocaleString()}</b> · read-only
                    </div>
                    <button onClick={() => void onRestoreHere()} style={{ fontFamily: 'inherit', fontSize: 12, fontWeight: 500, color: 'var(--bg)', background: accent, border: 'none', borderRadius: 7, padding: '6px 12px', cursor: 'pointer', flex: 'none' }}>Restore this version</button>
                    <button onClick={onNow} style={{ fontFamily: 'inherit', fontSize: 12, fontWeight: 500, color: 'var(--text2)', background: 'var(--bg)', border: '1px solid var(--line)', borderRadius: 7, padding: '6px 12px', cursor: 'pointer', flex: 'none' }}>Return to now</button>
                  </div>
                )}

                <div className="asp-scroll" style={{ flex: 1, minHeight: 0, overflowY: 'auto', overflowX: 'hidden', display: 'flex', justifyContent: 'center', alignItems: 'flex-start' }}>
                  {paint && (
                    <LiveEditor
                      source={paint.source}
                      paintKey={paint.key}
                      path={selectedPath || ''}
                      readOnly={paint.readOnly}
                      notExist={paint.notExist}
                      accent={accent}
                      centered={centered}
                      fontFamily={fontFamily}
                      frontmatterStyle={prefs.frontmatterStyle}
                      onChange={onEditorChange}
                    />
                  )}
                </div>
              </>
            ) : (
              <div style={{ flex: 1, display: 'flex', flexDirection: 'column', alignItems: 'center', justifyContent: 'center', gap: 14, color: 'var(--faint2)' }}>
                <Icon.FileIcon size={40} stroke="currentColor" />
                <div style={{ fontSize: 14, color: 'var(--faint)' }}>Select a note to start editing</div>
              </div>
            )}
          </main>
        </div>

        {(histOpen || logOpen) && (
          <div onPointerDown={onHistBarResize} className="hb-resize" style={{ height: 7, flex: 'none', cursor: 'row-resize', margin: '-3px 0', zIndex: 6, position: 'relative', display: 'flex', alignItems: 'center' }}>
            <div className="hb-line" style={{ height: 1, alignSelf: 'center', width: '100%', background: 'var(--line)' }} />
          </div>
        )}

        <HistoryBar
          events={events}
          histRaw={histRaw}
          view={view2}
          setView={setView}
          playhead={playhead}
          setPlayhead={setPlayhead}
          now={now}
          accent={accent}
          accentSoft={accentSoft}
          timeTravel={timeTravel}
          location={desktop ? activeMeta?.path || '' : 'Using browser storage'}
          locationIsPath={desktop}
          fingerprint={shortFingerprint(identity)}
          status={activeStatus}
          identity={identity}
          histOpen={histOpen}
          logOpen={logOpen}
          barHeight={histOpen || logOpen ? histBarH : 38}
          animate={!resizingBar}
          onTabHistory={onTabHistory}
          onTabLog={onTabLog}
          onNow={onNow}
        />

        {/* file / root context menu */}
        {ctxMenu && (
          <>
            <div onClick={() => setCtxMenu(null)} onContextMenu={(e) => { e.preventDefault(); setCtxMenu(null); }} style={{ position: 'fixed', inset: 0, zIndex: 60 }} />
            <div style={{ position: 'fixed', left: ctxMenu.x, top: ctxMenu.y, zIndex: 61, width: 172, background: 'var(--bg)', border: '1px solid var(--line)', borderRadius: 10, boxShadow: '0 12px 32px rgba(28,25,23,0.16)', padding: 5 }}>
              {ctxMenu.root ? (
                <>
                  <div className="asp-hover-soft" onClick={() => void createFile(ctxTargetDir(ctxMenu))} style={{ display: 'flex', alignItems: 'center', gap: 9, padding: '7px 11px', borderRadius: 7, cursor: 'pointer', fontSize: 13, color: 'var(--text)' }}>
                    <Icon.NewFileIcon size={14} style={{ flex: 'none' }} />
                    <span>New file</span>
                  </div>
                  <div className="asp-hover-soft" onClick={() => void createFolder(ctxTargetDir(ctxMenu))} style={{ display: 'flex', alignItems: 'center', gap: 9, padding: '7px 11px', borderRadius: 7, cursor: 'pointer', fontSize: 13, color: 'var(--text)' }}>
                    <Icon.NewFolderIcon size={14} style={{ flex: 'none' }} />
                    <span>New folder</span>
                  </div>
                </>
              ) : (
                <>
                  <div className="asp-hover-soft" onClick={() => { setRenaming(ctxMenu.path!); setRenameValue(ctxMenu.name!); setCtxMenu(null); }} style={{ display: 'flex', alignItems: 'center', gap: 9, padding: '7px 11px', borderRadius: 7, cursor: 'pointer', fontSize: 13, color: 'var(--text)' }}>
                    <Icon.PencilIcon style={{ flex: 'none' }} />
                    <span>Rename</span>
                  </div>
                  <div className="asp-hover-soft" onClick={() => deleteNode(ctxMenu.path!, !!ctxMenu.isDir)} style={{ display: 'flex', alignItems: 'center', gap: 9, padding: '7px 11px', borderRadius: 7, cursor: 'pointer', fontSize: 13, color: 'var(--text)' }}>
                    <Icon.TrashIcon stroke="var(--text2)" style={{ flex: 'none' }} />
                    <span>Delete</span>
                  </div>
                </>
              )}
            </div>
          </>
        )}
      </div>
    );
  };

  return (
    <>
      {screen === 'connect' ? renderConnect() : renderEditor()}

      {/* vault-row context menu (connect screen) */}
      {vaultCtx && (
        <>
          <div onClick={() => setVaultCtx(null)} onContextMenu={(e) => { e.preventDefault(); setVaultCtx(null); }} style={{ position: 'fixed', inset: 0, zIndex: 62 }} />
          <div style={{ position: 'fixed', left: vaultCtx.x, top: vaultCtx.y, zIndex: 63, width: 176, background: 'var(--bg)', border: '1px solid var(--line)', borderRadius: 10, boxShadow: '0 10px 28px rgba(28,25,23,0.15)', padding: 4 }}>
            <div className="asp-hover-soft" onClick={() => { const v = vaults.find((x) => x.id === vaultCtx.id); setVaultCtx(null); if (v) openCustomize(v); }} style={{ display: 'flex', alignItems: 'center', gap: 9, padding: '7px 11px', borderRadius: 7, cursor: 'pointer', fontSize: 13, color: 'var(--text2)' }}>
              <Icon.WandIcon size={14} style={{ flex: 'none' }} />
              <span>Customize…</span>
            </div>
            <div className="asp-hover-danger" onClick={() => { const v = vaultMetas.find((x) => x.id === vaultCtx.id); setVaultCtx(null); if (v) setRemoveVaultState({ id: v.id, name: v.displayName, path: v.path, trash: false }); }} style={{ display: 'flex', alignItems: 'center', gap: 9, padding: '7px 11px', borderRadius: 7, cursor: 'pointer', fontSize: 13, color: '#c0392b' }}>
              <Icon.TrashIcon stroke="#c0392b" size={14} style={{ flex: 'none' }} />
              <span>Remove vault…</span>
            </div>
          </div>
        </>
      )}

      {/* entry modal — New vault / Connect a vault */}
      {entry && (
        <>
          <div onClick={() => { if (!connecting) setEntry(null); }} style={{ position: 'fixed', inset: 0, zIndex: 58, background: 'var(--overlay)', backdropFilter: 'blur(2px)' }} />
          <div style={{ position: 'fixed', zIndex: 59, top: '50%', left: '50%', transform: 'translate(-50%,-50%)', width: 'min(424px,92vw)', background: 'var(--bg)', borderRadius: 16, boxShadow: '0 24px 60px rgba(28,25,23,0.28)', padding: 20, display: 'flex', flexDirection: 'column', gap: 15 }}>
            <div>
              <div style={{ fontSize: 16, fontWeight: 600, letterSpacing: '-0.01em' }}>{entry === 'connect' ? 'Connect a vault' : 'New vault'}</div>
              <div style={{ fontSize: 12.5, color: 'var(--text3)', marginTop: 3 }}>{entry === 'connect' ? 'Paste a code someone shared with you.' : desktop ? 'Name it and choose a folder — everything syncs automatically.' : 'Name it and start writing — it saves in this browser and syncs automatically.'}</div>
            </div>
            {entry === 'new' && (
              <label style={{ display: 'flex', flexDirection: 'column', gap: 7 }}>
                <span style={{ fontSize: 11, fontWeight: 600, letterSpacing: '0.05em', textTransform: 'uppercase', color: 'var(--faint2)' }}>Name</span>
                <input value={newVaultName} onChange={(e) => setNewVaultName(e.target.value)} spellCheck={false} placeholder="My vault" style={{ fontFamily: 'inherit', fontSize: 14, color: 'var(--text)', background: 'var(--bg-input)', border: '1px solid var(--line)', borderRadius: 10, padding: '10px 12px', outline: 'none', width: '100%', boxSizing: 'border-box' }} />
              </label>
            )}
            {entry === 'connect' && (
              <>
                <label style={{ display: 'flex', flexDirection: 'column', gap: 7 }}>
                  <span style={{ fontSize: 11, fontWeight: 600, letterSpacing: '0.05em', textTransform: 'uppercase', color: 'var(--faint2)' }}>Invite code</span>
                  <textarea value={ticket} onChange={(e) => setTicket(e.target.value)} rows={2} spellCheck={false} placeholder="Paste the code someone shared with you" style={{ fontFamily: "'JetBrains Mono', monospace", fontSize: 12.5, lineHeight: 1.5, color: 'var(--text)', background: 'var(--bg-input)', border: '1px solid var(--line)', borderRadius: 10, padding: '11px 13px', resize: 'none', outline: 'none', width: '100%', boxSizing: 'border-box' }} />
                </label>
                <label style={{ display: 'flex', flexDirection: 'column', gap: 7 }}>
                  <span style={{ fontSize: 11, fontWeight: 600, letterSpacing: '0.05em', textTransform: 'uppercase', color: 'var(--faint2)' }}>Access key <span style={{ textTransform: 'none', letterSpacing: 0, fontWeight: 400, color: 'var(--faint)' }}>— if required</span></span>
                  <input value={authKey} onChange={(e) => setAuthKey(e.target.value)} type="password" spellCheck={false} placeholder="Leave blank if you weren't given one" style={{ fontFamily: "'JetBrains Mono', monospace", fontSize: 12.5, color: 'var(--text)', background: 'var(--bg-input)', border: '1px solid var(--line)', borderRadius: 10, padding: '11px 13px', outline: 'none', width: '100%', boxSizing: 'border-box' }} />
                </label>
              </>
            )}
            {desktop && (
              <div style={{ display: 'flex', flexDirection: 'column', gap: 7 }}>
                <span style={{ fontSize: 11, fontWeight: 600, letterSpacing: '0.05em', textTransform: 'uppercase', color: 'var(--faint2)' }}>{entry === 'connect' ? 'Save to' : 'Location'}</span>
                <div onClick={() => void onChooseDest()} style={{ display: 'flex', alignItems: 'center', gap: 9, background: 'var(--bg-input)', border: '1px solid var(--line)', borderRadius: 10, padding: '10px 13px', cursor: 'pointer' }}>
                  <Icon.FolderIcon size={15} stroke="var(--faint)" style={{ flex: 'none' }} />
                  <span style={{ fontFamily: "'JetBrains Mono', monospace", fontSize: 12, color: connectDest ? 'var(--text)' : 'var(--faint)', flex: 1, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{connectDest || 'Choose a folder…'}</span>
                  <span style={{ fontSize: 12, color: 'var(--faint)' }}>Choose…</span>
                </div>
              </div>
            )}
            <div style={{ display: 'flex', justifyContent: 'flex-end', gap: 8, marginTop: 2 }}>
              <button onClick={() => { if (!connecting) setEntry(null); }} style={{ fontFamily: 'inherit', fontSize: 13, fontWeight: 500, color: 'var(--text2)', background: 'var(--bg)', border: '1px solid var(--line)', borderRadius: 9, padding: '8px 16px', cursor: 'pointer' }}>Cancel</button>
              <button onClick={() => void onEntrySubmit()} disabled={entryBlocked} style={{ display: 'flex', alignItems: 'center', justifyContent: 'center', gap: 8, minWidth: 108, height: 38, padding: '0 18px', border: 'none', borderRadius: 9, background: entryBlocked ? 'var(--faint2)' : 'var(--text)', color: 'var(--bg)', fontSize: 13, fontWeight: 500, fontFamily: 'inherit', cursor: entryBlocked ? 'default' : 'pointer' }}>
                {connecting && <span style={{ width: 13, height: 13, border: '2px solid #ffffff66', borderTopColor: 'var(--bg)', borderRadius: '50%', display: 'inline-block', animation: 'aspSpin 0.7s linear infinite' }} />}
                <span>{entry === 'connect' ? (connecting ? 'Connecting…' : 'Connect') : 'Create vault'}</span>
              </button>
            </div>
          </div>
        </>
      )}

      {/* customize modal */}
      {customize && (
        <CustomizeModal
          initial={customize}
          onCancel={() => setCustomize(null)}
          onSave={(m) => { updateMeta(m.id, { name: m.name, hue: m.hue, emoji: m.emoji }); setCustomize(null); }}
        />
      )}

      {/* share modal */}
      {share && (
        <>
          <div onClick={() => setShare(null)} style={{ position: 'fixed', inset: 0, zIndex: 70, background: 'var(--overlay)', backdropFilter: 'blur(2px)' }} />
          <div style={{ position: 'fixed', zIndex: 71, top: '50%', left: '50%', transform: 'translate(-50%,-50%)', width: 'min(420px,92vw)', background: 'var(--bg)', borderRadius: 16, boxShadow: '0 24px 60px rgba(28,25,23,0.28)', padding: 20, display: 'flex', flexDirection: 'column', gap: 14 }}>
            <div>
              <div style={{ fontSize: 16, fontWeight: 600, letterSpacing: '-0.01em' }}>Share this vault</div>
              <div style={{ fontSize: 13, color: 'var(--text3)', marginTop: 3 }}>Anyone you give this code to can connect and sync.</div>
            </div>
            <div style={{ display: 'flex', alignItems: 'stretch', gap: 8 }}>
              <div style={{ flex: 1, minWidth: 0, fontFamily: "'JetBrains Mono', monospace", fontSize: 12, lineHeight: 1.5, color: 'var(--text2)', background: 'var(--bg-input)', border: '1px solid var(--line)', borderRadius: 10, padding: '11px 13px', wordBreak: 'break-all', maxHeight: 64, overflow: 'hidden' }}>{share.code || 'Generating…'}</div>
              <button onClick={() => void onCopyCode()} style={{ flex: 'none', alignSelf: 'stretch', display: 'flex', alignItems: 'center', fontFamily: 'inherit', fontSize: 12.5, fontWeight: 500, color: share.copied ? '#3a9357' : 'var(--text2)', background: 'var(--bg)', border: '1px solid var(--line)', borderRadius: 10, padding: '0 14px', cursor: 'pointer' }}>{share.copied ? 'Copied' : 'Copy'}</button>
            </div>
            <div onClick={() => void onToggleRequireKey()} style={{ display: 'flex', alignItems: 'center', gap: 11, cursor: 'pointer', padding: 2 }}>
              <span style={{ width: 34, height: 20, borderRadius: 12, flex: 'none', background: share.requireKey ? accent : 'var(--faint2)', position: 'relative', transition: 'background .15s' }}>
                <span style={{ position: 'absolute', top: 2, left: share.requireKey ? 16 : 2, width: 16, height: 16, borderRadius: '50%', background: 'var(--bg)', transition: 'left .15s', boxShadow: '0 1px 2px rgba(0,0,0,0.2)' }} />
              </span>
              <div style={{ flex: 1 }}>
                <div style={{ fontSize: 13.5, fontWeight: 500, color: 'var(--text)' }}>Require an access key</div>
                <div style={{ fontSize: 12, color: 'var(--faint)' }}>Adds a second secret they must enter too.</div>
              </div>
            </div>
            {share.requireKey && (
              <div style={{ display: 'flex', alignItems: 'center', gap: 10, background: 'var(--bg-input)', border: '1px solid var(--line)', borderRadius: 10, padding: '11px 13px' }}>
                <span style={{ fontSize: 11.5, color: 'var(--faint)', flex: 'none' }}>Access key</span>
                <span style={{ flex: 1, fontFamily: "'JetBrains Mono', monospace", fontSize: 13, letterSpacing: '0.04em', color: 'var(--text)', textAlign: 'right' }}>{share.accessKey}</span>
              </div>
            )}
            <button onClick={() => setShare(null)} style={{ alignSelf: 'flex-end', fontFamily: 'inherit', fontSize: 13, fontWeight: 500, color: 'var(--bg)', background: 'var(--text)', border: 'none', borderRadius: 9, padding: '8px 18px', cursor: 'pointer' }}>Done</button>
          </div>
        </>
      )}

      {/* remove modal */}
      {removeVaultState && (
        <>
          <div onClick={() => setRemoveVaultState(null)} style={{ position: 'fixed', inset: 0, zIndex: 72, background: 'var(--overlay)', backdropFilter: 'blur(2px)' }} />
          <div style={{ position: 'fixed', zIndex: 73, top: '50%', left: '50%', transform: 'translate(-50%,-50%)', width: 'min(412px,92vw)', background: 'var(--bg)', borderRadius: 16, boxShadow: '0 24px 60px rgba(28,25,23,0.28)', padding: 20, display: 'flex', flexDirection: 'column', gap: 14 }}>
            <div>
              <div style={{ fontSize: 16, fontWeight: 600, letterSpacing: '-0.01em' }}>Remove “{removeVaultState.name}”?</div>
              <div style={{ fontSize: 13, color: 'var(--text3)', marginTop: 4, lineHeight: 1.5 }}>{removeVaultState.trash ? 'The folder and its notes will be moved to the Trash.' : 'The folder stays on your computer — it’s only removed from asp.'}</div>
            </div>
            <div style={{ display: 'flex', alignItems: 'center', gap: 9, background: 'var(--bg-input)', border: '1px solid var(--line)', borderRadius: 10, padding: '9px 12px' }}>
              <Icon.FolderIcon style={{ flex: 'none' }} />
              <span style={{ fontFamily: "'JetBrains Mono', monospace", fontSize: 12, color: 'var(--text2)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', flex: 1 }}>{removeVaultState.path}</span>
            </div>
            <div onClick={() => setRemoveVaultState((r) => (r ? { ...r, trash: !r.trash } : r))} style={{ display: 'flex', alignItems: 'flex-start', gap: 11, cursor: 'pointer', padding: 2 }}>
              <span style={{ width: 34, height: 20, borderRadius: 12, flex: 'none', background: removeVaultState.trash ? '#c0392b' : 'var(--faint2)', position: 'relative', transition: 'background .15s', marginTop: 1 }}>
                <span style={{ position: 'absolute', top: 2, left: removeVaultState.trash ? 16 : 2, width: 16, height: 16, borderRadius: '50%', background: 'var(--bg)', transition: 'left .15s', boxShadow: '0 1px 2px rgba(0,0,0,0.2)' }} />
              </span>
              <div style={{ flex: 1 }}>
                <div style={{ fontSize: 13.5, fontWeight: 500, color: 'var(--text)' }}>Also move the folder to the Trash</div>
                <div style={{ fontSize: 12, color: 'var(--faint)', marginTop: 1 }}>{removeVaultState.trash ? 'It will appear in your system Trash.' : 'Nothing on disk changes.'}</div>
              </div>
            </div>
            <div style={{ display: 'flex', justifyContent: 'flex-end', gap: 8, marginTop: 2 }}>
              <button onClick={() => setRemoveVaultState(null)} style={{ fontFamily: 'inherit', fontSize: 13, fontWeight: 500, color: 'var(--text2)', background: 'var(--bg)', border: '1px solid var(--line)', borderRadius: 9, padding: '8px 16px', cursor: 'pointer' }}>Cancel</button>
              <button onClick={() => void confirmRemove()} style={{ fontFamily: 'inherit', fontSize: 13, fontWeight: 500, color: 'var(--bg)', background: '#c0392b', border: 'none', borderRadius: 9, padding: '8px 16px', cursor: 'pointer' }}>{removeVaultState.trash ? 'Remove & Trash folder' : 'Remove from asp'}</button>
            </div>
          </div>
        </>
      )}
    </>
  );
}
