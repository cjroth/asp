// Vault Editor — the Context Desktop app. A faithful React port of the new
// "Vault Editor" design canvas, wired to the real backend (Tauri commands →
// asp-desktop-engine → asp-core). No protocol logic lives here; every vault,
// file, history and sync behavior is a command call. Cosmetic vault metadata
// (name/color/emoji) and view prefs (theme, font, sidebar, hidden/pretty) are
// local-only and never touch the protocol.
import { open } from '@tauri-apps/plugin-dialog';
import { listen } from '@tauri-apps/api/event';
import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { api, type Api, type BranchGraphData, type ClonePhase, type FileEntry, type GitStatus, type HistEvent, type PendingDiff, type VaultInfo, type VaultStatus } from './lib/api';
import { gitUrlScheme } from './lib/giturl';
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
import TabBar from './vault/TabBar';
import { buildHash, closeAll, closeOthers, closeTab, closeToLeft, closeToRight, loadOpenTabs, parseHash, remapTabs, removeTabs, reorderTabs, saveOpenTabs, withTab } from './vault/tabs';
import { WELCOME_MD } from './vault/welcome';
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
  /** The historical read is still in flight (time-travel) — show a loading hint. */
  loading?: boolean;
  /** The file existed at this instant but its content blob isn't on this node —
   *  show "content unavailable" instead of a blank editable pane (BUG B). */
  contentMissing?: boolean;
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

// One-shot guard: restore the vault+file from the URL hash only on the FIRST App
// mount after a genuine page load. A real refresh re-executes this module (reset
// to false → restore runs); within a single test session the module persists, so
// later <App/> mounts in the same file don't spuriously re-restore from a stale
// (in-session) hash. Tests reset it via __resetUrlRestore() in beforeEach.
let URL_RESTORE_DONE = false;
export function __resetUrlRestore(): void {
  URL_RESTORE_DONE = false;
}

const currentHash = (): string => (typeof window !== 'undefined' && window.location ? window.location.hash : '');

// A friendly default name for an auto-created branch, derived from the instant it
// forked from (e.g. "edit-jun30-1405"). The backend content-hashes the id (incl.
// site + lamport) so concurrent same-name forks still get distinct branches.
const AUTO_MONTHS = ['jan', 'feb', 'mar', 'apr', 'may', 'jun', 'jul', 'aug', 'sep', 'oct', 'nov', 'dec'];
function autoBranchName(fromMs: number): string {
  const d = new Date(fromMs);
  const p2 = (n: number) => (n < 10 ? '0' : '') + n;
  return `edit-${AUTO_MONTHS[d.getMonth()]}${d.getDate()}-${p2(d.getHours())}${p2(d.getMinutes())}`;
}

// Human label for a startup scan phase, shown next to the progress bar.
function scanPhaseLabel(p: { phase: string }): string {
  return p.phase === 'scanning' ? 'Scanning your vault…' : p.phase === 'hashing' ? 'Reading changed files…' : p.phase === 'saving' ? 'Saving…' : 'Loading your vaults…';
}

// Display name of the checked-out branch (for the "restore onto <branch>" button).
function lanes0Name(graph: BranchGraphData | null, currentBranch: string): string {
  return graph?.branches.find((b) => b.id === currentBranch)?.name ?? 'this branch';
}

// ---- git push (git-bridge §7.2) — small testable helpers ----

// Whether the "Commit & push to git" affordance shows: a git-configured vault
// (`gitStatus` non-null) AND the desktop shell (the browser can't push — the web
// binding rejects, spec non-goal). Kept pure so a test can assert the gate.
export function canPushGit(gitStatus: GitStatus | null, desktop: boolean): boolean {
  return !!gitStatus && desktop;
}

// Pre-fill message from the pending diff: `asp: N file(s) changed (a, b, c…)`.
// Empty for a clean tree (nothing to commit → the confirm button stays disabled).
export function gitPushSummary(diff: Pick<PendingDiff, 'filesChanged' | 'paths'>): string {
  if (diff.filesChanged === 0) return '';
  const shown = diff.paths.slice(0, 3).join(', ');
  const more = diff.paths.length > 3 ? '…' : '';
  const suffix = diff.paths.length ? ` (${shown}${more})` : '';
  return `asp: ${diff.filesChanged} file(s) changed${suffix}`;
}

// Result of a push attempt for the dialog to render. `ok` drives the success state;
// `error` carries a friendly message (frozen → rebaseline; clean → nothing to commit).
export interface GitPushOutcome {
  ok: boolean;
  pushedSha: string | null;
  commits: number;
  error: string | null;
  status?: GitStatus | null;
}

// Run the manual push (author plan + push) and map typed failures to friendly text.
// Pure over the injected `client` so a test can drive it without rendering the app.
export async function runGitPush(
  client: Pick<Api, 'gitPush' | 'gitStatus'>,
  id: string,
  message: string,
): Promise<GitPushOutcome> {
  try {
    const res = await client.gitPush(id, message);
    if (res.commits === 0) {
      return { ok: false, pushedSha: null, commits: 0, error: 'Nothing to commit — no changes since the last push.' };
    }
    const status = await client.gitStatus(id);
    return { ok: true, pushedSha: res.pushedSha, commits: res.commits, error: null, status };
  } catch (err) {
    const msg = err instanceof Error ? err.message : String(err);
    const friendly = /frozen|rewritten|rebaseline/i.test(msg)
      ? 'History was rewritten upstream — run rebaseline.'
      : /nothing to commit/i.test(msg)
        ? 'Nothing to commit.'
        : msg;
    return { ok: false, pushedSha: null, commits: 0, error: friendly };
  }
}

export default function App() {
  const desktop = isDesktop();
  const [prefs, setPrefsState] = useState<Prefs>(loadPrefs);
  const accent = prefs.accent;
  const accentSoft = accent + '22';
  const fontFamily = fontFamilyOf();
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
  // The active vault's git-bridge status chip DTO, or null for a non-git vault
  // (git-bridge §7.2). Fetched on open + after a pull + on a light interval.
  const [gitStatus, setGitStatus] = useState<GitStatus | null>(null);
  const [activeId, setActiveId] = useState<string | null>(null);

  const [files, setFiles] = useState<FileEntry[]>([]);
  const [expanded, setExpanded] = useState<Record<string, boolean>>({});
  const [selectedPath, setSelectedPath] = useState<string | null>(null);
  // Multi-selection: every file path that's currently highlighted (always
  // includes `selectedPath`, the active/editor file). `anchorPath` is the last
  // plainly-clicked file — the fixed end of a shift-range.
  const [selectedPaths, setSelectedPaths] = useState<Set<string>>(new Set());
  const [anchorPath, setAnchorPath] = useState<string | null>(null);
  // Open tabs for the ACTIVE vault (file paths). Additive to selection:
  // `selectedPath` stays the active/editor file; tabs just remember what's open.
  const [openTabs, setOpenTabs] = useState<string[]>([]);

  const [paint, setPaint] = useState<Paint | null>(null);
  const [docText, setDocText] = useState('');
  const [saving, setSaving] = useState(false);

  const [histRaw, setHistRaw] = useState<HistEvent[]>([]);
  // The branch/tag network graph that drives the timeline lanes + fork edges + tag
  // flags. Loaded alongside history when the History panel is open.
  const [graph, setGraph] = useState<BranchGraphData | null>(null);
  const [currentBranch, setCurrentBranch] = useState<string>('main');
  // Non-blocking "you're now on a new branch" banner after an edit-in-the-past fork.
  const [branchBanner, setBranchBanner] = useState<{ name: string; from: number } | null>(null);
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
  // Inline rename of an open TAB (kept separate from the file-tree `renaming`
  // state so a tab rename never also turns the tree row into an input).
  const [tabRenaming, setTabRenaming] = useState<string | null>(null);
  const [tabRenameValue, setTabRenameValue] = useState('');
  // Right-click-a-tab context menu (Rename / Close / Delete).
  const [tabCtx, setTabCtx] = useState<{ x: number; y: number; path: string } | null>(null);

  const [sidebarW, setSidebarW] = useState(prefs.sidebarW);
  const [histBarH, setHistBarH] = useState(prefs.histBarH);
  const [resizingBar, setResizingBar] = useState(false);

  const [entry, setEntry] = useState<'new' | 'connect' | null>(null);
  const [newVaultName, setNewVaultName] = useState('');
  const [ticket, setTicket] = useState('');
  const [authKey, setAuthKey] = useState('');
  // Git-bridge connect fields (git-bridge §7.2). `token` replaces the access key
  // for an https git URL; `depth` (blank = full history) drives a shallow import.
  const [token, setToken] = useState('');
  const [depth, setDepth] = useState('');
  // "Also import open branches" (git-open-branches §5): a first-clone-only snapshot of
  // every unmerged remote branch as a live ASP branch. Default OFF.
  const [allBranches, setAllBranches] = useState(false);
  const [advancedOpen, setAdvancedOpen] = useState(false);
  const [connecting, setConnecting] = useState(false);
  const [cloneProg, setCloneProg] = useState<{ done: number; total: number; phase: ClonePhase } | null>(null);
  // Surfaced clone/connect failure (e.g. a stalled transfer) so the connect dialog
  // shows what went wrong instead of silently resetting.
  const [connectError, setConnectError] = useState<string | null>(null);
  const [connectDest, setConnectDest] = useState<string | null>(null);
  // Non-dismissable "working…" overlay shown while a folder is being added
  // (capture_rescan hashes every file) or a vault is being opened
  // (list_files + tree build). Both scale with vault size, so on a large
  // folder they take long enough that the UI would otherwise look frozen.
  // The string is the label to show; null hides the overlay.
  const [opening, setOpening] = useState<string | null>(null);
  // True while the desktop shell is still reopening previously-saved vaults in
  // the background (see the `vaults-ready` Tauri event). Lets the connect screen
  // show "Loading your vaults…" instead of a bare empty state on cold start.
  // Web loads its registry synchronously, so it never enters this state.
  const [vaultsLoading, setVaultsLoading] = useState(isDesktop());
  // Determinate startup progress from the shell's `vault-scan-progress` events, so
  // a big vault's reconcile shows a real bar instead of an indeterminate spinner.
  const [scanProgress, setScanProgress] = useState<{ done: number; total: number; phase: string } | null>(null);

  const [share, setShare] = useState<{ id: string; code: string; requireKey: boolean; accessKey: string; copied: boolean; unavailable?: boolean } | null>(null);
  const [localRelayOn, setLocalRelayOn] = useState(false);
  const [vaultCtx, setVaultCtx] = useState<{ x: number; y: number; id: string; vaultId: string; name: string } | null>(null);
  const [customize, setCustomize] = useState<CustomizeInit | null>(null);
  const [removeVaultState, setRemoveVaultState] = useState<{ id: string; name: string; path: string; trash: boolean } | null>(null);
  // The "Commit & push to git" dialog (git-bridge §7.2), or null when closed.
  // `diff` is the pending change set (pre-fills `message`); `done` holds the pushed
  // sha after a successful push; `error` shows a frozen/nothing-to-commit message.
  const [gitPushState, setGitPushState] = useState<{ message: string; diff: PendingDiff | null; loading: boolean; pushing: boolean; error: string | null; done: string | null } | null>(null);
  // Pending delete awaiting confirmation. `paths` is the exact set to remove
  // (already expanded for multi-selection); `label`/`count` drive the message.
  const [deleteConfirm, setDeleteConfirm] = useState<{ paths: string[]; label: string; count: number } | null>(null);

  // refs for values used inside imperative handlers / async flows
  const activeIdRef = useRef<string | null>(null);
  const selectedRef = useRef<string | null>(null);
  const selectedPathsRef = useRef<Set<string>>(new Set());
  const bufferRef = useRef('');
  // True while a connect-modal clone is running, so the shared `vault-scan-progress`
  // event drives the clone bar (`cloneProg`) instead of the cold-start reconcile bar.
  const cloningRef = useRef(false);
  const playheadRef = useRef<number | null>(null);
  const viewRef = useRef<View | null>(null);
  const nowRef = useRef(now);
  const saveTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const histTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const dirtyRef = useRef(false);
  const filesRef = useRef<FileEntry[]>([]);
  const contentRef = useRef<Record<string, string>>({});
  const paintSeq = useRef(0);
  const vaultsRef = useRef<VaultInfo[]>([]);
  const openTabsRef = useRef<string[]>([]);
  const openVaultRef = useRef<(id: string) => Promise<void>>(async () => {});
  const urlHadSelection = useRef(false);
  activeIdRef.current = activeId;
  selectedRef.current = selectedPath;
  selectedPathsRef.current = selectedPaths;
  openTabsRef.current = openTabs;
  playheadRef.current = playhead;
  viewRef.current = view;
  nowRef.current = now;

  const curView = useCallback((): View => view || defaultView(now), [view, now]);
  const timeTravel = playhead != null && playhead < now;

  const events: TrackEvent[] = useMemo(() => buildEvents(histRaw), [histRaw]);

  // ---------- data loading ----------
  const refreshVaults = useCallback(async () => {
    const vs = await api.listVaults();
    vaultsRef.current = vs; // keep fresh synchronously for openVault/url-sync
    setVaults(vs);
    return vs;
  }, []);

  // The stable cross-session vault identity used in the URL hash + tabs storage
  // key. Falls back to the local handle if the vault isn't loaded yet.
  const vidOf = useCallback((id: string | null): string | null => {
    if (!id) return null;
    return vaultsRef.current.find((v) => v.id === id)?.vault_id ?? id;
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
    // The timeline IS the network graph now: pull lanes/edges/tags + HEAD too.
    try {
      const [g, cur] = await Promise.all([api.branchGraph(id, 400), api.currentBranch(id)]);
      setGraph(g);
      setCurrentBranch(cur);
    } catch {
      setGraph(null);
    }
    setNow(Date.now());
  }, []);

  // Building the history timeline scans the whole log (seconds on a big vault),
  // so only fetch it while the History/Log view is actually open. Opening either
  // triggers an immediate fetch (see onTabHistory/onTabLog).
  const histNeededRef = useRef(false);
  histNeededRef.current = histOpen || logOpen;
  const scheduleHistory = useCallback(
    (id: string) => {
      if (!histNeededRef.current) return;
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

  // Re-read the currently-open file so a peer's live edit to it shows up. Only
  // when the editor isn't dirty (never clobber unsaved local edits) and we're on
  // the live head (not time-travelling). Repaints only if the bytes changed.
  const refreshActiveContent = useCallback(async () => {
    const id = activeIdRef.current;
    const path = selectedRef.current;
    if (!id || !path || dirtyRef.current) return;
    const ph = playheadRef.current;
    if (ph != null && ph < nowRef.current) return; // viewing history; leave it be
    try {
      const fresh = await api.readFile(id, path);
      // Bail if the user moved on while we were awaiting, or started editing.
      if (activeIdRef.current !== id || selectedRef.current !== path || dirtyRef.current) return;
      const key = `${id}::${path}`;
      if (contentRef.current[key] === fresh) return; // unchanged — no repaint
      contentRef.current[key] = fresh;
      bufferRef.current = fresh;
      const seq = ++paintSeq.current;
      setDocText(fresh);
      setPaint({ source: fresh, readOnly: false, notExist: false, key: `${path}#live#${seq}` });
    } catch {
      /* transient backend error — try again next poll */
    }
  }, []);

  useEffect(() => {
    void api.getIdentity().then(setIdentity).catch(() => {});
    void (async () => {
      const vs = await refreshVaults();
      // Any vaults already available (web registry, or folders the desktop shell
      // finished reopening before we mounted) means we're not waiting on an empty
      // cold start — drop the loading hint. The `vaults-changed` event below also
      // clears it as each background-reopened vault lands.
      if (vs.length) setVaultsLoading(false);
      // Race-proof fallback: the `vaults-ready` event is one-shot and fires from
      // the shell's startup thread, often before our listener attaches (e.g. an
      // empty config reopens instantly), so it can be missed entirely. Querying
      // readiness here clears the gate deterministically once reopen has finished,
      // regardless of whether we caught the event.
      else if (desktop) {
        try {
          if (await api.vaultsReady()) setVaultsLoading(false);
        } catch {
          /* not the desktop shell, or command unavailable — leave it to the event */
        }
      }
      // Refresh-restore: on the first mount after a real page load, if the URL
      // hash names a known vault, open it (openVault then picks the hashed file).
      // Works identically on desktop and web — the hash is read the same way.
      if (!URL_RESTORE_DONE) {
        URL_RESTORE_DONE = true;
        const parsed = parseHash(currentHash());
        if (parsed) {
          const match = vs.find((v) => v.vault_id === parsed.vaultId);
          if (match) {
            try {
              await openVaultRef.current(match.id);
            } catch {
              /* fall through to the default connect screen */
            }
          }
        }
      }
      await refreshStatuses(vs);
    })();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [refreshVaults, refreshStatuses]);

  // Persist the active vault's open tabs whenever they change.
  useEffect(() => {
    const vid = vidOf(activeId);
    if (vid) saveOpenTabs(vid, openTabs);
  }, [openTabs, activeId, vidOf]);

  // Keep the active vault+file in the URL hash (replaceState — never spam
  // history). The clear branch only fires on a real transition AWAY from a
  // selection, so it can't wipe the page-load hash before the initial restore
  // (which runs in the mount effect above) has read it.
  useEffect(() => {
    try {
      const vid = vidOf(activeId);
      if (vid && selectedPath) {
        window.history.replaceState(null, '', buildHash(vid, selectedPath));
        urlHadSelection.current = true;
      } else if (urlHadSelection.current) {
        window.history.replaceState(null, '', window.location.pathname + window.location.search);
        urlHadSelection.current = false;
      }
    } catch {
      /* ignore */
    }
  }, [activeId, selectedPath, vidOf]);

  // No polling. Refreshes are fully event-driven: desktop reacts to the engine's
  // realtime `vault-changed` / `vaults-changed` Tauri events (see the effects
  // below), web to its live-sync push callback. Re-listing a 28k-file tree on a
  // 10s timer was both wasted work (~1s each tick) and the wrong model.

  // Web live sync: a browser can't be *pushed* to, but it can dial the upstream
  // and hold the link open. While a web vault is open in the editor we keep that
  // live connection up; each remote push lands in the engine and refreshes the
  // tree + open file in realtime. Desktop is excluded (its engine is already
  // live; the poll above re-reads it).
  // Listing the tree is O(N) (seconds-adjacent on a huge vault), and a sync burst
  // fires this per row, so coalesce the tree refresh while keeping the open
  // file's content update immediate (cheap single-file read).
  const fileRefreshTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const liveRefresh = useCallback(
    (id: string) => {
      if (fileRefreshTimer.current) clearTimeout(fileRefreshTimer.current);
      fileRefreshTimer.current = setTimeout(() => void refreshFiles(id).catch(() => {}), 400);
      scheduleHistory(id);
      void refreshActiveContent();
    },
    [refreshFiles, scheduleHistory, refreshActiveContent],
  );
  const liveRefreshRef = useRef(liveRefresh);
  liveRefreshRef.current = liveRefresh;
  useEffect(() => {
    if (desktop || screen !== 'editor' || !activeId) return;
    const id = activeId;
    void api.startLiveSync(id, () => liveRefreshRef.current(id));
    return () => {
      void api.stopLiveSync(id);
    };
  }, [desktop, screen, activeId]);

  // Desktop realtime: the engine emits 'vault-changed' (the changed vault_id) the
  // instant a peer's edit integrates — its background connections are already
  // live. This is the desktop's whole refresh path (no polling): bump that vault's
  // status (the "last synced" label / status bar) wherever we are, and if it's the
  // open editor vault, refresh its tree + open file too. Web has no Tauri events —
  // it refreshes via the live-sync callback instead.
  useEffect(() => {
    if (!desktop) return;
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    void listen<string>('vault-changed', (e) => {
      const changedVid = e.payload;
      const v = vaultsRef.current.find((x) => x.vault_id === changedVid);
      if (v) void api.getStatus(v.id).then((st) => setStatuses((p) => ({ ...p, [v.id]: st }))).catch(() => {});
      const cur = activeIdRef.current;
      const curVid = vaultsRef.current.find((x) => x.id === cur)?.vault_id;
      if (cur && changedVid === curVid) liveRefreshRef.current(cur);
    }).then((u) => (cancelled ? u() : (unlisten = u)));
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [desktop]);

  // Saved folders reopen in the background at startup (a big vault's reconcile is
  // slow), so the window can appear before they're loaded. The engine emits
  // `vaults-changed` the instant each reopened vault lands; refresh the list and
  // drop the cold-start loading hint then, instead of waiting on a poll.
  useEffect(() => {
    if (!desktop) return;
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    void listen('vaults-changed', () => {
      setVaultsLoading(false);
      setScanProgress(null); // a vault landed → reconcile of the scanned one is done
      void refreshVaults().then(refreshStatuses).catch(() => {});
    }).then((u) => (cancelled ? u() : (unlisten = u)));
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [desktop, refreshVaults, refreshStatuses]);

  // Determinate startup progress: the shell streams (done, total, phase) as it
  // reconciles each saved vault on cold start. Drives the progress bar below.
  useEffect(() => {
    if (!desktop) return;
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    void listen<{ done: number; total: number; phase: string }>('vault-scan-progress', (e) => {
      // A git clone reuses this event (phases fetching→replaying→saving→materialize).
      // While a clone is in flight, drive the connect modal's determinate bar; otherwise
      // it's the cold-start reconcile bar.
      if (cloningRef.current) setCloneProg({ done: e.payload.done, total: e.payload.total, phase: e.payload.phase as ClonePhase });
      else setScanProgress(e.payload);
    }).then((u) => (cancelled ? u() : (unlisten = u)));
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [desktop]);

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
    let ttTimer: ReturnType<typeof setTimeout> | undefined;
    if (live) {
      void (async () => {
        try {
          const content = await api.readFile(id, path);
          if (cancelled || seq !== paintSeq.current) return;
          contentRef.current[key] = content;
          bufferRef.current = content;
          dirtyRef.current = false;
          setDocText(content);
          setPaint({ source: content, readOnly: false, notExist: false, key: `${path}#live#${seq}` });
        } catch {
          if (!cancelled) setPaint(null);
        }
      })();
    } else {
      // Debounce time-travel reads: dragging the history-slider handle fires a
      // playhead update on every pointermove, and each read_file_at folds the
      // log as-of that instant. Coalesce a drag into one read after a short
      // settle (a discrete tick-jump still feels instant at 60ms) so a scrub
      // never queues dozens of backend reads on a large vault.
      ttTimer = setTimeout(() => {
        if (cancelled || seq !== paintSeq.current) return;
        // Show a loading hint while the (path-scoped, but still async) history read
        // is in flight — mirrors the timeline popover's loading state.
        setDocText('');
        setPaint({ source: '', readOnly: true, notExist: false, loading: true, key: `${path}#tt-loading${ph}#${seq}` });
        void (async () => {
          try {
            const at = await api.readFileAt(id, path, Math.floor(ph / 1000));
            if (cancelled || seq !== paintSeq.current) return;
            const missing = !!at.contentMissing;
            setDocText(missing ? '' : at.exists ? at.content : '');
            // Time travel is EDITABLE: the first edit here auto-forks a branch at this
            // instant (see onEditorChange), so we never overwrite the past. But a point
            // whose content is unavailable must stay read-only — editing it would fork
            // with an empty body and silently lose the real history.
            setPaint({
              source: missing ? '' : at.content,
              readOnly: missing,
              notExist: !at.exists,
              contentMissing: missing,
              key: `${path}#tt${ph}#${seq}`,
            });
          } catch {
            if (!cancelled) setPaint(null);
          }
        })();
      }, 60);
    }
    return () => {
      cancelled = true;
      if (ttTimer) clearTimeout(ttTimer);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeId, selectedPath, playhead]);

  // Edit-in-the-past ⇒ auto-branch (§2.5). Editing while scrubbed back forks a new
  // branch AT the scrubbed instant, checks it out, and lands the edit there — main
  // is never overwritten and no manual "create branch" step is needed. Guarded so
  // only the first keystroke forks; subsequent keystrokes ride the new live branch.
  const forkingRef = useRef(false);
  const autoBranchFromEdit = useCallback(async () => {
    if (forkingRef.current) return;
    const id = activeIdRef.current;
    const path = selectedRef.current;
    const ph = playheadRef.current;
    if (!id || !path || ph == null) return;
    forkingRef.current = true;
    setSaving(true);
    try {
      const name = autoBranchName(ph);
      await api.forkBranchAt(id, name, Math.floor(ph / 1000)); // fork + checkout at that instant
      // We're now live on the new branch (at the historical state). Land the edit.
      await api.writeFile(id, path, bufferRef.current);
      contentRef.current[`${id}::${path}`] = bufferRef.current;
      dirtyRef.current = false;
      setPlayhead(null); // return to live — on the new branch
      setBranchBanner({ name, from: ph });
      await refreshFiles(id);
      void api.currentBranch(id).then(setCurrentBranch).catch(() => {});
      void api.branchGraph(id, 400).then(setGraph).catch(() => {});
      scheduleHistory(id);
    } catch {
      /* ignore — stays on current branch */
    } finally {
      forkingRef.current = false;
      setSaving(false);
    }
  }, [refreshFiles, scheduleHistory]);

  const onEditorChange = useCallback(
    (src: string) => {
      bufferRef.current = src;
      if (activeIdRef.current && selectedRef.current) contentRef.current[`${activeIdRef.current}::${selectedRef.current}`] = src;
      // Scrubbed into the past → the edit forks a branch instead of saving to HEAD.
      if (playheadRef.current != null && playheadRef.current < nowRef.current) {
        void autoBranchFromEdit();
        return;
      }
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
    [scheduleHistory, autoBranchFromEdit],
  );

  // ---------- vault open / switch ----------
  // Run a potentially-slow open/add operation, showing a non-dismissable
  // "working…" overlay only if it runs longer than a short threshold — so a
  // quick vault switch never flashes a spinner, but a large folder (where
  // capture_rescan / list_files / tree-build take seconds) shows progress
  // instead of looking frozen. Nesting is safe: the inner call clears the
  // overlay when its work is done, and the outer's finally is a harmless no-op.
  const withOpening = useCallback(async (label: string, fn: () => Promise<void>) => {
    const timer = setTimeout(() => setOpening(label), 140);
    try {
      await fn();
    } finally {
      clearTimeout(timer);
      setOpening(null);
    }
  }, []);

  const openVault = useCallback(
    (id: string) =>
      withOpening('Opening vault…', async () => {
      await flushSave();
      const fs = await refreshFiles(id);
      const tree = buildTree(fs);
      const vid = vaultsRef.current.find((v) => v.id === id)?.vault_id ?? id;
      const fileSet = new Set(fs.filter((f) => !f.is_dir).map((f) => f.path));
      // Restore this vault's previously-open tabs (dropping any now-missing file).
      const stored = loadOpenTabs(vid).filter((p) => fileSet.has(p));
      // Pick the active file: the URL hash (if it points at THIS vault and the
      // file still exists), else the first restored tab, else today's default.
      const parsed = parseHash(currentHash());
      let active: string | null = null;
      if (parsed && parsed.vaultId === vid && fileSet.has(parsed.path)) active = parsed.path;
      else if (stored.length) active = stored[0];
      else active = firstSelectable(tree);
      const tabs = active ? withTab(stored, active) : stored;
      const exp: Record<string, boolean> = {};
      if (active) {
        const parts = active.split('/');
        for (let i = 1; i < parts.length; i++) exp[parts.slice(0, i).join('/')] = true;
      }
      // All state lands in ONE batched update (after the awaits) so the active
      // vault id and its tab list never disagree — otherwise the tab-persistence
      // effect would briefly save the previous vault's tabs under the new key.
      contentRef.current = {};
      setActiveId(id);
      setScreen('editor');
      setVaultMenuOpen(false);
      setPlayhead(null);
      setView(defaultView(Date.now()));
      setNow(Date.now());
      setExpanded(exp);
      setOpenTabs(tabs);
      setSelectedPath(active);
      setSelectedPaths(active ? new Set([active]) : new Set());
      setAnchorPath(active);
      scheduleHistory(id);
      // Fetch this vault's status (file count, peers, head) so the status bar is
      // populated immediately — desktop refreshes it on events, but a freshly
      // opened/created vault (esp. web) otherwise has none until something changes.
      void api.getStatus(id).then((st) => setStatuses((p) => ({ ...p, [id]: st }))).catch(() => {});
      }),
    [withOpening, flushSave, refreshFiles, scheduleHistory],
  );
  openVaultRef.current = openVault;

  // Git-bridge §7.2: keep the active vault's git-status chip fresh — fetched on
  // open, on a light 30s interval, and after a pull. `gitStatus()` returns null
  // for a non-git vault (so no chip renders). Resets when switching vaults.
  useEffect(() => {
    const id = activeId;
    setGitStatus(null);
    if (!id) return;
    let cancelled = false;
    const load = () => {
      void api.gitStatus(id).then((s) => { if (!cancelled) setGitStatus(s); }).catch(() => {});
    };
    load();
    const iv = setInterval(load, 30000);
    return () => { cancelled = true; clearInterval(iv); };
  }, [activeId]);

  // Pull new upstream commits into the active git vault, then refresh the tree,
  // the open file, the timeline and the status chip.
  const onGitPull = useCallback(async () => {
    const id = activeIdRef.current;
    if (!id) return;
    setVaultMenuOpen(false);
    try {
      await api.gitPull(id);
      await refreshFiles(id);
      await refreshActiveContent();
      scheduleHistory(id);
      const s = await api.gitStatus(id);
      setGitStatus(s);
    } catch (err) {
      console.error('git pull failed', err);
    }
  }, [refreshFiles, refreshActiveContent, scheduleHistory]);

  // Open the "Commit & push to git" dialog: fetch the pending diff and pre-fill an
  // editable commit message with a summary of what would be pushed (git-bridge §7.2).
  const openGitPush = useCallback(async () => {
    const id = activeIdRef.current;
    if (!id) return;
    setVaultMenuOpen(false);
    setGitPushState({ message: '', diff: null, loading: true, pushing: false, error: null, done: null });
    try {
      const diff = await api.gitPendingDiff(id);
      setGitPushState({ message: gitPushSummary(diff), diff, loading: false, pushing: false, error: null, done: null });
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      setGitPushState({ message: '', diff: null, loading: false, pushing: false, error: msg, done: null });
    }
  }, []);

  // Confirm the push: author a plan + push, refresh the status chip, then show the
  // pushed sha (or a friendly error) inside the dialog.
  const confirmGitPush = useCallback(async () => {
    const id = activeIdRef.current;
    if (!id) return;
    let message = '';
    setGitPushState((s) => {
      if (!s) return s;
      message = s.message;
      return { ...s, pushing: true, error: null };
    });
    const outcome = await runGitPush(api, id, message);
    if (outcome.ok) setGitStatus(outcome.status ?? null);
    setGitPushState((s) => (s ? { ...s, pushing: false, error: outcome.error, done: outcome.ok ? outcome.pushedSha : null } : s));
  }, []);

  const selectFile = useCallback(
    async (path: string) => {
      await flushSave();
      setOpenTabs((t) => withTab(t, path));
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
      setOpenTabs((t) => withTab(t, path));
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
      setTabRenaming(null);
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
      setOpenTabs((t) => remapTabs(t, oldPath, newPath));

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
      setOpenTabs((t) => {
        let nt = t;
        for (const { src, dst } of moves) nt = remapTabs(nt, src, dst);
        return nt;
      });

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

  // Actually delete one or more paths (folders delete their whole subtree). Drives
  // both the single-file delete and the batch delete of a multi-selection. Runs
  // only AFTER the user confirms — entry points go through `requestDelete`.
  const performDelete = useCallback(
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
      const victims = Array.from(victimSet);
      const oldTabs = openTabsRef.current;
      const tabsAfter = removeTabs(oldTabs, victims);
      const sel = selectedRef.current;
      if (sel && victimSet.has(sel)) {
        // The active file is being deleted → switch to a surviving neighbor tab
        // (prefer the next, then previous); if none remain, fall back to the
        // vault's default file (and adopt it as the sole open tab).
        let na: string | null = null;
        const idx = oldTabs.indexOf(sel);
        if (idx >= 0) {
          for (let i = idx + 1; i < oldTabs.length && na == null; i++) if (tabsAfter.includes(oldTabs[i])) na = oldTabs[i];
          for (let i = idx - 1; i >= 0 && na == null; i--) if (tabsAfter.includes(oldTabs[i])) na = oldTabs[i];
        }
        if (na == null) na = tabsAfter.length ? tabsAfter[0] : firstSelectable(buildTree(next));
        setOpenTabs(na ? withTab(tabsAfter, na) : tabsAfter);
        setSelectedPath(na);
        setSelectedPaths(na ? new Set([na]) : new Set());
        setAnchorPath(na);
      } else {
        setOpenTabs(tabsAfter);
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

  // Open the confirm-before-delete modal for `paths`. Nothing is removed until the
  // user confirms (`confirmDelete` → `performDelete`). All delete entry points —
  // the file-tree menu, the tab menu, and the keyboard — funnel through here.
  const requestDelete = useCallback((paths: string[]) => {
    if (paths.length === 0) return;
    setCtxMenu(null);
    setTabCtx(null);
    setDeleteConfirm({ paths, label: basename(paths[0]), count: paths.length });
  }, []);

  const confirmDelete = useCallback(() => {
    setDeleteConfirm((dc) => {
      if (dc) performDelete(dc.paths);
      return null;
    });
  }, [performDelete]);

  // Context-menu / programmatic delete of a single node. If the node is a FILE
  // that's part of a multi-selection, delete the whole selection (batch delete);
  // otherwise just this node. Expands to the selection FIRST, then confirms.
  const deleteNode = useCallback(
    (path: string, isDir: boolean) => {
      const sel = selectedPathsRef.current;
      if (!isDir && sel.size > 1 && sel.has(path)) requestDelete(Array.from(sel));
      else requestDelete([path]);
    },
    [requestDelete],
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
        requestDelete(Array.from(sel));
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [requestDelete]);

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

  // Switch branch from the timeline's lane label (replaces the old dropdown).
  const onCheckoutBranch = useCallback(async (branchId: string) => {
    const id = activeIdRef.current;
    if (!id) return;
    await withOpening('Switching branch…', async () => {
      await flushSave();
      await api.checkoutBranch(id, branchId);
      contentRef.current = {}; // the working tree changed; drop cached file bodies
      setPlayhead(null);
      setCurrentBranch(branchId);
      await refreshFiles(id);
      const sel = selectedRef.current;
      if (sel) {
        try {
          const content = await api.readFile(id, sel);
          contentRef.current[`${id}::${sel}`] = content;
          bufferRef.current = content;
          setDocText(content);
          setPaint({ source: content, readOnly: false, notExist: false, key: `${sel}#live#${++paintSeq.current}` });
        } catch {
          /* file may not exist on this branch */
        }
      }
      void api.branchGraph(id, 400).then(setGraph).catch(() => {});
      scheduleHistory(id);
    });
  }, [flushSave, refreshFiles, scheduleHistory, withOpening]);

  // Tag the given instant (epoch ms) with a name — a marker on the timeline.
  const onCreateTag = useCallback(async (name: string, tsMs: number) => {
    const id = activeIdRef.current;
    if (!id) return;
    try {
      await api.createTag(id, name, Math.floor(tsMs / 1000));
      void api.branchGraph(id, 400).then(setGraph).catch(() => {});
    } catch {
      /* ignore */
    }
  }, []);

  const onDeleteTag = useCallback(async (tagId: string) => {
    const id = activeIdRef.current;
    if (!id) return;
    try {
      await api.deleteTag(id, tagId);
      void api.branchGraph(id, 400).then(setGraph).catch(() => {});
    } catch {
      /* ignore */
    }
  }, []);

  // Load the before/after content for a history event → the timeline's diff popup.
  // "After" is the file as of that event's instant; "before" is as of the previous
  // event that touched the same path (empty if this was its creation).
  const loadDiff = useCallback(
    async (ev: TrackEvent): Promise<{ path: string; kind: string; before: string; after: string } | null> => {
      const id = activeIdRef.current;
      if (!id) return null;
      const sec = Math.floor(ev.ts / 1000);
      const prev = events
        .filter((e) => e.path === ev.path && e.ts < ev.ts)
        .sort((a, b) => b.ts - a.ts)[0];
      try {
        const after = await api.readFileAt(id, ev.path, sec);
        const before = prev ? await api.readFileAt(id, ev.path, Math.floor(prev.ts / 1000)) : { exists: false, content: '' };
        return {
          path: ev.path,
          kind: ev.kind,
          before: before.exists ? before.content : '',
          after: after.exists ? after.content : '',
        };
      } catch {
        return null;
      }
    },
    [events],
  );

  const onTabHistory = useCallback(() => {
    setHistOpen((h) => {
      if (!h && activeIdRef.current) void refreshHistory(activeIdRef.current); // fetch on open
      return !h;
    });
    setLogOpen(false);
  }, [refreshHistory]);
  const onTabLog = useCallback(() => {
    setLogOpen((l) => {
      if (!l && activeIdRef.current) void refreshHistory(activeIdRef.current); // fetch on open
      return !l;
    });
    setHistOpen(false);
  }, [refreshHistory]);

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

  // ---------- theme ----------
  const onToggleTheme = useCallback(() => {
    const next = prefs.theme === 'dark' ? 'light' : 'dark';
    applyTheme(next);
    updatePrefs({ theme: next });
  }, [prefs.theme, updatePrefs]);

  // ---------- connect / new / share / remove / customize ----------
  const onOpenFolder = useCallback(async () => {
    try {
      const dir = await open({ directory: true });
      if (typeof dir === 'string') {
        // addLocalFolder runs capture_rescan (hashes every file on disk), which
        // scales with folder size — show the overlay so a large folder doesn't
        // look frozen. openVault then shows its own (for the list_files step).
        await withOpening('Opening folder…', async () => {
          const info = await api.addLocalFolder(dir);
          await refreshVaults();
          await openVault(info.id);
        });
      }
    } catch (err) {
      console.error('open folder failed', err);
      alert('Could not open that folder: ' + String((err as Error)?.message ?? err));
    }
  }, [openVault, refreshVaults, withOpening]);

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
      // A git URL routes to the git bridge; anything else is an ASP peer ticket.
      const scheme = gitUrlScheme(t);
      // SSH clones need the local ssh-agent, unavailable in the browser.
      if (scheme === 'ssh' && !desktop) {
        setConnectError('SSH clone isn’t supported in the browser — use an https:// URL or the desktop app.');
        return;
      }
      setConnecting(true);
      setConnectError(null);
      cloningRef.current = true;
      setCloneProg({ done: 0, total: 0, phase: scheme ? 'fetching' : 'receiving' });
      try {
        const onProg = (done: number, total: number, phase: ClonePhase) => setCloneProg({ done, total, phase });
        let info: VaultInfo;
        if (scheme) {
          // Blank depth = full history; otherwise a positive shallow import.
          const n = parseInt(depth.trim(), 10);
          const d = depth.trim() && n > 0 ? n : undefined;
          info = await api.cloneGit(connectDest || '', t, token.trim() || undefined, d, allBranches, onProg);
        } else {
          info = await api.cloneRemote(connectDest || '', t, authKey || undefined, onProg);
        }
        setTicket('');
        setAuthKey('');
        setToken('');
        setDepth('');
        setAllBranches(false);
        setAdvancedOpen(false);
        setConnectDest(null);
        setEntry(null);
        await refreshVaults();
        await openVault(info.id);
      } catch (err) {
        // Surface the failure in the dialog (a stalled transfer, bad ticket, …)
        // instead of silently resetting — the user was left staring at a spinner.
        console.error('clone failed', err);
        setConnectError(String((err as Error)?.message || err) || 'Connection failed. Please try again.');
      } finally {
        cloningRef.current = false;
        setConnecting(false);
        setCloneProg(null);
      }
    } else {
      // New vault: desktop adds a chosen folder; web creates a browser (OPFS) vault.
      if (desktop && !connectDest) return;
      try {
        const nm = newVaultName.trim();
        const info = desktop ? await api.addLocalFolder(connectDest!) : await api.createVault(nm || 'Untitled vault');
        if (nm) updateMeta(info.vault_id, { name: nm, hue: hueForId(info.vault_id), emoji: null });
        // Seed a welcome README into a genuinely EMPTY vault only — a desktop
        // folder that already holds files must never be clobbered. openVault()
        // below then selects it as the first file.
        const seedFiles = await api.listFiles(info.id);
        if (!seedFiles.length) await api.writeFile(info.id, 'README.md', WELCOME_MD);
        setNewVaultName('');
        setConnectDest(null);
        setEntry(null);
        await refreshVaults();
        await openVault(info.id);
      } catch (err) {
        console.error('create vault failed', err);
      }
    }
  }, [entry, connecting, ticket, connectDest, authKey, token, depth, allBranches, newVaultName, desktop, updateMeta, openVault, refreshVaults]);

  const onShareVault = useCallback(async (id: string) => {
    setVaultMenuOpen(false);
    // Browser (wasm/OPFS) vaults can't open a listening socket, so there's no
    // ticket to generate — show an honest "unavailable" state instead of an
    // endless "Generating…" spinner.
    if (!desktop) {
      setShare({ id, code: '', requireKey: false, accessKey: '', copied: false, unavailable: true });
      return;
    }
    setShare({ id, code: '', requireKey: false, accessKey: '', copied: false });
    void api.getLocalRelay().then(setLocalRelayOn).catch(() => {});
    try {
      const tkt = await api.setAllowConnections(id, true);
      setShare((s) => (s && s.id === id ? { ...s, code: tkt || '' } : s));
      await api.getStatus(id).then((st) => setStatuses((p) => ({ ...p, [id]: st })));
    } catch (err) {
      console.error('share failed', err);
    }
  }, [desktop]);

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

  // "Faster local syncing": co-host a relay so same-machine/LAN peers connect
  // locally instead of via the public n0 relay. Re-establishing re-mints the
  // active share's ticket, so re-fetch it after toggling.
  const onToggleLocalRelay = useCallback(async () => {
    const s = share;
    const next = !localRelayOn;
    setLocalRelayOn(next);
    try {
      await api.setLocalRelay(next);
      if (s && !s.unavailable) {
        const tkt = await api.setAllowConnections(s.id, true, s.requireKey ? s.accessKey : undefined);
        setShare((x) => (x && x.id === s.id ? { ...x, code: tkt || '' } : x));
      }
    } catch (err) {
      console.error(err);
      setLocalRelayOn(!next); // revert on failure
    }
  }, [share, localRelayOn]);

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
      setOpenTabs([]);
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

  // ---------- tab bar ----------
  // Clicking a tab makes it the active/editor file (collapsing any multi-select).
  const onTabSelect = useCallback(
    (path: string) => {
      setSelectedPaths(new Set([path]));
      setAnchorPath(path);
      void selectFile(path);
    },
    [selectFile],
  );

  // Closing a tab. If it's the active file, switch to closeTab's chosen neighbor
  // (or the empty state when it was the last tab).
  const onTabClose = useCallback(
    (path: string) => {
      const res = closeTab(openTabsRef.current, selectedRef.current, path);
      setOpenTabs(res.tabs);
      if (selectedRef.current === path) {
        if (res.active) {
          void flushSave();
          setSelectedPath(res.active);
          setSelectedPaths(new Set([res.active]));
          setAnchorPath(res.active);
        } else {
          setSelectedPath(null);
          setSelectedPaths(new Set());
          setAnchorPath(null);
        }
      }
    },
    [flushSave],
  );

  // Apply a multi-close transform (Close Others / Left / Right / All) to the open
  // tabs. These CLOSE tabs only — no file is deleted. If the active file is among
  // the closed tabs, switch to a surviving tab (prefer the kept tab nearest the
  // old active, scanning right then left); when nothing survives, fall back to the
  // vault's default file and adopt it as the sole open tab. Mirrors the survivor
  // selection in `performDelete`.
  const applyTabClose = useCallback(
    (tabsAfter: string[]) => {
      const oldTabs = openTabsRef.current;
      const sel = selectedRef.current;
      if (sel && !tabsAfter.includes(sel)) {
        let na: string | null = null;
        const idx = oldTabs.indexOf(sel);
        if (idx >= 0) {
          for (let i = idx + 1; i < oldTabs.length && na == null; i++) if (tabsAfter.includes(oldTabs[i])) na = oldTabs[i];
          for (let i = idx - 1; i >= 0 && na == null; i--) if (tabsAfter.includes(oldTabs[i])) na = oldTabs[i];
        }
        if (na == null) na = tabsAfter.length ? tabsAfter[0] : firstSelectable(buildTree(filesRef.current));
        void flushSave();
        setOpenTabs(na ? withTab(tabsAfter, na) : tabsAfter);
        setSelectedPath(na);
        setSelectedPaths(na ? new Set([na]) : new Set());
        setAnchorPath(na);
      } else {
        setOpenTabs(tabsAfter);
      }
    },
    [flushSave],
  );

  // Right-click a tab → Close / Close-variants / Rename / Delete menu (App renders it).
  const onTabContext = useCallback((path: string, e: React.MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    setVaultMenuOpen(false);
    setCtxMenu(null);
    setTabCtx({ x: Math.min(e.clientX, window.innerWidth - 196), y: Math.min(e.clientY, window.innerHeight - 308), path });
  }, []);

  // Drag-reorder within the strip → persist the new order (the per-vault
  // localStorage save fires off the openTabs change).
  const onTabReorder = useCallback((from: number, to: number) => {
    setOpenTabs((t) => reorderTabs(t, from, to));
  }, []);

  // A file dragged from the tree onto the strip OPENS it as a tab (no move).
  const onTabDropOpen = useCallback(
    (path: string) => {
      if (filesRef.current.some((f) => f.path === path && !f.is_dir)) onTabSelect(path);
    },
    [onTabSelect],
  );

  const ctxTargetPath = ctxMenu && !ctxMenu.root ? ctxMenu.path ?? null : null;

  const count = selectedPath ? countLabel(docText, selectedPath) : '';
  // Git-bridge §7.2: when the connect input looks like a git URL, the modal swaps
  // the access-key field for Token (https) or an SSH-agent note (ssh), and submit
  // routes to `cloneGit` instead of `cloneRemote`. `gitScheme` is null for an ASP
  // ticket / node id (the ordinary peer path).
  const gitScheme = entry === 'connect' ? gitUrlScheme(ticket) : null;
  // SSH clones need a local ssh-agent, which the browser has no access to.
  const sshOnWeb = gitScheme === 'ssh' && !desktop;
  // Submit is blocked until the modal has what it needs — desktop additionally
  // requires a chosen destination folder; web needs none (it writes to OPFS). An
  // ssh git URL on web can never proceed, so block it (with an inline hint below).
  const entryBlocked = entry === 'connect' ? connecting || !ticket.trim() || (desktop && !connectDest) || sshOnWeb : desktop && !connectDest;

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
            <button onClick={() => { setEntry('connect'); setTicket(''); setAuthKey(''); setToken(''); setDepth(''); setAdvancedOpen(false); setConnectError(null); setConnectDest(null); }} style={{ flex: 1, display: 'flex', alignItems: 'center', justifyContent: 'center', gap: 8, height: 46, padding: '0 14px', border: '1px solid var(--line)', borderRadius: 11, background: 'var(--bg)', color: 'var(--text2)', fontSize: 14, fontWeight: 500, fontFamily: 'inherit', cursor: 'pointer' }}>
              <Icon.ConnectIcon size={15} stroke="currentColor" />
              <span>Connect Vault</span>
            </button>
          </div>

          {desktop && vaultsLoading && saved.length === 0 && (
            <div data-testid="vaults-loading" style={{ marginTop: 26, padding: '14px 15px', border: '1px solid var(--line)', borderRadius: 14, background: 'var(--bg)', color: 'var(--text3)', fontSize: 13 }}>
              <div style={{ display: 'flex', alignItems: 'center', gap: 10 }}>
                <span style={{ width: 15, height: 15, border: '2px solid var(--faint2)', borderTopColor: 'var(--text2)', borderRadius: '50%', display: 'inline-block', animation: 'aspSpin 0.7s linear infinite', flex: 'none' }} />
                <span style={{ flex: 1 }}>{scanProgress ? scanPhaseLabel(scanProgress) : 'Loading your vaults…'}</span>
                {scanProgress && scanProgress.total > 0 && (
                  <span data-testid="scan-count" style={{ fontVariantNumeric: 'tabular-nums', color: 'var(--faint2)', fontSize: 12 }}>
                    {scanProgress.done.toLocaleString()} / {scanProgress.total.toLocaleString()}
                  </span>
                )}
              </div>
              {scanProgress && scanProgress.total > 0 && (
                <div data-testid="scan-bar" style={{ marginTop: 10, height: 5, borderRadius: 3, background: 'var(--line)', overflow: 'hidden' }}>
                  <div style={{ height: '100%', width: `${Math.min(100, Math.round((scanProgress.done / scanProgress.total) * 100))}%`, background: accent, borderRadius: 3, transition: 'width .2s ease' }} />
                </div>
              )}
            </div>
          )}

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
                    <span style={{ fontSize: 11, color: 'var(--faint)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', flex: 'none' }}>{syncSummary}</span>
                    {gitStatus && (
                      <>
                        <span style={{ fontSize: 11, color: 'var(--faint2)', flex: 'none' }}>·</span>
                        <span
                          data-testid="git-chip"
                          title={gitStatus.frozen ? `${gitStatus.remoteUrl} — upstream history was rewritten; rebaseline this vault` : gitStatus.remoteUrl}
                          style={{ display: 'flex', alignItems: 'center', gap: 4, fontSize: 11, color: gitStatus.frozen ? '#c0392b' : 'var(--faint)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', minWidth: 0 }}
                        >
                          <span style={{ overflow: 'hidden', textOverflow: 'ellipsis' }}>
                            git · {gitStatus.atSha ? gitStatus.atSha.slice(0, 7) : '—'}
                            {(gitStatus.ahead > 0 || gitStatus.behind > 0) ? ` ↑${gitStatus.ahead} ↓${gitStatus.behind}` : ''}
                            {gitStatus.frozen ? ' · ⚠ rebaseline' : ''}
                          </span>
                        </span>
                      </>
                    )}
                  </div>
                </div>
                <Icon.CaretDown style={{ flex: 'none', transition: 'transform .15s', transform: vaultMenuOpen ? 'rotate(180deg)' : 'rotate(0deg)' }} />
              </div>

              {vaultMenuOpen && (
                <>
                  <div onClick={() => setVaultMenuOpen(false)} style={{ position: 'fixed', inset: 0, zIndex: 40 }} />
                  <div style={{ position: 'absolute', top: 'calc(100% - 4px)', left: 8, right: 8, zIndex: 41, background: 'var(--bg)', border: '1px solid var(--line)', borderRadius: 12, boxShadow: '0 12px 32px rgba(28,25,23,0.13)', padding: 6, display: 'flex', flexDirection: 'column', gap: 2, maxHeight: 'calc(100vh - 80px)' }}>
                    <div style={{ fontSize: 10.5, fontWeight: 600, letterSpacing: '0.06em', textTransform: 'uppercase', color: 'var(--faint2)', padding: '7px 9px 4px' }}>Switch vault</div>
                    <div className="asp-scroll" data-testid="vault-list" style={{ overflowY: 'auto', minHeight: 0, maxHeight: 'calc(100vh - 320px)', display: 'flex', flexDirection: 'column', gap: 2 }}>
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
                    </div>
                    <div style={{ height: 1, background: 'var(--line)', margin: '4px 6px' }} />
                    <div className="asp-hover-soft" onClick={() => activeMeta && openCustomize(activeMeta)} style={{ display: 'flex', alignItems: 'center', gap: 10, padding: '8px 9px', borderRadius: 8, cursor: 'pointer', color: 'var(--text2)' }}>
                      <Icon.WandIcon style={{ flex: 'none' }} />
                      <span style={{ fontSize: 13.5 }}>Customize this vault…</span>
                    </div>
                    <div className="asp-hover-soft" onClick={() => activeId && void onShareVault(activeId)} style={{ display: 'flex', alignItems: 'center', gap: 10, padding: '8px 9px', borderRadius: 8, cursor: 'pointer', color: 'var(--text2)' }}>
                      <Icon.ShareIcon style={{ flex: 'none' }} />
                      <span style={{ fontSize: 13.5 }}>Share this vault…</span>
                    </div>
                    {gitStatus && (
                      <div className="asp-hover-soft" data-testid="git-pull-item" onClick={() => void onGitPull()} style={{ display: 'flex', alignItems: 'center', gap: 10, padding: '8px 9px', borderRadius: 8, cursor: 'pointer', color: 'var(--text2)' }}>
                        <Icon.ConnectIcon size={15} stroke="var(--text2)" style={{ flex: 'none' }} />
                        <span style={{ fontSize: 13.5 }}>Pull from git</span>
                      </div>
                    )}
                    {/* Push is desktop/CLI-only (the browser can't push — spec non-goal),
                        so this item shows only for a git vault in the desktop shell. */}
                    {canPushGit(gitStatus, desktop) && (
                      <div className="asp-hover-soft" data-testid="git-push-item" onClick={() => void openGitPush()} style={{ display: 'flex', alignItems: 'center', gap: 10, padding: '8px 9px', borderRadius: 8, cursor: 'pointer', color: 'var(--text2)' }}>
                        <Icon.ShareIcon style={{ flex: 'none' }} />
                        <span style={{ fontSize: 13.5 }}>Commit &amp; push to git</span>
                      </div>
                    )}
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
                {/* Tab strip row: the tab strip (left, scrollable) shares one
                    row with the dark-mode/theme button (right). The save-status
                    and word count moved down into the content area below. */}
                <div style={{ height: 48, flex: 'none', display: 'flex', alignItems: 'center', gap: 10, padding: '0 16px 0 0', borderBottom: '1px solid var(--line)' }}>
                  <TabBar
                    tabs={openTabs}
                    active={selectedPath}
                    prettyNames={prefs.prettyNames}
                    accent={accent}
                    accentSoft={accentSoft}
                    onSelect={onTabSelect}
                    onClose={onTabClose}
                    onContext={onTabContext}
                    onRequestRename={(path) => { setTabRenaming(path); setTabRenameValue(basename(path)); }}
                    onReorder={onTabReorder}
                    onDropOpenPath={onTabDropOpen}
                    renamingPath={tabRenaming}
                    renameValue={tabRenameValue}
                    onRenameChange={setTabRenameValue}
                    onRenameKeyDown={(e, path) => { if (e.key === 'Enter') { e.preventDefault(); void commitRename(path, tabRenameValue); } else if (e.key === 'Escape') setTabRenaming(null); }}
                    onRenameCommit={(path) => void commitRename(path, tabRenameValue)}
                  />
                  <div style={{ display: 'flex', alignItems: 'center', gap: 1, flex: 'none' }}>
                    {themeBtn({ display: 'flex', alignItems: 'center', justifyContent: 'center', width: 28, height: 26, flex: 'none', border: 'none', background: 'transparent', color: 'var(--text3)', borderRadius: 7, cursor: 'pointer', padding: 0 })}
                  </div>
                </div>

                {/* Save-status + word count: a subtle, fixed bar pinned to the
                    top of the content area (below the tab strip, above the
                    editor scroll region). It does not scroll with the document.
                    Full-width of the pane, with the cluster hugging the right
                    edge so it sits near the editor pane's right chrome. */}
                <div data-testid="content-status" style={{ flex: 'none', display: 'flex', alignItems: 'center', justifyContent: 'flex-end', gap: 8, padding: '7px 18px', borderBottom: '1px solid var(--line)' }}>
                  <span style={{ width: 6, height: 6, borderRadius: '50%', flex: 'none', background: saving ? '#d9a93d' : '#3fa45a', transition: 'background .2s' }} />
                  <span style={{ fontSize: 11.5, color: 'var(--faint)', whiteSpace: 'nowrap' }}>{saving ? 'Saving…' : 'Saved'}</span>
                  <span style={{ width: 1, height: 11, background: 'var(--line)', flex: 'none', margin: '0 2px' }} />
                  <span style={{ fontSize: 11.5, color: 'var(--faint2)', fontVariantNumeric: 'tabular-nums', whiteSpace: 'nowrap' }}>{count}</span>
                </div>

                {timeTravel && (
                  <div data-testid="time-travel-banner" style={{ flex: 'none', display: 'flex', alignItems: 'center', gap: 12, padding: '9px 18px', background: accentSoft, borderBottom: `1px solid ${accent}33` }}>
                    <Icon.ClockIcon stroke={accent} style={{ flex: 'none' }} />
                    <div style={{ flex: 1, minWidth: 0, fontSize: 12.5, color: 'var(--text2)' }}>
                      This vault as it was on <b style={{ fontWeight: 600, color: 'var(--text)' }}>{new Date(playT).toLocaleString()}</b> · start typing to branch from here
                    </div>
                    <button onClick={() => void onRestoreHere()} title="Bring this version forward onto the current branch" style={{ fontFamily: 'inherit', fontSize: 12, fontWeight: 500, color: 'var(--text2)', background: 'var(--bg)', border: '1px solid var(--line)', borderRadius: 7, padding: '6px 12px', cursor: 'pointer', flex: 'none' }}>Restore onto {lanes0Name(graph, currentBranch)}</button>
                    <button onClick={onNow} style={{ fontFamily: 'inherit', fontSize: 12, fontWeight: 500, color: 'var(--bg)', background: accent, border: 'none', borderRadius: 7, padding: '6px 12px', cursor: 'pointer', flex: 'none' }}>Return to now</button>
                  </div>
                )}
                {branchBanner && !timeTravel && (
                  <div data-testid="branch-created-banner" style={{ flex: 'none', display: 'flex', alignItems: 'center', gap: 12, padding: '9px 18px', background: accentSoft, borderBottom: `1px solid ${accent}33` }}>
                    <Icon.ClockIcon stroke={accent} style={{ flex: 'none' }} />
                    <div style={{ flex: 1, minWidth: 0, fontSize: 12.5, color: 'var(--text2)' }}>
                      You're now on a new branch <b style={{ fontWeight: 600, color: 'var(--text)' }}>{branchBanner.name}</b> — forked from {new Date(branchBanner.from).toLocaleString()}. Edits here won't touch the original.
                    </div>
                    <button onClick={() => setBranchBanner(null)} style={{ fontFamily: 'inherit', fontSize: 12, fontWeight: 500, color: 'var(--text2)', background: 'var(--bg)', border: '1px solid var(--line)', borderRadius: 7, padding: '6px 12px', cursor: 'pointer', flex: 'none' }}>Got it</button>
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
                      contentMissing={paint.contentMissing}
                      loading={paint.loading}
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
          graph={graph}
          currentBranch={currentBranch}
          onCheckoutBranch={(b) => void onCheckoutBranch(b)}
          onCreateTag={(name, tsMs) => void onCreateTag(name, tsMs)}
          onDeleteTag={(t) => void onDeleteTag(t)}
          loadDiff={loadDiff}
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
                  <div className="asp-hover-soft" onClick={() => void createFile(ctxTargetDir(ctxMenu))} style={{ display: 'flex', alignItems: 'center', padding: '7px 11px', borderRadius: 7, cursor: 'pointer', fontSize: 13, color: 'var(--text)' }}>
                    <span>New file</span>
                  </div>
                  <div className="asp-hover-soft" onClick={() => void createFolder(ctxTargetDir(ctxMenu))} style={{ display: 'flex', alignItems: 'center', padding: '7px 11px', borderRadius: 7, cursor: 'pointer', fontSize: 13, color: 'var(--text)' }}>
                    <span>New folder</span>
                  </div>
                </>
              ) : (
                <>
                  <div className="asp-hover-soft" onClick={() => { setRenaming(ctxMenu.path!); setRenameValue(ctxMenu.name!); setCtxMenu(null); }} style={{ display: 'flex', alignItems: 'center', padding: '7px 11px', borderRadius: 7, cursor: 'pointer', fontSize: 13, color: 'var(--text)' }}>
                    <span>Rename</span>
                  </div>
                  <div className="asp-hover-soft" onClick={() => deleteNode(ctxMenu.path!, !!ctxMenu.isDir)} style={{ display: 'flex', alignItems: 'center', padding: '7px 11px', borderRadius: 7, cursor: 'pointer', fontSize: 13, color: 'var(--text)' }}>
                    <span>Delete</span>
                  </div>
                </>
              )}
            </div>
          </>
        )}

        {/* tab context menu — Close / Close Others / Close to the Left / Close to the Right / Close All / Rename / Delete */}
        {tabCtx && (
          <>
            <div onClick={() => setTabCtx(null)} onContextMenu={(e) => { e.preventDefault(); setTabCtx(null); }} style={{ position: 'fixed', inset: 0, zIndex: 60 }} />
            <div style={{ position: 'fixed', left: tabCtx.x, top: tabCtx.y, zIndex: 61, width: 188, background: 'var(--bg)', border: '1px solid var(--line)', borderRadius: 10, boxShadow: '0 12px 32px rgba(28,25,23,0.16)', padding: 5 }}>
              <div className="asp-hover-soft" onClick={() => { onTabClose(tabCtx.path); setTabCtx(null); }} style={{ display: 'flex', alignItems: 'center', padding: '7px 11px', borderRadius: 7, cursor: 'pointer', fontSize: 13, color: 'var(--text)' }}>
                <span>Close</span>
              </div>
              <div className="asp-hover-soft" onClick={() => { applyTabClose(closeOthers(openTabs, tabCtx.path)); setTabCtx(null); }} style={{ display: 'flex', alignItems: 'center', padding: '7px 11px', borderRadius: 7, cursor: 'pointer', fontSize: 13, color: 'var(--text)' }}>
                <span>Close Others</span>
              </div>
              <div className="asp-hover-soft" onClick={() => { applyTabClose(closeToLeft(openTabs, tabCtx.path)); setTabCtx(null); }} style={{ display: 'flex', alignItems: 'center', padding: '7px 11px', borderRadius: 7, cursor: 'pointer', fontSize: 13, color: 'var(--text)' }}>
                <span>Close to the Left</span>
              </div>
              <div className="asp-hover-soft" onClick={() => { applyTabClose(closeToRight(openTabs, tabCtx.path)); setTabCtx(null); }} style={{ display: 'flex', alignItems: 'center', padding: '7px 11px', borderRadius: 7, cursor: 'pointer', fontSize: 13, color: 'var(--text)' }}>
                <span>Close to the Right</span>
              </div>
              <div className="asp-hover-soft" onClick={() => { applyTabClose(closeAll()); setTabCtx(null); }} style={{ display: 'flex', alignItems: 'center', padding: '7px 11px', borderRadius: 7, cursor: 'pointer', fontSize: 13, color: 'var(--text)' }}>
                <span>Close All</span>
              </div>
              <div style={{ height: 1, background: 'var(--line)', margin: '4px 6px' }} />
              <div className="asp-hover-soft" onClick={() => { setTabRenaming(tabCtx.path); setTabRenameValue(basename(tabCtx.path)); setTabCtx(null); }} style={{ display: 'flex', alignItems: 'center', padding: '7px 11px', borderRadius: 7, cursor: 'pointer', fontSize: 13, color: 'var(--text)' }}>
                <span>Rename</span>
              </div>
              <div className="asp-hover-soft" onClick={() => { deleteNode(tabCtx.path, false); setTabCtx(null); }} style={{ display: 'flex', alignItems: 'center', padding: '7px 11px', borderRadius: 7, cursor: 'pointer', fontSize: 13, color: 'var(--text)' }}>
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

      {/* open/add progress overlay — non-dismissable; shown only when an open
          runs long enough to look frozen (see withOpening). Above every modal. */}
      {opening && (
        <div data-testid="opening-overlay" style={{ position: 'fixed', inset: 0, zIndex: 95, background: 'var(--overlay)', backdropFilter: 'blur(2px)', display: 'flex', alignItems: 'center', justifyContent: 'center' }}>
          <div style={{ display: 'flex', flexDirection: 'column', alignItems: 'center', gap: 14, background: 'var(--bg)', borderRadius: 16, boxShadow: '0 24px 60px rgba(28,25,23,0.28)', padding: '26px 34px' }}>
            <span style={{ width: 26, height: 26, border: '3px solid var(--faint2)', borderTopColor: 'var(--text)', borderRadius: '50%', display: 'inline-block', animation: 'aspSpin 0.7s linear infinite' }} />
            <div style={{ fontSize: 13.5, fontWeight: 500, color: 'var(--text2)' }}>{opening}</div>
          </div>
        </div>
      )}

      {/* vault-row context menu (connect screen) */}
      {vaultCtx && (
        <>
          <div onClick={() => setVaultCtx(null)} onContextMenu={(e) => { e.preventDefault(); setVaultCtx(null); }} style={{ position: 'fixed', inset: 0, zIndex: 62 }} />
          <div style={{ position: 'fixed', left: vaultCtx.x, top: vaultCtx.y, zIndex: 63, width: 176, background: 'var(--bg)', border: '1px solid var(--line)', borderRadius: 10, boxShadow: '0 10px 28px rgba(28,25,23,0.15)', padding: 4 }}>
            <div className="asp-hover-soft" onClick={() => { const v = vaults.find((x) => x.id === vaultCtx.id); setVaultCtx(null); if (v) openCustomize(v); }} style={{ display: 'flex', alignItems: 'center', padding: '7px 11px', borderRadius: 7, cursor: 'pointer', fontSize: 13, color: 'var(--text2)' }}>
              <span>Customize…</span>
            </div>
            <div className="asp-hover-danger" onClick={() => { const v = vaultMetas.find((x) => x.id === vaultCtx.id); setVaultCtx(null); if (v) setRemoveVaultState({ id: v.id, name: v.displayName, path: v.path, trash: false }); }} style={{ display: 'flex', alignItems: 'center', padding: '7px 11px', borderRadius: 7, cursor: 'pointer', fontSize: 13, color: '#c0392b' }}>
              <span>Remove vault…</span>
            </div>
          </div>
        </>
      )}

      {/* entry modal — New vault / Connect a vault */}
      {entry && (
        <>
          <div onClick={() => { if (!connecting) setEntry(null); }} style={{ position: 'fixed', inset: 0, zIndex: 58, background: 'var(--overlay)', backdropFilter: 'blur(2px)' }} />
          <div
            onKeyDown={(e) => {
              if (e.key === 'Escape') { if (!connecting) setEntry(null); return; }
              if (e.key === 'Enter') {
                // Plain Enter inside the multi-line invite-code box inserts a
                // newline; Cmd/Ctrl+Enter submits there. Single-line inputs submit
                // on plain Enter. Always respect the disabled/blocked state.
                const ta = (e.target as HTMLElement).tagName === 'TEXTAREA';
                if (ta && !(e.metaKey || e.ctrlKey)) return;
                if (entryBlocked) return;
                e.preventDefault();
                void onEntrySubmit();
              }
            }}
            style={{ position: 'fixed', zIndex: 59, top: '50%', left: '50%', transform: 'translate(-50%,-50%)', width: 'min(424px,92vw)', background: 'var(--bg)', borderRadius: 16, boxShadow: '0 24px 60px rgba(28,25,23,0.28)', padding: 20, display: 'flex', flexDirection: 'column', gap: 15 }}
          >
            <div>
              <div style={{ fontSize: 16, fontWeight: 600, letterSpacing: '-0.01em' }}>{entry === 'connect' ? 'Connect a vault' : 'New vault'}</div>
              <div style={{ fontSize: 12.5, color: 'var(--text3)', marginTop: 3 }}>{entry === 'connect' ? 'Paste a code someone shared with you.' : desktop ? 'Name it and choose a folder — everything syncs automatically.' : 'Name it and start writing — it saves in this browser and syncs automatically.'}</div>
            </div>
            {entry === 'new' && (
              <label style={{ display: 'flex', flexDirection: 'column', gap: 7 }}>
                <span style={{ fontSize: 11, fontWeight: 600, letterSpacing: '0.05em', textTransform: 'uppercase', color: 'var(--faint2)' }}>Name</span>
                <input autoFocus value={newVaultName} onChange={(e) => setNewVaultName(e.target.value)} spellCheck={false} placeholder="My vault" style={{ fontFamily: 'inherit', fontSize: 14, color: 'var(--text)', background: 'var(--bg-input)', border: '1px solid var(--line)', borderRadius: 10, padding: '10px 12px', outline: 'none', width: '100%', boxSizing: 'border-box' }} />
              </label>
            )}
            {entry === 'connect' && !cloneProg && (
              <>
                <label style={{ display: 'flex', flexDirection: 'column', gap: 7 }}>
                  <span style={{ fontSize: 11, fontWeight: 600, letterSpacing: '0.05em', textTransform: 'uppercase', color: 'var(--faint2)' }}>Invite code <span style={{ textTransform: 'none', letterSpacing: 0, fontWeight: 400, color: 'var(--faint)' }}>or git URL</span></span>
                  <textarea autoFocus value={ticket} onChange={(e) => setTicket(e.target.value)} rows={2} spellCheck={false} placeholder="Paste an invite code, or a git URL (https://… or git@…)" style={{ fontFamily: "'JetBrains Mono', monospace", fontSize: 12.5, lineHeight: 1.5, color: 'var(--text)', background: 'var(--bg-input)', border: '1px solid var(--line)', borderRadius: 10, padding: '11px 13px', resize: 'none', outline: 'none', width: '100%', boxSizing: 'border-box' }} />
                </label>
                {gitScheme === 'https' ? (
                  <label data-testid="git-token-field" style={{ display: 'flex', flexDirection: 'column', gap: 7 }}>
                    <span style={{ fontSize: 11, fontWeight: 600, letterSpacing: '0.05em', textTransform: 'uppercase', color: 'var(--faint2)' }}>Token <span style={{ textTransform: 'none', letterSpacing: 0, fontWeight: 400, color: 'var(--faint)' }}>— for a private repo</span></span>
                    <input value={token} onChange={(e) => setToken(e.target.value)} type="password" spellCheck={false} placeholder="Personal access token (leave blank if public)" style={{ fontFamily: "'JetBrains Mono', monospace", fontSize: 12.5, color: 'var(--text)', background: 'var(--bg-input)', border: '1px solid var(--line)', borderRadius: 10, padding: '11px 13px', outline: 'none', width: '100%', boxSizing: 'border-box' }} />
                    <span style={{ fontSize: 11.5, lineHeight: 1.5, color: 'var(--faint)' }}>
                      Use a fine-grained, single-repo access token — that limits what a leaked token can reach.{!desktop && ' It’s saved in this browser’s storage, so a stolen browser profile could expose it.'}
                    </span>
                  </label>
                ) : gitScheme === 'ssh' ? (
                  <div data-testid="git-ssh-note" style={{ fontSize: 12.5, lineHeight: 1.55, color: sshOnWeb ? '#c0392b' : 'var(--text2)', background: sshOnWeb ? '#c0392b12' : 'var(--bg-input)', border: `1px solid ${sshOnWeb ? '#c0392b40' : 'var(--line)'}`, borderRadius: 10, padding: '11px 13px' }}>
                    {sshOnWeb
                      ? 'SSH clone isn’t supported in the browser — use an https:// URL or the desktop app.'
                      : 'This clones over SSH using your local SSH agent — no token needed.'}
                  </div>
                ) : (
                  <label style={{ display: 'flex', flexDirection: 'column', gap: 7 }}>
                    <span style={{ fontSize: 11, fontWeight: 600, letterSpacing: '0.05em', textTransform: 'uppercase', color: 'var(--faint2)' }}>Access key <span style={{ textTransform: 'none', letterSpacing: 0, fontWeight: 400, color: 'var(--faint)' }}>— if required</span></span>
                    <input value={authKey} onChange={(e) => setAuthKey(e.target.value)} type="password" spellCheck={false} placeholder="Leave blank if you weren't given one" style={{ fontFamily: "'JetBrains Mono', monospace", fontSize: 12.5, color: 'var(--text)', background: 'var(--bg-input)', border: '1px solid var(--line)', borderRadius: 10, padding: '11px 13px', outline: 'none', width: '100%', boxSizing: 'border-box' }} />
                  </label>
                )}
                {gitScheme && !sshOnWeb && (
                  <div style={{ display: 'flex', flexDirection: 'column', gap: 8 }}>
                    <button type="button" onClick={() => setAdvancedOpen((v) => !v)} style={{ alignSelf: 'flex-start', display: 'flex', alignItems: 'center', gap: 6, background: 'none', border: 'none', padding: 0, cursor: 'pointer', fontFamily: 'inherit', fontSize: 12, fontWeight: 500, color: 'var(--faint)' }}>
                      <Icon.CaretDown style={{ flex: 'none', transition: 'transform .15s', transform: advancedOpen ? 'rotate(180deg)' : 'rotate(-90deg)' }} />
                      <span>Advanced</span>
                    </button>
                    {advancedOpen && (
                      <div style={{ display: 'flex', flexDirection: 'column', gap: 13 }}>
                        <label style={{ display: 'flex', flexDirection: 'column', gap: 7 }}>
                          <span style={{ fontSize: 11, fontWeight: 600, letterSpacing: '0.05em', textTransform: 'uppercase', color: 'var(--faint2)' }}>Import last N commits <span style={{ textTransform: 'none', letterSpacing: 0, fontWeight: 400, color: 'var(--faint)' }}>— blank = full history</span></span>
                          <input value={depth} onChange={(e) => setDepth(e.target.value.replace(/[^0-9]/g, ''))} inputMode="numeric" spellCheck={false} placeholder="e.g. 50" style={{ fontFamily: "'JetBrains Mono', monospace", fontSize: 12.5, color: 'var(--text)', background: 'var(--bg-input)', border: '1px solid var(--line)', borderRadius: 10, padding: '11px 13px', outline: 'none', width: '100%', boxSizing: 'border-box' }} />
                        </label>
                        <label data-testid="git-all-branches-field" style={{ display: 'flex', gap: 9, alignItems: 'flex-start', cursor: 'pointer' }}>
                          <input type="checkbox" checked={allBranches} onChange={(e) => setAllBranches(e.target.checked)} style={{ marginTop: 2, flex: 'none', accentColor: accent, cursor: 'pointer' }} />
                          <span style={{ display: 'flex', flexDirection: 'column', gap: 3 }}>
                            <span style={{ fontSize: 13, color: 'var(--text)' }}>Also import open branches</span>
                            <span style={{ fontSize: 11.5, lineHeight: 1.5, color: 'var(--faint)' }}>
                              Best for the first clone of a repo. Other devices should connect to this vault with an invite code instead of re-cloning from git.
                            </span>
                          </span>
                        </label>
                      </div>
                    )}
                  </div>
                )}
              </>
            )}
            {cloneProg && (() => {
              const { done, total, phase } = cloneProg;
              // ONE monotonic weighted bar across the whole clone. Each git phase owns a
              // fixed slice of [0,1]; the fill never resets between phases because the
              // slice floor is the previous phase's ceiling. An ASP peer clone
              // ('receiving') owns the whole bar. total>0 → determinate within the
              // slice; total==0 → the fill sits at the slice floor + a shimmer plays
              // within the slice while a live count ticks.
              const seg: Record<ClonePhase, [number, number]> = {
                fetching: [0, 0.2],
                scanning: [0.2, 0.28],
                replaying: [0.28, 0.55],
                importing: [0.55, 0.66],
                saving: [0.66, 0.82],
                materialize: [0.82, 1],
                receiving: [0, 1],
              };
              const [lo, hi] = seg[phase] ?? [0, 1];
              const determinate = total > 0;
              const frac = determinate ? Math.min(1, done / total) : 0;
              const pct = Math.round((lo + (hi - lo) * frac) * 100);
              // Git phases get their own copy; the ASP peer path keeps its wording.
              const title =
                phase === 'saving' ? 'Saving to this device…'
                  : phase === 'materialize' ? 'Writing files to this device…'
                    : phase === 'fetching' ? 'Fetching from git…'
                      : phase === 'scanning' ? 'Scanning the pack…'
                        : phase === 'replaying' ? 'Replaying history…'
                          : phase === 'importing' ? 'Importing commits…'
                            : 'Receiving notes…';
              const sub =
                phase === 'saving' ? 'Almost done — writing everything to local storage.'
                  : phase === 'materialize' ? 'Saving the working tree locally.'
                    : phase === 'fetching' ? 'Downloading the repository over a secure connection.'
                      : phase === 'scanning' ? 'Indexing the objects in the downloaded pack.'
                        : phase === 'replaying' ? 'Rebuilding the note history from the repository.'
                          : phase === 'importing' ? 'Turning commits into synced note history.'
                            : 'Pulling the vault over a direct connection. Hang tight.';
              // Right-aligned count: determinate → done/total; fetching-while-coarse →
              // live MB (bytes); other coarse phases → a running count if we have one.
              const count = determinate
                ? `${done.toLocaleString()} / ${total.toLocaleString()}`
                : phase === 'fetching'
                  ? (done > 0 ? `${(done / 1048576).toFixed(1)} MB` : '')
                  : done > 0 ? done.toLocaleString() : '';
              return (
                <div data-testid="clone-progress" data-phase={phase} data-done={done} data-total={total} data-pct={pct} style={{ display: 'flex', flexDirection: 'column', gap: 9, background: 'var(--bg-input)', border: '1px solid var(--line)', borderRadius: 10, padding: '14px 15px' }}>
                  <div style={{ display: 'flex', alignItems: 'baseline', justifyContent: 'space-between', gap: 10 }}>
                    <span style={{ fontSize: 13, fontWeight: 500, color: 'var(--text)' }}>{title}</span>
                    <span style={{ fontFamily: "'JetBrains Mono', monospace", fontSize: 11.5, color: 'var(--faint)' }}>{count}</span>
                  </div>
                  <div style={{ position: 'relative', height: 6, borderRadius: 3, background: 'var(--faint2)', overflow: 'hidden' }}>
                    {/* Always render the filled bar to pct (slice floor when coarse), so it
                        only ever grows. */}
                    <div style={{ height: '100%', borderRadius: 3, background: accent, width: `${pct}%`, transition: 'width 0.25s ease' }} />
                    {/* Coarse phase: overlay a shimmer confined to the current slice. */}
                    {!determinate && (
                      <div style={{ position: 'absolute', top: 0, bottom: 0, left: `${lo * 100}%`, width: `${(hi - lo) * 100}%`, borderRadius: 3, overflow: 'hidden' }}>
                        <div style={{ position: 'absolute', top: 0, bottom: 0, width: '40%', borderRadius: 3, background: accent, animation: 'aspIndet 1.1s ease-in-out infinite' }} />
                      </div>
                    )}
                  </div>
                  <span style={{ fontSize: 11.5, color: 'var(--faint)' }}>{sub}</span>
                </div>
              );
            })()}
            {desktop && !cloneProg && (
              <div style={{ display: 'flex', flexDirection: 'column', gap: 7 }}>
                <span style={{ fontSize: 11, fontWeight: 600, letterSpacing: '0.05em', textTransform: 'uppercase', color: 'var(--faint2)' }}>{entry === 'connect' ? 'Save to' : 'Location'}</span>
                <div onClick={() => void onChooseDest()} style={{ display: 'flex', alignItems: 'center', gap: 9, background: 'var(--bg-input)', border: '1px solid var(--line)', borderRadius: 10, padding: '10px 13px', cursor: 'pointer' }}>
                  <Icon.FolderIcon size={15} stroke="var(--faint)" style={{ flex: 'none' }} />
                  <span style={{ fontFamily: "'JetBrains Mono', monospace", fontSize: 12, color: connectDest ? 'var(--text)' : 'var(--faint)', flex: 1, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{connectDest || 'Choose a folder…'}</span>
                  <span style={{ fontSize: 12, color: 'var(--faint)' }}>Choose…</span>
                </div>
              </div>
            )}
            {connectError && (
              <div data-testid="connect-error" style={{ fontSize: 12.5, color: '#c0392b', background: '#c0392b12', border: '1px solid #c0392b40', borderRadius: 9, padding: '9px 12px' }}>
                {connectError}
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
          <div
            onKeyDown={(e) => { if (e.key === 'Escape') { e.preventDefault(); setShare(null); } }}
            style={{ position: 'fixed', zIndex: 71, top: '50%', left: '50%', transform: 'translate(-50%,-50%)', width: 'min(420px,92vw)', background: 'var(--bg)', borderRadius: 16, boxShadow: '0 24px 60px rgba(28,25,23,0.28)', padding: 20, display: 'flex', flexDirection: 'column', gap: 14 }}
          >
            <div>
              <div style={{ fontSize: 16, fontWeight: 600, letterSpacing: '-0.01em' }}>Share this vault</div>
              <div style={{ fontSize: 13, color: 'var(--text3)', marginTop: 3 }}>{share.unavailable ? 'Sharing needs the desktop app.' : 'Anyone you give this code to can connect and sync.'}</div>
            </div>
            {share.unavailable ? (
              <div style={{ fontSize: 12.5, lineHeight: 1.6, color: 'var(--text2)', background: 'var(--bg-input)', border: '1px solid var(--line)', borderRadius: 10, padding: '13px 14px' }}>
                Sharing isn’t available for browser vaults — a vault stored in your browser can’t accept connections from other devices.
                <div style={{ fontSize: 12, color: 'var(--faint)', marginTop: 6 }}>Open this vault in the desktop app to share it.</div>
              </div>
            ) : (
              <>
                <div style={{ display: 'flex', alignItems: 'stretch', gap: 8 }}>
                  <div style={{ flex: 1, minWidth: 0, fontFamily: "'JetBrains Mono', monospace", fontSize: 12, lineHeight: 1.5, color: 'var(--text2)', background: 'var(--bg-input)', border: '1px solid var(--line)', borderRadius: 10, padding: '11px 13px', wordBreak: 'break-all', maxHeight: 64, overflow: 'hidden' }}>{share.code || 'Generating…'}</div>
                  <button autoFocus onClick={() => void onCopyCode()} style={{ flex: 'none', alignSelf: 'stretch', display: 'flex', alignItems: 'center', fontFamily: 'inherit', fontSize: 12.5, fontWeight: 500, color: share.copied ? '#3a9357' : 'var(--text2)', background: 'var(--bg)', border: '1px solid var(--line)', borderRadius: 10, padding: '0 14px', cursor: 'pointer' }}>{share.copied ? 'Copied' : 'Copy'}</button>
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
                <div onClick={() => void onToggleLocalRelay()} style={{ display: 'flex', alignItems: 'center', gap: 11, cursor: 'pointer', padding: 2 }}>
                  <span style={{ width: 34, height: 20, borderRadius: 12, flex: 'none', background: localRelayOn ? accent : 'var(--faint2)', position: 'relative', transition: 'background .15s' }}>
                    <span style={{ position: 'absolute', top: 2, left: localRelayOn ? 16 : 2, width: 16, height: 16, borderRadius: '50%', background: 'var(--bg)', transition: 'left .15s', boxShadow: '0 1px 2px rgba(0,0,0,0.2)' }} />
                  </span>
                  <div style={{ flex: 1 }}>
                    <div style={{ fontSize: 13.5, fontWeight: 500, color: 'var(--text)' }}>Allow connections directly for faster syncing</div>
                    <div style={{ fontSize: 12, color: 'var(--faint)' }}>Routes peers on this device/network through your machine instead of a public relay.</div>
                  </div>
                </div>
              </>
            )}
            <button autoFocus={share.unavailable} onClick={() => setShare(null)} style={{ alignSelf: 'flex-end', fontFamily: 'inherit', fontSize: 13, fontWeight: 500, color: 'var(--bg)', background: 'var(--text)', border: 'none', borderRadius: 9, padding: '8px 18px', cursor: 'pointer' }}>Done</button>
          </div>
        </>
      )}

      {/* remove modal */}
      {removeVaultState && (
        <>
          <div onClick={() => setRemoveVaultState(null)} style={{ position: 'fixed', inset: 0, zIndex: 72, background: 'var(--overlay)', backdropFilter: 'blur(2px)' }} />
          <div
            onKeyDown={(e) => { if (e.key === 'Escape') { e.preventDefault(); setRemoveVaultState(null); } }}
            style={{ position: 'fixed', zIndex: 73, top: '50%', left: '50%', transform: 'translate(-50%,-50%)', width: 'min(412px,92vw)', background: 'var(--bg)', borderRadius: 16, boxShadow: '0 24px 60px rgba(28,25,23,0.28)', padding: 20, display: 'flex', flexDirection: 'column', gap: 14 }}
          >
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
              <button autoFocus onClick={() => setRemoveVaultState(null)} style={{ fontFamily: 'inherit', fontSize: 13, fontWeight: 500, color: 'var(--text2)', background: 'var(--bg)', border: '1px solid var(--line)', borderRadius: 9, padding: '8px 16px', cursor: 'pointer' }}>Cancel</button>
              <button onClick={() => void confirmRemove()} style={{ fontFamily: 'inherit', fontSize: 13, fontWeight: 500, color: 'var(--bg)', background: '#c0392b', border: 'none', borderRadius: 9, padding: '8px 16px', cursor: 'pointer' }}>{removeVaultState.trash ? 'Remove & Trash folder' : 'Remove from asp'}</button>
            </div>
          </div>
        </>
      )}

      {/* Commit & push to git (git-bridge §7.2) — desktop/CLI-only. Shows the pending
          diff + an editable commit message; confirm authors a plan and pushes. */}
      {gitPushState && (
        <>
          <div onClick={() => setGitPushState(null)} style={{ position: 'fixed', inset: 0, zIndex: 72, background: 'var(--overlay)', backdropFilter: 'blur(2px)' }} />
          <div
            data-testid="git-push-dialog"
            onKeyDown={(e) => { if (e.key === 'Escape') { e.preventDefault(); setGitPushState(null); } }}
            style={{ position: 'fixed', zIndex: 73, top: '50%', left: '50%', transform: 'translate(-50%,-50%)', width: 'min(560px,94vw)', maxHeight: '86vh', background: 'var(--bg)', borderRadius: 16, boxShadow: '0 24px 60px rgba(28,25,23,0.28)', padding: 20, display: 'flex', flexDirection: 'column', gap: 12 }}
          >
            <div>
              <div style={{ fontSize: 16, fontWeight: 600, letterSpacing: '-0.01em' }}>Commit &amp; push to git</div>
              <div style={{ fontSize: 13, color: 'var(--text3)', marginTop: 4, lineHeight: 1.5 }}>
                {gitPushState.loading
                  ? 'Checking for pending changes…'
                  : gitPushState.done
                    ? `Pushed ${gitPushState.done.slice(0, 7)} upstream.`
                    : (gitPushState.diff?.filesChanged ?? 0) === 0
                      ? 'Nothing to commit — your vault matches the last push.'
                      : `${gitPushState.diff?.filesChanged} file(s) changed — review and push.`}
              </div>
            </div>

            {!gitPushState.loading && !gitPushState.done && (
              <>
                <textarea
                  data-testid="git-push-message"
                  value={gitPushState.message}
                  onChange={(e) => { const v = e.target.value; setGitPushState((s) => (s ? { ...s, message: v } : s)); }}
                  placeholder="Commit message"
                  rows={2}
                  style={{ fontFamily: 'inherit', fontSize: 13, color: 'var(--text)', background: 'var(--bg-input)', border: '1px solid var(--line)', borderRadius: 10, padding: '9px 12px', resize: 'vertical', outline: 'none' }}
                />
                {(gitPushState.diff?.unified || gitPushState.diff?.paths.length) ? (
                  <pre
                    data-testid="git-push-diff"
                    className="asp-scroll"
                    style={{ margin: 0, fontFamily: "'JetBrains Mono', monospace", fontSize: 11.5, lineHeight: 1.5, color: 'var(--text2)', background: 'var(--bg-input)', border: '1px solid var(--line)', borderRadius: 10, padding: '10px 12px', overflow: 'auto', maxHeight: '38vh', whiteSpace: 'pre' }}
                  >{gitPushState.diff?.unified || (gitPushState.diff?.paths ?? []).join('\n')}</pre>
                ) : null}
              </>
            )}

            {gitPushState.error && (
              <div data-testid="git-push-error" style={{ fontSize: 12.5, color: '#c0392b', lineHeight: 1.5 }}>{gitPushState.error}</div>
            )}

            <div style={{ display: 'flex', justifyContent: 'flex-end', gap: 8, marginTop: 2 }}>
              <button onClick={() => setGitPushState(null)} style={{ fontFamily: 'inherit', fontSize: 13, fontWeight: 500, color: 'var(--text2)', background: 'var(--bg)', border: '1px solid var(--line)', borderRadius: 9, padding: '8px 16px', cursor: 'pointer' }}>{gitPushState.done ? 'Close' : 'Cancel'}</button>
              {!gitPushState.done && (
                <button
                  data-testid="git-push-confirm"
                  disabled={gitPushState.loading || gitPushState.pushing || (gitPushState.diff?.filesChanged ?? 0) === 0}
                  title={(gitPushState.diff?.filesChanged ?? 0) === 0 ? 'Nothing to commit' : undefined}
                  onClick={() => void confirmGitPush()}
                  style={{ fontFamily: 'inherit', fontSize: 13, fontWeight: 500, color: 'var(--bg)', background: accent, border: 'none', borderRadius: 9, padding: '8px 16px', cursor: (gitPushState.loading || gitPushState.pushing || (gitPushState.diff?.filesChanged ?? 0) === 0) ? 'not-allowed' : 'pointer', opacity: (gitPushState.loading || gitPushState.pushing || (gitPushState.diff?.filesChanged ?? 0) === 0) ? 0.5 : 1 }}
                >{gitPushState.pushing ? 'Pushing…' : 'Commit & push'}</button>
              )}
            </div>
          </div>
        </>
      )}

      {/* delete-confirm modal — files/folders are recoverable from History, so the
          message is honest (not "can't be undone") and the button is non-red. */}
      {deleteConfirm && (
        <>
          <div data-testid="delete-confirm-overlay" onClick={() => setDeleteConfirm(null)} style={{ position: 'fixed', inset: 0, zIndex: 72, background: 'var(--overlay)', backdropFilter: 'blur(2px)' }} />
          <div
            data-testid="delete-confirm"
            onKeyDown={(e) => {
              if (e.key === 'Escape') { e.preventDefault(); setDeleteConfirm(null); }
              else if (e.key === 'Enter') { e.preventDefault(); confirmDelete(); }
            }}
            style={{ position: 'fixed', zIndex: 73, top: '50%', left: '50%', transform: 'translate(-50%,-50%)', width: 'min(412px,92vw)', background: 'var(--bg)', borderRadius: 16, boxShadow: '0 24px 60px rgba(28,25,23,0.28)', padding: 20, display: 'flex', flexDirection: 'column', gap: 14 }}
          >
            <div>
              <div style={{ fontSize: 16, fontWeight: 600, letterSpacing: '-0.01em' }}>{deleteConfirm.count === 1 ? `Delete “${deleteConfirm.label}”?` : `Delete ${deleteConfirm.count} items?`}</div>
              <div style={{ fontSize: 13, color: 'var(--text3)', marginTop: 4, lineHeight: 1.5 }}>{deleteConfirm.count === 1 ? 'It’s removed from the vault — you can bring it back from the History timeline.' : 'They’re removed from the vault — you can bring them back from the History timeline.'}</div>
            </div>
            <div style={{ display: 'flex', justifyContent: 'flex-end', gap: 8, marginTop: 2 }}>
              <button onClick={() => setDeleteConfirm(null)} style={{ fontFamily: 'inherit', fontSize: 13, fontWeight: 500, color: 'var(--text2)', background: 'var(--bg)', border: '1px solid var(--line)', borderRadius: 9, padding: '8px 16px', cursor: 'pointer' }}>Cancel</button>
              <button data-testid="confirm-delete" autoFocus onClick={() => confirmDelete()} style={{ fontFamily: 'inherit', fontSize: 13, fontWeight: 500, color: 'var(--bg)', background: 'var(--text)', border: 'none', borderRadius: 9, padding: '8px 16px', cursor: 'pointer' }}>Delete</button>
            </div>
          </div>
        </>
      )}
    </>
  );
}
