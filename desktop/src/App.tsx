// Vault Editor — the Context Desktop app. A faithful React port of the
// "Vault Editor" design canvas, wired to the real backend (Tauri commands →
// asp-desktop-engine → asp-core). No protocol logic lives here; every vault,
// file, history and sync behavior is a command call.
import { open } from '@tauri-apps/plugin-dialog';
import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { api, type FileEntry, type VaultInfo, type VaultStatus } from './lib/api';
import LiveEditor from './vault/LiveEditor';
import {
  axisTicksFor,
  buildEvents,
  clampView,
  colorOf,
  createTsByPath,
  DAY,
  defaultView,
  fmtFull,
  toPct,
  type TrackEvent,
  type View,
  viewForNow,
  zoomAround,
  zoomKeepingFocus,
} from './vault/history';
import * as Icon from './vault/icons';
import { wordCountOf } from './vault/markdown';
import { allDirPaths, buildTree, firstSelectable, flatten, freeUntitledName } from './vault/tree';

// ---------- small helpers ----------
const basename = (p: string) => p.split('/').filter(Boolean).pop() || p;

function hueOf(s: string): number {
  let h = 0;
  for (let i = 0; i < s.length; i++) h = (h * 31 + s.charCodeAt(i)) >>> 0;
  return h % 360;
}
const dotStyle = (hue: number, size = 9): React.CSSProperties => ({
  width: size,
  height: size,
  borderRadius: '50%',
  background: `hsl(${hue}deg 52% 55%)`,
  flex: 'none',
});

function relTime(sec: number | null | undefined): string {
  if (!sec) return '—';
  const d = Math.max(0, Math.floor(Date.now() / 1000) - sec);
  if (d < 5) return 'just now';
  if (d < 60) return d + 's ago';
  if (d < 3600) return Math.floor(d / 60) + 'm ago';
  if (d < 86400) return Math.floor(d / 3600) + 'h ago';
  if (d < 172800) return 'yesterday';
  return Math.floor(d / 86400) + 'd ago';
}

function shortFingerprint(identity: string): string {
  const cleaned = identity.replace(/^ssh-\S+\s+/, '').trim();
  if (cleaned.length <= 14) return cleaned;
  return cleaned.slice(0, 8) + '…' + cleaned.slice(-4);
}

function makeAccessKey(): string {
  const alpha = 'ABCDEFGHJKLMNPQRSTUVWXYZ23456789';
  const grp = () =>
    Array.from({ length: 4 }, () => alpha[Math.floor(Math.random() * alpha.length)]).join('');
  return [grp(), grp(), grp(), grp()].join('-');
}

const FONT_FAMILIES: Record<string, string> = {
  Sans: "system-ui, -apple-system, 'Segoe UI', sans-serif",
  Serif: "'Newsreader', Georgia, serif",
  Mono: "'JetBrains Mono', ui-monospace, Menlo, monospace",
};

// Persisted UI prefs (the design's props become in-app settings).
interface Prefs {
  accent: string;
  font: 'Sans' | 'Serif' | 'Mono';
  writingColumn: boolean;
}
function loadPrefs(): Prefs {
  try {
    const raw = localStorage.getItem('asp.prefs.v1');
    if (raw) return { accent: '#3d63dd', font: 'Sans', writingColumn: true, ...JSON.parse(raw) };
  } catch {
    /* ignore */
  }
  return { accent: '#3d63dd', font: 'Sans', writingColumn: true };
}

interface VaultMeta extends VaultInfo {
  name: string;
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

export default function App() {
  const prefs = useMemo(loadPrefs, []);
  const accent = prefs.accent;
  const accentSoft = accent + '22';
  const fontFamily = FONT_FAMILIES[prefs.font];
  const centered = prefs.writingColumn !== false;

  const [identity, setIdentity] = useState('');
  const [screen, setScreen] = useState<'connect' | 'editor'>('connect');
  const [vaults, setVaults] = useState<VaultInfo[]>([]);
  const [statuses, setStatuses] = useState<Record<string, VaultStatus>>({});
  const [activeId, setActiveId] = useState<string | null>(null);

  const [files, setFiles] = useState<FileEntry[]>([]);
  const [expanded, setExpanded] = useState<Record<string, boolean>>({});
  const [selectedPath, setSelectedPath] = useState<string | null>(null);

  const [paint, setPaint] = useState<Paint | null>(null);
  const [docText, setDocText] = useState('');
  const [saving, setSaving] = useState(false);

  const [events, setEvents] = useState<TrackEvent[]>([]);
  const [now, setNow] = useState(() => Date.now());
  const [view, setView] = useState<View | null>(null);
  const [playhead, setPlayhead] = useState<number | null>(null);

  const [vaultMenuOpen, setVaultMenuOpen] = useState(false);
  const [ctxMenu, setCtxMenu] = useState<{ x: number; y: number; path: string; isDir: boolean; name: string } | null>(null);
  const [renaming, setRenaming] = useState<string | null>(null);
  const [renameValue, setRenameValue] = useState('');

  const [codeOpen, setCodeOpen] = useState(false);
  const [ticket, setTicket] = useState('');
  const [authKey, setAuthKey] = useState('');
  const [connecting, setConnecting] = useState(false);
  const [connectDest, setConnectDest] = useState<string | null>(null);

  const [share, setShare] = useState<{ id: string; code: string; requireKey: boolean; accessKey: string; copied: boolean } | null>(null);
  const [vaultCtx, setVaultCtx] = useState<{ x: number; y: number; id: string; name: string } | null>(null);
  const [removeVaultState, setRemoveVaultState] = useState<{ id: string; name: string; path: string; trash: boolean } | null>(null);

  // refs for values used inside imperative handlers / async flows
  const activeIdRef = useRef<string | null>(null);
  const selectedRef = useRef<string | null>(null);
  const bufferRef = useRef('');
  const playheadRef = useRef<number | null>(null);
  const viewRef = useRef<View | null>(null);
  const nowRef = useRef(now);
  const saveTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const trackRef = useRef<HTMLDivElement | null>(null);
  const paintSeq = useRef(0);
  activeIdRef.current = activeId;
  selectedRef.current = selectedPath;
  playheadRef.current = playhead;
  viewRef.current = view;
  nowRef.current = now;

  const curView = useCallback((): View => view || defaultView(now), [view, now]);
  const timeTravel = playhead != null && playhead < now;

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
      const h = await api.history(id);
      setEvents(buildEvents(h));
    } catch {
      setEvents([]);
    }
    setNow(Date.now());
  }, []);

  const refreshFiles = useCallback(async (id: string) => {
    const fs = await api.listFiles(id);
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

  // Poll status so peers / "last synced" stay fresh. Kept infrequent because
  // each status read folds the vault log; the live UI doesn't need it faster.
  useEffect(() => {
    const t = setInterval(() => {
      void refreshVaults().then(refreshStatuses);
    }, 10000);
    return () => clearInterval(t);
  }, [refreshVaults, refreshStatuses]);

  const vaultMetas: VaultMeta[] = useMemo(
    () =>
      vaults.map((v) => {
        const st = statuses[v.id];
        return {
          ...v,
          name: basename(v.path),
          peers: st?.peers.length ?? 0,
          lastTs: st?.last_ts ?? null,
          ticket: st?.listening_ticket ?? v.listening_ticket,
        };
      }),
    [vaults, statuses],
  );
  const activeMeta = vaultMetas.find((v) => v.id === activeId) || null;

  // ---------- selection + content resolution ----------
  const flushSave = useCallback(async () => {
    if (saveTimer.current) {
      clearTimeout(saveTimer.current);
      saveTimer.current = null;
    }
    const id = activeIdRef.current;
    const path = selectedRef.current;
    if (id && path && !(playheadRef.current != null && playheadRef.current < nowRef.current)) {
      try {
        await api.writeFile(id, path, bufferRef.current);
      } catch {
        /* ignore */
      }
    }
    setSaving(false);
  }, []);

  // Resolve displayed content whenever the vault / file / playhead changes.
  useEffect(() => {
    const id = activeId;
    const path = selectedPath;
    if (!id || !path) {
      setPaint(null);
      return;
    }
    let cancelled = false;
    const seq = ++paintSeq.current;
    const ph = playhead;
    const live = ph == null || ph >= nowRef.current;
    void (async () => {
      try {
        if (live) {
          const content = await api.readFile(id, path);
          if (cancelled || seq !== paintSeq.current) return;
          bufferRef.current = content;
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
      // Keep the working copy in a ref (no per-keystroke React re-render — that
      // would re-render the whole editor screen on every key). `setSaving(true)`
      // is a no-op once already saving, so it renders at most once per edit burst.
      bufferRef.current = src;
      setSaving(true);
      if (saveTimer.current) clearTimeout(saveTimer.current);
      saveTimer.current = setTimeout(() => {
        const id = activeIdRef.current;
        const path = selectedRef.current;
        if (!id || !path) return;
        void api
          .writeFile(id, path, bufferRef.current)
          .then(() => {
            setSaving(false);
            setDocText(bufferRef.current); // refresh the word count once, after the save
            return refreshHistory(id);
          })
          .catch(() => setSaving(false));
      }, 650);
    },
    [refreshHistory],
  );

  // ---------- vault open / switch ----------
  const openVault = useCallback(
    async (id: string) => {
      await flushSave();
      setActiveId(id);
      setScreen('editor');
      setVaultMenuOpen(false);
      setPlayhead(null);
      setView(defaultView(Date.now()));
      setNow(Date.now());
      const fs = await refreshFiles(id);
      const tree = buildTree(fs);
      const exp: Record<string, boolean> = {};
      for (const d of allDirPaths(tree)) exp[d] = true;
      setExpanded(exp);
      setSelectedPath(firstSelectable(tree));
      await refreshHistory(id);
    },
    [flushSave, refreshFiles, refreshHistory],
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

  // ---------- file ops ----------
  const newFile = useCallback(async () => {
    const id = activeIdRef.current;
    if (!id) return;
    await flushSave();
    const name = freeUntitledName(files.map((f) => f.path));
    await api.writeFile(id, name, `# ${name.replace(/\.md$/, '')}\n\n`);
    await refreshFiles(id);
    await refreshHistory(id);
    setSelectedPath(name);
  }, [files, flushSave, refreshFiles, refreshHistory]);

  const commitRename = useCallback(
    async (oldPath: string, rawName: string) => {
      const id = activeIdRef.current;
      setRenaming(null);
      const name = rawName.trim();
      if (!id || !name) return;
      const parts = oldPath.split('/');
      parts[parts.length - 1] = name;
      const newPath = parts.join('/');
      if (newPath === oldPath) return;
      await flushSave();
      // The backend renames by exact path, so a directory rename must move every
      // descendant entry (the Dir entity + each child file) to its new prefix.
      const affected = files.filter((f) => f.path === oldPath || f.path.startsWith(oldPath + '/'));
      if (affected.length === 0) affected.push({ path: oldPath } as FileEntry);
      for (const a of affected) {
        await api.renameFile(id, a.path, newPath + a.path.slice(oldPath.length));
      }
      await refreshFiles(id);
      await refreshHistory(id);
      setExpanded((e) => {
        const next: Record<string, boolean> = {};
        for (const k of Object.keys(e)) {
          if (k === oldPath) next[newPath] = e[k];
          else if (k.startsWith(oldPath + '/')) next[newPath + k.slice(oldPath.length)] = e[k];
          else next[k] = e[k];
        }
        return next;
      });
      if (selectedRef.current === oldPath) setSelectedPath(newPath);
      else if (selectedRef.current && selectedRef.current.startsWith(oldPath + '/')) {
        setSelectedPath(newPath + selectedRef.current.slice(oldPath.length));
      }
    },
    [files, flushSave, refreshFiles, refreshHistory],
  );

  const deleteNode = useCallback(
    async (path: string, isDir: boolean) => {
      const id = activeIdRef.current;
      if (!id) return;
      setCtxMenu(null);
      if (isDir) {
        const victims = files.filter((f) => f.path === path || f.path.startsWith(path + '/'));
        for (const v of victims) await api.deleteFile(id, v.path);
      } else {
        await api.deleteFile(id, path);
      }
      const fs = await refreshFiles(id);
      await refreshHistory(id);
      const sel = selectedRef.current;
      if (sel && (sel === path || sel.startsWith(path + '/'))) {
        setSelectedPath(firstSelectable(buildTree(fs)));
      }
    },
    [files, refreshFiles, refreshHistory],
  );

  const openCtx = useCallback((e: React.MouseEvent, node: { path: string; isDir: boolean; name: string }) => {
    e.preventDefault();
    e.stopPropagation();
    setVaultMenuOpen(false);
    setCtxMenu({
      x: Math.min(e.clientX, window.innerWidth - 184),
      y: Math.min(e.clientY, window.innerHeight - 110),
      path: node.path,
      isDir: node.isDir,
      name: node.name,
    });
  }, []);

  // ---------- history track interaction ----------
  const onTrackDown = useCallback(
    (e: React.PointerEvent) => {
      const el = trackRef.current;
      if (!el) return;
      const startX = e.clientX;
      const v0 = viewRef.current || defaultView(nowRef.current);
      const span0 = v0.end - v0.start;
      let moved = false;
      const move = (ev: PointerEvent) => {
        const dx = ev.clientX - startX;
        if (Math.abs(dx) > 3) moved = true;
        if (moved) {
          const r = el.getBoundingClientRect();
          const dt = -(dx / r.width) * span0;
          setView(clampView(v0.start + dt, v0.end + dt, nowRef.current));
        }
      };
      const up = (ev: PointerEvent) => {
        document.removeEventListener('pointermove', move);
        document.removeEventListener('pointerup', up);
        if (!moved) {
          const r = el.getBoundingClientRect();
          const t = v0.start + ((ev.clientX - r.left) / r.width) * span0;
          setPlayhead(Math.min(t, nowRef.current));
        }
      };
      document.addEventListener('pointermove', move);
      document.addEventListener('pointerup', up);
    },
    [],
  );

  const onHandleDown = useCallback((e: React.PointerEvent) => {
    e.stopPropagation();
    const el = trackRef.current;
    if (!el) return;
    const move = (ev: PointerEvent) => {
      const v = viewRef.current || defaultView(nowRef.current);
      const r = el.getBoundingClientRect();
      const t = v.start + ((ev.clientX - r.left) / r.width) * (v.end - v.start);
      setPlayhead(Math.max(nowRef.current - 90 * DAY, Math.min(t, nowRef.current)));
    };
    const up = () => {
      document.removeEventListener('pointermove', move);
      document.removeEventListener('pointerup', up);
    };
    document.addEventListener('pointermove', move);
    document.addEventListener('pointerup', up);
  }, []);

  // Non-passive wheel listener so we can preventDefault and zoom.
  useEffect(() => {
    const el = trackRef.current;
    if (!el) return;
    const handler = (e: WheelEvent) => {
      e.preventDefault();
      const v = viewRef.current || defaultView(nowRef.current);
      const r = el.getBoundingClientRect();
      const f = (e.clientX - r.left) / r.width;
      const factor = e.deltaY > 0 ? 1.2 : 0.82;
      setView(zoomKeepingFocus(v, f, factor, nowRef.current));
    };
    el.addEventListener('wheel', handler, { passive: false });
    return () => el.removeEventListener('wheel', handler);
  }, [screen]);

  const zoomBtn = useCallback((factor: number) => {
    const v = viewRef.current || defaultView(nowRef.current);
    const c = playheadRef.current != null ? playheadRef.current : nowRef.current;
    setView(zoomAround(v, c, factor, nowRef.current));
  }, []);

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
    // Returning to "now" (playhead → null) re-runs the content resolver, which
    // reads the freshly-restored file from disk and repaints it editable.
    setPlayhead(null);
    await refreshFiles(id);
    await refreshHistory(id);
  }, [refreshFiles, refreshHistory]);

  // ---------- connect / share / remove ----------
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

  const onConnect = useCallback(async () => {
    if (connecting) return;
    const t = ticket.trim();
    if (!t || !connectDest) return;
    setConnecting(true);
    try {
      const info = await api.cloneRemote(connectDest, t, authKey || undefined);
      setTicket('');
      setAuthKey('');
      setConnectDest(null);
      setCodeOpen(false);
      await refreshVaults();
      await openVault(info.id);
    } catch (err) {
      console.error('clone failed', err);
    } finally {
      setConnecting(false);
    }
  }, [authKey, connectDest, connecting, openVault, refreshVaults, ticket]);

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
    }
  }, [removeVaultState, refreshVaults]);

  // ---------- derived view-model ----------
  const tree = useMemo(() => buildTree(files), [files]);
  const view2 = curView();
  const span = view2.end - view2.start;
  const playT = playhead == null ? now : playhead;
  const filterTs = timeTravel ? playhead : null;
  const axisTicks = useMemo(() => axisTicksFor(view2), [view2]);
  const createTs = useMemo(() => createTsByPath(events), [events]);

  const fileVisible = (path: string) => filterTs == null || (createTs[path] != null && createTs[path] <= filterTs);
  const dirVisible = (path: string) =>
    filterTs == null || files.some((f) => !f.is_dir && (f.path === path || f.path.startsWith(path + '/')) && fileVisible(f.path));

  const rows = useMemo(() => {
    const flat = flatten(tree, expanded);
    return flat.filter((r) => (r.node.type === 'dir' ? dirVisible(r.node.path) : fileVisible(r.node.path)));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [tree, expanded, filterTs, files]);

  const visibleRows = filterTs == null ? events.length : events.filter((e) => e.ts <= filterTs).length;

  const ticks = events
    .filter((e) => e.ts >= view2.start - span * 0.03 && e.ts <= view2.end + span * 0.03)
    .map((e) => ({
      title: `${e.kind} · ${e.path} · ${fmtFull(e.ts)}`,
      pct: toPct(e.ts, view2),
      color: colorOf(e.kind),
      past: e.ts <= playT,
    }));

  const playPct = Math.max(0, Math.min(100, toPct(playT, view2)));
  const nowPct = Math.max(0, Math.min(100, toPct(now, view2)));
  const phColor = timeTravel ? accent : '#1c1917';

  const selParts = selectedPath ? selectedPath.split('/') : [];
  const crumbFile = selParts.length ? selParts[selParts.length - 1] : '';
  const crumbDir = selParts.length > 1 ? selParts.slice(0, -1).join(' / ') + ' / ' : '';
  const wordCount = wordCountOf(docText);

  // ===================================================================
  // RENDER
  // ===================================================================
  const renderConnect = () => {
    const saved = vaultMetas;
    return (
      <div style={{ position: 'fixed', inset: 0, display: 'flex', flexDirection: 'column', alignItems: 'center', justifyContent: 'center', background: '#fafaf8', color: '#1c1917', padding: 32, overflow: 'auto' }}>
        <div style={{ width: 'min(452px, 94vw)', display: 'flex', flexDirection: 'column' }}>
          <div style={{ display: 'flex', alignItems: 'center', gap: 11, marginBottom: 34 }}>
            <div style={{ width: 26, height: 26, borderRadius: 7, background: accent, display: 'flex', alignItems: 'center', justifyContent: 'center', flex: 'none' }}>
              <div style={{ width: 9, height: 9, borderRadius: '50%', background: '#fff' }} />
            </div>
            <div style={{ fontFamily: "'JetBrains Mono', monospace", fontWeight: 600, fontSize: 16, letterSpacing: '-0.01em' }}>asp</div>
            <span style={{ flex: 1 }} />
            <div style={{ display: 'flex', alignItems: 'center', gap: 6, fontSize: 12, color: '#a8a29e' }}>
              <Icon.DesktopIcon />
              <span>Desktop</span>
            </div>
          </div>

          <h1 style={{ fontSize: 25, fontWeight: 600, letterSpacing: '-0.02em', margin: '0 0 22px' }}>Your vaults</h1>

          <div style={{ display: 'flex', gap: 10 }}>
            <button onClick={onOpenFolder} style={{ flex: 1, display: 'flex', alignItems: 'center', justifyContent: 'center', gap: 9, height: 46, border: 'none', borderRadius: 12, background: '#1c1917', color: '#fff', fontSize: 14, fontWeight: 500, fontFamily: 'inherit', cursor: 'pointer' }}>
              <Icon.FolderIcon stroke="#fff" />
              <span>Open a folder</span>
            </button>
            <button onClick={() => setCodeOpen((v) => !v)} style={{ display: 'flex', alignItems: 'center', justifyContent: 'center', gap: 8, height: 46, padding: '0 16px', border: '1px solid #e0ddd8', borderRadius: 12, background: '#fff', color: '#57534e', fontSize: 14, fontWeight: 500, fontFamily: 'inherit', cursor: 'pointer', flex: 'none' }}>
              <Icon.LinkIcon size={16} />
              <span>Connect with a code</span>
            </button>
          </div>

          {codeOpen && (
            <div style={{ marginTop: 14, background: '#fff', border: '1px solid #e7e5e4', borderRadius: 14, padding: 16, display: 'flex', flexDirection: 'column', gap: 13 }}>
              <label style={{ display: 'flex', flexDirection: 'column', gap: 7 }}>
                <span style={{ fontSize: 12, fontWeight: 500, color: '#57534e' }}>Invite code</span>
                <textarea value={ticket} onChange={(e) => setTicket(e.target.value)} rows={2} spellCheck={false} placeholder="Paste the code someone shared with you" style={{ fontFamily: "'JetBrains Mono', monospace", fontSize: 12.5, lineHeight: 1.5, color: '#1c1917', background: '#faf9f7', border: '1px solid #e7e5e4', borderRadius: 10, padding: '11px 13px', resize: 'none', outline: 'none', width: '100%' }} />
              </label>
              <label style={{ display: 'flex', flexDirection: 'column', gap: 7 }}>
                <span style={{ fontSize: 12, fontWeight: 500, color: '#57534e' }}>Access key <span style={{ color: '#a8a29e', fontWeight: 400 }}>— if required</span></span>
                <input value={authKey} onChange={(e) => setAuthKey(e.target.value)} type="password" spellCheck={false} placeholder="Leave blank if you weren't given one" style={{ fontFamily: "'JetBrains Mono', monospace", fontSize: 12.5, color: '#1c1917', background: '#faf9f7', border: '1px solid #e7e5e4', borderRadius: 10, padding: '11px 13px', outline: 'none', width: '100%' }} />
              </label>
              <div style={{ display: 'flex', flexDirection: 'column', gap: 7 }}>
                <span style={{ fontSize: 12, fontWeight: 500, color: '#57534e' }}>Save to</span>
                <div onClick={onChooseDest} style={{ display: 'flex', alignItems: 'center', gap: 9, background: '#faf9f7', border: '1px solid #e7e5e4', borderRadius: 10, padding: '10px 13px', cursor: 'pointer' }}>
                  <Icon.FolderIcon />
                  <span style={{ fontFamily: "'JetBrains Mono', monospace", fontSize: 12, color: connectDest ? '#1c1917' : '#a8a29e', flex: 1, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{connectDest || 'No folder chosen'}</span>
                  <span style={{ fontSize: 12, color: '#a8a29e' }}>Choose…</span>
                </div>
              </div>
              <button onClick={onConnect} disabled={connecting || !ticket.trim() || !connectDest} style={{ display: 'flex', alignItems: 'center', justifyContent: 'center', gap: 8, height: 44, border: 'none', borderRadius: 11, background: connecting || !ticket.trim() || !connectDest ? '#c9c5be' : accent, color: '#fff', fontSize: 14, fontWeight: 500, fontFamily: 'inherit', cursor: connecting || !ticket.trim() || !connectDest ? 'default' : 'pointer' }}>
                {connecting && <span style={{ width: 13, height: 13, border: '2px solid #ffffff66', borderTopColor: '#fff', borderRadius: '50%', display: 'inline-block', animation: 'aspSpin 0.7s linear infinite' }} />}
                <span>{connecting ? 'Connecting…' : 'Connect'}</span>
              </button>
            </div>
          )}

          {saved.length > 0 && (
            <div style={{ marginTop: 26, display: 'flex', flexDirection: 'column', gap: 2 }}>
              {saved.map((v) => (
                <div key={v.id} className="asp-hover-list" onClick={() => void openVault(v.id)} onContextMenu={(e) => { e.preventDefault(); setVaultCtx({ x: Math.min(e.clientX, window.innerWidth - 180), y: Math.min(e.clientY, window.innerHeight - 70), id: v.id, name: v.name }); }} style={{ display: 'flex', alignItems: 'center', gap: 13, padding: '11px 12px', borderRadius: 12, cursor: 'pointer' }}>
                  <div style={dotStyle(hueOf(v.vault_id || v.id))} />
                  <div style={{ flex: 1, minWidth: 0 }}>
                    <div style={{ fontSize: 14, fontWeight: 500, color: '#1c1917', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{v.name}</div>
                    <div style={{ display: 'flex', alignItems: 'center', gap: 6, marginTop: 2, minWidth: 0 }}>
                      <Icon.FolderIcon size={12} stroke="#bdb8b0" />
                      <span style={{ fontFamily: "'JetBrains Mono', monospace", fontSize: 11, color: '#a8a29e', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{v.path}</span>
                    </div>
                  </div>
                  <div style={{ display: 'flex', alignItems: 'center', gap: 9, flex: 'none' }}>
                    <span style={{ fontSize: 11.5, color: '#a8a29e' }}>{relTime(v.lastTs)}</span>
                    <Icon.ChevronRight size={14} stroke="#c4bfb8" />
                  </div>
                </div>
              ))}
            </div>
          )}

          <div style={{ marginTop: 28, fontSize: 11.5, color: '#bdb8b0', display: 'flex', alignItems: 'center', gap: 7 }}>
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
    const peersLabel = activeMeta && activeMeta.peers > 0 ? `${activeMeta.peers} online` : 'Only you';
    const otherVaults = vaultMetas;

    return (
      <div style={{ position: 'fixed', inset: 0, display: 'flex', flexDirection: 'column', background: '#fff', color: '#1c1917', fontSize: 14 }}>
        <div style={{ flex: 1, minHeight: 0, display: 'flex' }}>
          {/* sidebar */}
          <aside style={{ width: 266, flex: 'none', display: 'flex', flexDirection: 'column', background: '#fafaf8', borderRight: '1px solid #ededea' }}>
            <div style={{ position: 'relative', borderBottom: '1px solid #f0efec' }}>
              <div className="asp-hover-list" onClick={() => setVaultMenuOpen((v) => !v)} style={{ display: 'flex', alignItems: 'center', gap: 11, height: 47, padding: '0 14px', boxSizing: 'border-box', cursor: 'pointer' }}>
                <div style={dotStyle(hueOf(activeMeta?.vault_id || activeId || ''))} />
                <div style={{ flex: 1, minWidth: 0 }}>
                  <div style={{ fontSize: 14, fontWeight: 600, letterSpacing: '-0.01em', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{activeMeta?.name || 'Vault'}</div>
                  <div style={{ display: 'flex', alignItems: 'center', gap: 6, marginTop: 2 }}>
                    <span style={{ width: 6, height: 6, borderRadius: '50%', background: accent, animation: 'aspPulse 2.4s ease-in-out infinite', flex: 'none' }} />
                    <span style={{ fontSize: 11, color: '#a8a29e', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{syncSummary}</span>
                  </div>
                </div>
                <Icon.CaretDown style={{ flex: 'none', transition: 'transform .15s', transform: vaultMenuOpen ? 'rotate(180deg)' : 'rotate(0deg)' }} />
              </div>

              {vaultMenuOpen && (
                <>
                  <div onClick={() => setVaultMenuOpen(false)} style={{ position: 'fixed', inset: 0, zIndex: 40 }} />
                  <div style={{ position: 'absolute', top: 'calc(100% - 4px)', left: 8, right: 8, zIndex: 41, background: '#fff', border: '1px solid #e7e5e4', borderRadius: 12, boxShadow: '0 12px 32px rgba(28,25,23,0.13)', padding: 6, display: 'flex', flexDirection: 'column', gap: 2 }}>
                    <div style={{ fontSize: 10.5, fontWeight: 600, letterSpacing: '0.06em', textTransform: 'uppercase', color: '#b0aaa2', padding: '7px 9px 4px' }}>Switch vault</div>
                    {otherVaults.map((v) => (
                      <div key={v.id} className="asp-hover-soft" onClick={() => void openVault(v.id)} style={{ display: 'flex', alignItems: 'center', gap: 11, padding: '8px 9px', borderRadius: 8, cursor: 'pointer' }}>
                        <div style={dotStyle(hueOf(v.vault_id || v.id))} />
                        <div style={{ flex: 1, minWidth: 0 }}>
                          <div style={{ fontSize: 13.5, fontWeight: 500, color: '#1c1917', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{v.name}</div>
                          <div style={{ display: 'flex', alignItems: 'center', gap: 5, marginTop: 1, minWidth: 0 }}>
                            <Icon.FolderIcon size={12} stroke="#bdb8b0" />
                            <span style={{ fontFamily: "'JetBrains Mono', monospace", fontSize: 10.5, color: '#b0aaa2', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{v.path}</span>
                          </div>
                        </div>
                        {v.id === activeId && <Icon.CheckIcon stroke={accent} style={{ flex: 'none' }} />}
                      </div>
                    ))}
                    <div style={{ height: 1, background: '#f0efec', margin: '4px 6px' }} />
                    <div className="asp-hover-soft" onClick={() => activeId && void onShareVault(activeId)} style={{ display: 'flex', alignItems: 'center', gap: 10, padding: '8px 9px', borderRadius: 8, cursor: 'pointer', color: '#57534e' }}>
                      <Icon.ShareIcon style={{ flex: 'none' }} />
                      <span style={{ fontSize: 13.5 }}>Share this vault…</span>
                    </div>
                    <div className="asp-hover-soft" onClick={() => { setVaultMenuOpen(false); void onOpenFolder(); }} style={{ display: 'flex', alignItems: 'center', gap: 10, padding: '8px 9px', borderRadius: 8, cursor: 'pointer', color: '#57534e' }}>
                      <Icon.FolderIcon stroke="#57534e" />
                      <span style={{ fontSize: 13.5 }}>Open another folder…</span>
                    </div>
                    <div className="asp-hover-danger" onClick={() => { setVaultMenuOpen(false); if (activeMeta) setRemoveVaultState({ id: activeMeta.id, name: activeMeta.name, path: activeMeta.path, trash: false }); }} style={{ display: 'flex', alignItems: 'center', gap: 10, padding: '8px 9px', borderRadius: 8, cursor: 'pointer', color: '#c0392b' }}>
                      <Icon.TrashIcon stroke="#c0392b" style={{ flex: 'none' }} />
                      <span style={{ fontSize: 13.5 }}>Remove this vault…</span>
                    </div>
                  </div>
                </>
              )}
            </div>

            <div style={{ display: 'flex', alignItems: 'center', gap: 8, padding: '9px 12px 7px' }}>
              <span style={{ fontSize: 11, fontWeight: 600, letterSpacing: '0.06em', textTransform: 'uppercase', color: '#b0aaa2', flex: 1 }}>Files</span>
              <button className="asp-icon-btn" onClick={() => void newFile()} title="New note" style={{ display: 'flex', alignItems: 'center', justifyContent: 'center', width: 24, height: 24, border: 'none', background: 'transparent', color: '#78716c', borderRadius: 6, cursor: 'pointer', padding: 0 }}>
                <Icon.PlusIcon />
              </button>
            </div>

            <div className="asp-scroll" style={{ flex: 1, overflowY: 'auto', padding: '2px 8px 12px' }}>
              {rows.map(({ node, depth }) => {
                const isActive = node.type === 'file' && node.path === selectedPath;
                const isRenaming = renaming === node.path;
                return (
                  <div
                    key={node.path}
                    className="asp-hover-row"
                    onClick={() => { if (isRenaming) return; if (node.type === 'dir') toggleDir(node.path); else void selectFile(node.path); }}
                    onContextMenu={(e) => openCtx(e, { path: node.path, isDir: node.type === 'dir', name: node.name })}
                    style={{ display: 'flex', alignItems: 'center', gap: 6, padding: '5px 7px', paddingLeft: 7 + depth * 15, borderRadius: 7, cursor: 'pointer', fontSize: 13.5, background: isActive ? accentSoft : 'transparent', color: isActive ? '#1c1917' : '#44403c' }}
                  >
                    <span style={{ width: 16, display: 'inline-flex', justifyContent: 'center', flex: 'none' }}>
                      {node.type === 'dir' && (
                        <span style={{ display: 'inline-flex', color: '#a8a29e', transition: 'transform .12s', transform: expanded[node.path] ? 'rotate(90deg)' : 'rotate(0deg)' }}>
                          <Icon.ChevronRight />
                        </span>
                      )}
                    </span>
                    {node.type === 'file' && (
                      <span style={{ display: 'inline-flex', flex: 'none', color: isActive ? accent : '#a8a29e' }}>
                        <Icon.FileIcon />
                      </span>
                    )}
                    {isRenaming ? (
                      <input
                        autoFocus
                        value={renameValue}
                        spellCheck={false}
                        onChange={(e) => setRenameValue(e.target.value)}
                        onKeyDown={(e) => { if (e.key === 'Enter') { e.preventDefault(); void commitRename(node.path, renameValue); } else if (e.key === 'Escape') setRenaming(null); }}
                        onBlur={() => void commitRename(node.path, renameValue)}
                        onClick={(e) => e.stopPropagation()}
                        style={{ flex: 1, minWidth: 0, fontFamily: 'inherit', fontSize: 13.5, border: `1px solid ${accent}`, borderRadius: 4, padding: '1px 5px', outline: 'none', background: '#fff', color: '#1c1917' }}
                      />
                    ) : (
                      <span style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', flex: 1 }}>{node.name}</span>
                    )}
                  </div>
                );
              })}
            </div>

            <div style={{ borderTop: '1px solid #f0efec', padding: '10px 14px', display: 'flex', flexDirection: 'column', gap: 5 }}>
              <div style={{ display: 'flex', alignItems: 'center', gap: 8, minWidth: 0 }}>
                <Icon.FolderIcon size={13} stroke="#b0aaa2" />
                <span style={{ fontFamily: "'JetBrains Mono', monospace", fontSize: 10.5, color: '#a8a29e', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{activeMeta?.path}</span>
              </div>
              <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                <Icon.UserIcon stroke="#b8b3ac" style={{ flex: 'none' }} />
                <span style={{ fontFamily: "'JetBrains Mono', monospace", fontSize: 10, color: '#b0aaa2', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', flex: 1 }}>{shortFingerprint(identity)}</span>
                <span style={{ fontSize: 11, color: '#a8a29e', flex: 'none' }}>{peersLabel}</span>
              </div>
            </div>
          </aside>

          {/* main */}
          <main style={{ flex: 1, display: 'flex', flexDirection: 'column', minWidth: 0 }}>
            {hasSelection ? (
              <>
                <div style={{ height: 47, flex: 'none', display: 'flex', alignItems: 'center', gap: 14, padding: '0 18px', borderBottom: '1px solid #f0efec' }}>
                  <div style={{ flex: 1, minWidth: 0, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', fontSize: 13 }}>
                    <span style={{ color: '#b0aaa2' }}>{crumbDir}</span>
                    <span style={{ color: '#292524', fontWeight: 500 }}>{crumbFile}</span>
                  </div>
                  <div style={{ display: 'flex', alignItems: 'center', gap: 6, flex: 'none' }}>
                    <span style={{ width: 7, height: 7, borderRadius: '50%', flex: 'none', background: saving ? '#d9a93d' : '#3fa45a', transition: 'background .2s' }} />
                    <span style={{ fontSize: 12, color: '#a8a29e', width: 62 }}>{saving ? 'Saving…' : 'Saved'}</span>
                  </div>
                  <div style={{ width: 1, height: 18, background: '#ececea', flex: 'none' }} />
                  <span style={{ fontSize: 12, color: '#b0aaa2', fontVariantNumeric: 'tabular-nums', flex: 'none' }}>{wordCount}</span>
                </div>

                {timeTravel && (
                  <div style={{ flex: 'none', display: 'flex', alignItems: 'center', gap: 12, padding: '9px 18px', background: accentSoft, borderBottom: `1px solid ${accent}33` }}>
                    <Icon.ClockIcon stroke={accent} style={{ flex: 'none' }} />
                    <div style={{ flex: 1, minWidth: 0, fontSize: 12.5, color: '#44403c' }}>
                      Viewing this vault as it was on <b style={{ fontWeight: 600, color: '#1c1917' }}>{fmtFull(playT)}</b> · read-only
                    </div>
                    <button onClick={() => void onRestoreHere()} style={{ fontFamily: 'inherit', fontSize: 12, fontWeight: 500, color: '#fff', background: accent, border: 'none', borderRadius: 7, padding: '6px 12px', cursor: 'pointer', flex: 'none' }}>Restore this version</button>
                    <button onClick={onNow} style={{ fontFamily: 'inherit', fontSize: 12, fontWeight: 500, color: '#57534e', background: '#fff', border: '1px solid #e0ddd8', borderRadius: 7, padding: '6px 12px', cursor: 'pointer', flex: 'none' }}>Return to now</button>
                  </div>
                )}

                <div className="asp-scroll" style={{ flex: 1, minHeight: 0, overflowY: 'auto', display: 'flex', justifyContent: 'center', alignItems: 'flex-start' }}>
                  {paint && (
                    <LiveEditor
                      source={paint.source}
                      paintKey={paint.key}
                      readOnly={paint.readOnly}
                      notExist={paint.notExist}
                      accent={accent}
                      centered={centered}
                      fontFamily={fontFamily}
                      onChange={onEditorChange}
                    />
                  )}
                </div>
              </>
            ) : (
              <div style={{ flex: 1, display: 'flex', flexDirection: 'column', alignItems: 'center', justifyContent: 'center', gap: 14, color: '#c4bfb8' }}>
                <Icon.FileIcon size={40} stroke="currentColor" />
                <div style={{ fontSize: 14, color: '#a8a29e' }}>Select a note to start editing</div>
              </div>
            )}
          </main>
        </div>

        {/* history bar */}
        <div style={{ flex: 'none', height: 80, background: '#fafaf8', borderTop: '1px solid #ededea', display: 'flex', flexDirection: 'column', userSelect: 'none' }}>
          <div style={{ display: 'flex', alignItems: 'center', gap: 10, padding: '7px 16px 4px' }}>
            <Icon.ClockIcon style={{ flex: 'none' }} />
            <span style={{ fontSize: 11, fontWeight: 600, letterSpacing: '0.06em', textTransform: 'uppercase', color: '#a8a29e' }}>History</span>
            <span style={{ fontSize: 11, fontFamily: "'JetBrains Mono', monospace", padding: '2px 9px', borderRadius: 20, flex: 'none', background: timeTravel ? accentSoft : '#e9f2ec', color: timeTravel ? accent : '#3a9357', fontWeight: 500 }}>{timeTravel ? fmtFull(playT) : 'Live · now'}</span>
            <span style={{ flex: 1 }} />
            <span style={{ fontSize: 11, color: '#b0aaa2', fontVariantNumeric: 'tabular-nums', flex: 'none' }}>{filterTs == null ? `${events.length} rows` : `${visibleRows} / ${events.length} rows`}</span>
            <div style={{ display: 'flex', alignItems: 'center', gap: 2, background: '#efece7', borderRadius: 7, padding: 2, flex: 'none' }}>
              <button className="asp-zoom-btn" onClick={() => zoomBtn(1.8)} title="Zoom out" style={{ width: 24, height: 22, border: 'none', background: 'transparent', color: '#78716c', borderRadius: 5, cursor: 'pointer', display: 'flex', alignItems: 'center', justifyContent: 'center', padding: 0 }}>
                <Icon.MinusIcon />
              </button>
              <button className="asp-zoom-btn" onClick={() => zoomBtn(0.55)} title="Zoom in" style={{ width: 24, height: 22, border: 'none', background: 'transparent', color: '#78716c', borderRadius: 5, cursor: 'pointer', display: 'flex', alignItems: 'center', justifyContent: 'center', padding: 0 }}>
                <Icon.PlusIcon size={14} />
              </button>
            </div>
            <button onClick={onNow} style={{ fontFamily: 'inherit', fontSize: 12, fontWeight: 500, color: timeTravel ? '#57534e' : '#bdb8b0', background: '#fff', border: '1px solid #e7e4df', borderRadius: 7, padding: '4px 12px', cursor: 'pointer', flex: 'none', transition: 'border-color .12s' }}>Now</button>
          </div>

          <div ref={trackRef} onPointerDown={onTrackDown} style={{ position: 'relative', flex: 1, margin: '0 16px 9px', cursor: 'crosshair', touchAction: 'none' }}>
            <div style={{ position: 'absolute', inset: 0, borderBottom: '1px solid #e7e4df' }} />
            {axisTicks.map((a, i) => (
              <React.Fragment key={i}>
                <div style={{ position: 'absolute', left: a.pct + '%', top: 0, bottom: 0, width: 1, background: '#efece7' }} />
                <div style={{ position: 'absolute', left: a.pct + '%', bottom: -2, transform: 'translateX(4px)', fontSize: 9.5, color: '#bdb8b0', fontFamily: "'JetBrains Mono', monospace", whiteSpace: 'nowrap' }}>{a.label}</div>
              </React.Fragment>
            ))}
            {ticks.map((t, i) => (
              <div key={i} title={t.title} style={{ position: 'absolute', left: t.pct + '%', top: '16%', height: '68%', width: 2, marginLeft: -1, borderRadius: 1, background: t.color, opacity: t.past ? 0.85 : 0.28 }} />
            ))}
            <div style={{ position: 'absolute', left: nowPct + '%', top: 0, bottom: 0, width: 0, borderLeft: '1px dashed #cfcbc4' }} />
            <div style={{ position: 'absolute', left: playPct + '%', top: -3, bottom: -3, width: 2, marginLeft: -1, background: phColor, zIndex: 5 }}>
              <div onPointerDown={onHandleDown} style={{ position: 'absolute', left: -6, top: -8, width: 14, height: 14, borderRadius: 4, background: phColor, cursor: 'ew-resize', boxShadow: '0 1px 4px rgba(28,25,23,0.3)' }} />
            </div>
          </div>
        </div>

        {/* file context menu */}
        {ctxMenu && (
          <>
            <div onClick={() => setCtxMenu(null)} onContextMenu={(e) => { e.preventDefault(); setCtxMenu(null); }} style={{ position: 'fixed', inset: 0, zIndex: 60 }} />
            <div style={{ position: 'fixed', left: ctxMenu.x, top: ctxMenu.y, zIndex: 61, width: 172, background: '#fff', border: '1px solid #e7e5e4', borderRadius: 10, boxShadow: '0 12px 32px rgba(28,25,23,0.16)', padding: 5 }}>
              <div style={{ fontSize: 10, fontWeight: 600, letterSpacing: '0.05em', textTransform: 'uppercase', color: '#b8b3ac', padding: '5px 11px 3px', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{ctxMenu.name}</div>
              <div className="asp-hover-soft" onClick={() => { setRenaming(ctxMenu.path); setRenameValue(ctxMenu.name); setCtxMenu(null); }} style={{ display: 'flex', alignItems: 'center', gap: 9, padding: '7px 11px', borderRadius: 7, cursor: 'pointer', fontSize: 13, color: '#1c1917' }}>
                <Icon.PencilIcon style={{ flex: 'none' }} />
                <span>Rename</span>
              </div>
              <div className="asp-hover-danger" onClick={() => void deleteNode(ctxMenu.path, ctxMenu.isDir)} style={{ display: 'flex', alignItems: 'center', gap: 9, padding: '7px 11px', borderRadius: 7, cursor: 'pointer', fontSize: 13, color: '#c0392b' }}>
                <Icon.TrashIcon stroke="#c0392b" style={{ flex: 'none' }} />
                <span>Delete</span>
              </div>
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
          <div style={{ position: 'fixed', left: vaultCtx.x, top: vaultCtx.y, zIndex: 63, width: 168, background: '#fff', border: '1px solid #e7e5e4', borderRadius: 10, boxShadow: '0 12px 32px rgba(28,25,23,0.16)', padding: 5 }}>
            <div className="asp-hover-danger" onClick={() => { const v = vaultMetas.find((x) => x.id === vaultCtx.id); setVaultCtx(null); if (v) setRemoveVaultState({ id: v.id, name: v.name, path: v.path, trash: false }); }} style={{ display: 'flex', alignItems: 'center', gap: 9, padding: '7px 11px', borderRadius: 7, cursor: 'pointer', fontSize: 13, color: '#c0392b' }}>
              <Icon.TrashIcon stroke="#c0392b" style={{ flex: 'none' }} />
              <span>Remove vault…</span>
            </div>
          </div>
        </>
      )}

      {/* share modal */}
      {share && (
        <>
          <div onClick={() => setShare(null)} style={{ position: 'fixed', inset: 0, zIndex: 70, background: 'rgba(28,25,23,0.30)' }} />
          <div style={{ position: 'fixed', zIndex: 71, top: '50%', left: '50%', transform: 'translate(-50%,-50%)', width: 'min(420px,92vw)', background: '#fff', borderRadius: 16, boxShadow: '0 24px 60px rgba(28,25,23,0.28)', padding: 20, display: 'flex', flexDirection: 'column', gap: 14 }}>
            <div>
              <div style={{ fontSize: 16, fontWeight: 600, letterSpacing: '-0.01em' }}>Share this vault</div>
              <div style={{ fontSize: 13, color: '#78716c', marginTop: 3 }}>Anyone you give this code to can connect and sync.</div>
            </div>
            <div style={{ display: 'flex', alignItems: 'stretch', gap: 8 }}>
              <div style={{ flex: 1, minWidth: 0, fontFamily: "'JetBrains Mono', monospace", fontSize: 12, lineHeight: 1.5, color: '#44403c', background: '#faf9f7', border: '1px solid #e7e5e4', borderRadius: 10, padding: '11px 13px', wordBreak: 'break-all', maxHeight: 64, overflow: 'hidden' }}>{share.code || 'Generating…'}</div>
              <button onClick={() => void onCopyCode()} style={{ flex: 'none', alignSelf: 'stretch', display: 'flex', alignItems: 'center', fontFamily: 'inherit', fontSize: 12.5, fontWeight: 500, color: share.copied ? '#3a9357' : '#57534e', background: '#fff', border: '1px solid #e0ddd8', borderRadius: 10, padding: '0 14px', cursor: 'pointer' }}>{share.copied ? 'Copied' : 'Copy'}</button>
            </div>
            <div onClick={() => void onToggleRequireKey()} style={{ display: 'flex', alignItems: 'center', gap: 11, cursor: 'pointer', padding: 2 }}>
              <span style={{ width: 34, height: 20, borderRadius: 12, flex: 'none', background: share.requireKey ? accent : '#d6d3cd', position: 'relative', transition: 'background .15s' }}>
                <span style={{ position: 'absolute', top: 2, left: share.requireKey ? 16 : 2, width: 16, height: 16, borderRadius: '50%', background: '#fff', transition: 'left .15s', boxShadow: '0 1px 2px rgba(0,0,0,0.2)' }} />
              </span>
              <div style={{ flex: 1 }}>
                <div style={{ fontSize: 13.5, fontWeight: 500, color: '#1c1917' }}>Require an access key</div>
                <div style={{ fontSize: 12, color: '#a8a29e' }}>Adds a second secret they must enter too.</div>
              </div>
            </div>
            {share.requireKey && (
              <div style={{ display: 'flex', alignItems: 'center', gap: 10, background: '#faf9f7', border: '1px solid #e7e5e4', borderRadius: 10, padding: '11px 13px' }}>
                <span style={{ fontSize: 11.5, color: '#a8a29e', flex: 'none' }}>Access key</span>
                <span style={{ flex: 1, fontFamily: "'JetBrains Mono', monospace", fontSize: 13, letterSpacing: '0.04em', color: '#1c1917', textAlign: 'right' }}>{share.accessKey}</span>
              </div>
            )}
            <button onClick={() => setShare(null)} style={{ alignSelf: 'flex-end', fontFamily: 'inherit', fontSize: 13, fontWeight: 500, color: '#fff', background: '#1c1917', border: 'none', borderRadius: 9, padding: '8px 18px', cursor: 'pointer' }}>Done</button>
          </div>
        </>
      )}

      {/* remove modal */}
      {removeVaultState && (
        <>
          <div onClick={() => setRemoveVaultState(null)} style={{ position: 'fixed', inset: 0, zIndex: 72, background: 'rgba(28,25,23,0.30)' }} />
          <div style={{ position: 'fixed', zIndex: 73, top: '50%', left: '50%', transform: 'translate(-50%,-50%)', width: 'min(412px,92vw)', background: '#fff', borderRadius: 16, boxShadow: '0 24px 60px rgba(28,25,23,0.28)', padding: 20, display: 'flex', flexDirection: 'column', gap: 14 }}>
            <div>
              <div style={{ fontSize: 16, fontWeight: 600, letterSpacing: '-0.01em' }}>Remove “{removeVaultState.name}”?</div>
              <div style={{ fontSize: 13, color: '#78716c', marginTop: 4, lineHeight: 1.5 }}>{removeVaultState.trash ? 'The folder and its notes will be moved to the Trash.' : 'The folder stays on your computer — it’s only removed from asp.'}</div>
            </div>
            <div style={{ display: 'flex', alignItems: 'center', gap: 9, background: '#faf9f7', border: '1px solid #ededea', borderRadius: 10, padding: '9px 12px' }}>
              <Icon.FolderIcon style={{ flex: 'none' }} />
              <span style={{ fontFamily: "'JetBrains Mono', monospace", fontSize: 12, color: '#57534e', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', flex: 1 }}>{removeVaultState.path}</span>
            </div>
            <div onClick={() => setRemoveVaultState((r) => (r ? { ...r, trash: !r.trash } : r))} style={{ display: 'flex', alignItems: 'flex-start', gap: 11, cursor: 'pointer', padding: 2 }}>
              <span style={{ width: 34, height: 20, borderRadius: 12, flex: 'none', background: removeVaultState.trash ? '#c0392b' : '#d6d3cd', position: 'relative', transition: 'background .15s', marginTop: 1 }}>
                <span style={{ position: 'absolute', top: 2, left: removeVaultState.trash ? 16 : 2, width: 16, height: 16, borderRadius: '50%', background: '#fff', transition: 'left .15s', boxShadow: '0 1px 2px rgba(0,0,0,0.2)' }} />
              </span>
              <div style={{ flex: 1 }}>
                <div style={{ fontSize: 13.5, fontWeight: 500, color: '#1c1917' }}>Also move the folder to the Trash</div>
                <div style={{ fontSize: 12, color: '#a8a29e', marginTop: 1 }}>{removeVaultState.trash ? 'It will appear in your system Trash.' : 'Nothing on disk changes.'}</div>
              </div>
            </div>
            <div style={{ display: 'flex', justifyContent: 'flex-end', gap: 8, marginTop: 2 }}>
              <button onClick={() => setRemoveVaultState(null)} style={{ fontFamily: 'inherit', fontSize: 13, fontWeight: 500, color: '#57534e', background: '#fff', border: '1px solid #e0ddd8', borderRadius: 9, padding: '8px 16px', cursor: 'pointer' }}>Cancel</button>
              <button onClick={() => void confirmRemove()} style={{ fontFamily: 'inherit', fontSize: 13, fontWeight: 500, color: '#fff', background: '#c0392b', border: 'none', borderRadius: 9, padding: '8px 16px', cursor: 'pointer' }}>{removeVaultState.trash ? 'Remove & Trash folder' : 'Remove from asp'}</button>
            </div>
          </div>
        </>
      )}
    </>
  );
}
