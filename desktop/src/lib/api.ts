// The backend surface, abstracted over platform. On desktop it's a thin
// pass-through to the Tauri command layer (→ asp-desktop-engine → asp-core). On
// web it's the same asp-core engine compiled to wasm, persisted to OPFS (see
// webApi.ts). No protocol logic lives in the app either way.
import { invoke } from '@tauri-apps/api/core';
import { isDesktop } from './platform';

export interface VaultInfo {
  id: string;
  path: string;
  vault_id: string;
  enabled: boolean;
  // The iroh connection ticket this folder is listening on (share to pair), or null.
  listening_ticket: string | null;
}
export interface VaultStatus {
  id: string;
  vault_id: string;
  rows: number;
  files: number;
  head: string;
  listening_ticket: string | null;
  peers: string[];
  // Wall-clock unix SECONDS of the most recent log row, or null for an empty vault.
  last_ts: number | null;
}
export interface FileEntry {
  path: string;
  file_id: string;
  is_dir: boolean;
  merge_class: string;
}
export interface HistEvent {
  id: string;
  // Wall-clock unix SECONDS.
  ts: number;
  lamport: number;
  kind: string; // create | edit | rename | delete | reclass
  path: string;
  // The branch this event was authored on (for lane placement on the timeline).
  branch_id?: string;
}
export interface FileAt {
  exists: boolean;
  content: string;
}
// ---- branches (§2, §7): scoped views over the shared log ----
export interface BranchInfo {
  branch_id: string;
  name: string;
  parent: string | null;
  current: boolean;
}
export interface GraphNode {
  commit_id: string;
  branch_id: string;
  parents: string[];
  ts: number;
  lamport: number;
  label: string;
  lane: number;
}
export interface GraphBranch {
  id: string;
  name: string;
  parent: string | null;
  head_commit: string | null;
  lane: number;
  current: boolean;
}
export interface GraphTag {
  tag_id: string;
  name: string;
  at_ts: number;
  branch_id: string;
  lane: number;
}
export interface BranchGraphData {
  nodes: GraphNode[];
  branches: GraphBranch[];
  tags?: GraphTag[];
}
// A user-named marker at a point in history (unix SECONDS in `at_ts`).
export interface TagInfo {
  tag_id: string;
  name: string;
  at_ts: number;
  branch_id: string;
}
// 'receiving'|'saving' cover an ASP peer clone; 'fetching'|'replaying' are the extra
// git-bridge phases (fetch the pack over the proxy, then replay it into rows).
export type ClonePhase = 'receiving' | 'saving' | 'fetching' | 'replaying';
export type CloneProgress = (done: number, total: number, phase: ClonePhase) => void;

// The git-bridge status chip DTO (git-bridge §7.2). Shared verbatim by both
// backends: web computes it from the fold ledger, desktop's `git_status` command
// returns the same shape (asp_core::gitremote::GitStatus, camelCased at the Tauri
// boundary). `atSha` is null before the first ingest; `ahead`/`behind` are
// best-effort in v1 (exact frontier accounting lands with push).
export interface GitStatus {
  remoteUrl: string;
  atSha: string | null;
  frozen: boolean;
  ahead: number;
  behind: number;
  policy: string;
}

// The result of a `gitPush` (git-bridge §7.2). `pushedSha` is the new remote tip,
// or null when nothing was unpushed; `commits` is 0 for "nothing to commit".
export interface GitPushSummary {
  pushedSha: string | null;
  commits: number;
}

// The pending (unpushed) change set for a git vault (git-bridge §5.3) — pre-fills
// the push dialog's commit message and shows what a push would send.
export interface PendingDiff {
  filesChanged: number;
  paths: string[];
  unified: string;
}

export interface Api {
  listVaults(): Promise<VaultInfo[]>;
  // Desktop: true once the background startup reopen has finished, so the UI can
  // clear its "Loading your vaults…" gate without racing the `vaults-ready` event.
  // Web has no background reopen, so it's always ready.
  vaultsReady(): Promise<boolean>;
  addLocalFolder(path: string): Promise<VaultInfo>;
  // Create a fresh browser-storage (OPFS) vault. Web-only.
  createVault(name: string): Promise<VaultInfo>;
  // `onProgress` (web clone only) reports catch-up progress so the UI can show a
  // live bar: `phase` is 'receiving' while pages stream in, then 'saving' while
  // the state is written to OPFS. `total` may be 0 until the peer's count is known.
  cloneRemote(dest: string, ticket: string, authKey?: string, onProgress?: CloneProgress): Promise<VaultInfo>;
  // Clone a git repo into a new vault (git-bridge §7.3). Web routes git-over-HTTPS
  // through the relay CORS proxy; desktop runs the native bridge. `onProgress`
  // reports 'fetching' → 'replaying' → 'saving'. Browser is clone/pull only (no push).
  cloneGit(dest: string, url: string, token: string | undefined, depth: number | undefined, onProgress?: CloneProgress): Promise<VaultInfo>;
  // Pull new upstream commits into a git-configured vault (git-bridge §4).
  gitPull(id: string): Promise<void>;
  // The git status chip DTO, or null if the vault has no git remote configured.
  gitStatus(id: string): Promise<GitStatus | null>;
  // Commit the vault's pending changes and push them upstream (git-bridge §7.2).
  // Desktop/CLI-only — the web backend rejects (browser is clone/pull only).
  gitPush(id: string, message: string): Promise<GitPushSummary>;
  // The pending (unpushed) diff, so the push dialog can pre-fill the message and
  // show what would be sent. Web returns an empty diff (nothing to push there).
  gitPendingDiff(id: string): Promise<PendingDiff>;
  setAllowConnections(id: string, on: boolean, authKey?: string): Promise<string | null>;
  // Co-host a local relay so same-machine/LAN peers sync without the public n0
  // relay ("faster local syncing"). Desktop-only; returns the new state.
  setLocalRelay(on: boolean): Promise<boolean>;
  getLocalRelay(): Promise<boolean>;
  // Sync once against `ticket`. On web, omit `ticket` to re-dial the upstream the
  // vault was cloned from. (Web also holds a live connection — see startLiveSync.)
  syncNow(id: string, ticket?: string, authKey?: string): Promise<void>;
  // Web: open and hold a live connection to the upstream, calling `onChange`
  // whenever a remote push lands (realtime, no polling). Desktop syncs live in
  // its background engine, so this is a no-op there. Idempotent per id.
  startLiveSync(id: string, onChange: () => void): Promise<void>;
  stopLiveSync(id: string): Promise<void>;
  getStatus(id: string): Promise<VaultStatus>;
  getIdentity(): Promise<string>;
  authorize(id: string, pubkey: string): Promise<void>;
  createSnapshot(id: string, name: string): Promise<string>;
  restore(id: string, target: string): Promise<void>;
  listFiles(id: string): Promise<FileEntry[]>;
  readFile(id: string, path: string): Promise<string>;
  writeFile(id: string, path: string, content: string): Promise<void>;
  renameFile(id: string, oldPath: string, newPath: string): Promise<void>;
  createDir(id: string, path: string): Promise<void>;
  deleteFile(id: string, path: string): Promise<void>;
  history(id: string): Promise<HistEvent[]>;
  // ---- branches ----
  listBranches(id: string): Promise<BranchInfo[]>;
  currentBranch(id: string): Promise<string>;
  branchGraph(id: string, cap: number): Promise<BranchGraphData>;
  createBranch(id: string, name: string): Promise<string>;
  checkoutBranch(id: string, branchId: string): Promise<void>;
  forkBranchAt(id: string, name: string, ts: number): Promise<string>;
  deleteBranch(id: string, branchId: string): Promise<void>;
  // ---- tags: named markers at points in history ----
  listTags(id: string): Promise<TagInfo[]>;
  createTag(id: string, name: string, atTs: number): Promise<string>;
  deleteTag(id: string, tagId: string): Promise<void>;
  readFileAt(id: string, path: string, ts: number): Promise<FileAt>;
  restoreFileAt(id: string, path: string, ts: number): Promise<void>;
  rescan(id: string): Promise<void>;
  removeVault(id: string, trash: boolean): Promise<void>;
  // Reveal a folder/file in the OS file manager (Finder/Explorer). Desktop-only.
  revealPath(path: string): Promise<void>;
}

// ---- desktop backend: Tauri commands (a thin pass-through) ----
const tauriApi: Api = {
  listVaults: () => invoke<VaultInfo[]>('list_vaults'),
  vaultsReady: () => invoke<boolean>('vaults_ready'),
  addLocalFolder: (path) => invoke<VaultInfo>('add_local_folder', { path }),
  createVault: () => Promise.reject(new Error('createVault is web-only')),
  cloneRemote: (dest, ticket, authKey) => invoke<VaultInfo>('clone_remote', { dest, ticket, authKey }),
  // Invoke-arg names MUST match the Rust `#[tauri::command]` param names exactly
  // (lesson of f6c1d07); the desktop slice binds `clone_git(dest, url, token, depth)`.
  cloneGit: (dest, url, token, depth) => invoke<VaultInfo>('clone_git', { dest, url, token, depth }),
  gitPull: (id) => invoke<void>('git_pull', { id }),
  gitStatus: (id) => invoke<GitStatus | null>('git_status', { id }),
  gitPush: (id, message) => invoke<GitPushSummary>('git_push', { id, message }),
  gitPendingDiff: (id) => invoke<PendingDiff>('git_pending_diff', { id }),
  setAllowConnections: (id, on, authKey) => invoke<string | null>('set_allow_connections', { id, on, authKey }),
  setLocalRelay: (on) => invoke<boolean>('set_local_relay', { on }),
  getLocalRelay: () => invoke<boolean>('get_local_relay'),
  syncNow: (id, ticket, authKey) => invoke<void>('sync_now', { id, ticket: ticket ?? null, authKey }),
  // Desktop keeps a standing connection in its background engine; nothing for the
  // frontend to hold open, so these are no-ops (the UI refreshes on its poll).
  startLiveSync: async () => {},
  stopLiveSync: async () => {},
  getStatus: (id) => invoke<VaultStatus>('get_status', { id }),
  getIdentity: () => invoke<string>('get_identity'),
  authorize: (id, pubkey) => invoke<void>('authorize', { id, pubkey }),
  createSnapshot: (id, name) => invoke<string>('create_snapshot', { id, name }),
  restore: (id, target) => invoke<void>('restore', { id, target }),
  listFiles: (id) => invoke<FileEntry[]>('list_files', { id }),
  readFile: (id, path) => invoke<string>('read_file', { id, path }),
  writeFile: (id, path, content) => invoke<void>('write_file', { id, path, content }),
  renameFile: (id, oldPath, newPath) => invoke<void>('rename_file', { id, old: oldPath, new: newPath }),
  createDir: (id, path) => invoke<void>('create_dir', { id, path }),
  deleteFile: (id, path) => invoke<void>('delete_file', { id, path }),
  history: (id) => invoke<HistEvent[]>('history', { id }),
  listBranches: (id) => invoke<BranchInfo[]>('list_branches', { id }),
  currentBranch: (id) => invoke<string>('current_branch', { id }),
  branchGraph: (id, cap) => invoke<BranchGraphData>('branch_graph', { id, cap }),
  createBranch: (id, name) => invoke<string>('create_branch', { id, name }),
  checkoutBranch: (id, branchId) => invoke<void>('checkout_branch', { id, branchId }),
  forkBranchAt: (id, name, ts) => invoke<string>('fork_branch_at', { id, name, ts }),
  deleteBranch: (id, branchId) => invoke<void>('delete_branch', { id, branchId }),
  listTags: (id) => invoke<TagInfo[]>('list_tags', { id }),
  createTag: (id, name, atTs) => invoke<string>('create_tag', { id, name, atTs }),
  deleteTag: (id, tagId) => invoke<void>('delete_tag', { id, tagId }),
  readFileAt: (id, path, ts) => invoke<FileAt>('read_file_at', { id, path, ts }),
  restoreFileAt: (id, path, ts) => invoke<void>('restore_file_at', { id, path, ts }),
  rescan: (id) => invoke<void>('rescan', { id }),
  removeVault: (id, trash) => invoke<void>('remove_vault', { id, trash }),
  revealPath: (path) => invoke<void>('reveal_path', { path }),
};

// The web backend (wasm + OPFS) is heavy, so it's loaded lazily only when we're
// actually running in a browser.
let webApiPromise: Promise<Api> | null = null;
function backend(): Promise<Api> {
  if (isDesktop()) return Promise.resolve(tauriApi);
  if (!webApiPromise) webApiPromise = import('./webApi').then((m) => m.createWebApi());
  return webApiPromise;
}

// `api` dispatches every call to the active backend at call time.
export const api: Api = new Proxy({} as Api, {
  get(_t, prop: string) {
    return (...args: unknown[]) => backend().then((b) => (b as unknown as Record<string, (...a: unknown[]) => unknown>)[prop](...args));
  },
});
