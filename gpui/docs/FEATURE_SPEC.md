# ASP Context Desktop — Feature & API Specification

**Specification of the React+Tauri desktop app, for re-implementation in Rust/gpui.**

This document precisely maps the backend API and frontend feature behavior so the gpui port can call the Rust backend directly (asp-core via asp-desktop-engine) instead of going through Tauri commands.

---

## 1. Engine API Reference

The public surface of `asp-desktop-engine::DesktopEngine` (in `/home/chris/asp/desktop/engine/src/lib.rs`). Every method is a thin forwarder to `asp-core`; no protocol logic lives in the engine.

### Struct Definitions

**All structs are `#[derive(Clone, Serialize)]` and used only for API boundaries (React ↔ Tauri ↔ Engine).**

```rust
pub struct VaultInfo {
    pub id: String,                           // per-session local handle
    pub path: String,                         // filesystem path of the vault
    pub vault_id: String,                     // stable cross-session identity (for URL hash & tabs key)
    pub enabled: bool,
    pub listening_ticket: Option<String>,     // iroh connection ticket (share to pair)
    #[serde(default)]
    pub loading: bool,                        // true while reopening from disk at startup
}

pub struct VaultStatus {
    pub id: String,
    pub vault_id: String,
    pub rows: u64,                            // total log row count
    pub files: usize,                         // number of live (non-deleted) files
    pub head: String,                         // git HEAD ref (main branch)
    pub listening_ticket: Option<String>,
    pub peers: Vec<String>,                   // ssh identities of connected peers
    pub last_ts: Option<i64>,                 // wall-clock unix seconds of most recent log row (or null for empty vault)
}

pub struct FileEntry {
    pub path: String,                         // slash-separated path (relative to vault root)
    pub file_id: String,                      // stable file identity
    pub is_dir: bool,
    pub merge_class: String,                  // "dir" or "text" (from asp-core MergeClass)
}

pub struct HistEvent {
    pub id: String,
    pub ts: i64,                              // wall-clock unix seconds
    pub lamport: u64,
    pub kind: String,                         // "create" | "edit" | "rename" | "delete" | "reclass"
    pub path: String,                         // resolved path (created/renamed rows carry it; edits/deletes resolved via file_id tracking)
}

pub struct FileAt {
    pub exists: bool,
    pub content: String,                      // materialized file bytes at the requested timestamp (lossy UTF-8)
}
```

### Public Methods

#### Identity & Initialization

```rust
pub fn new(identity: Identity) -> Result<DesktopEngine>
```
- Create the engine with a device identity (one per device, stored at `~/.asp/id_ed25519`).
- Initializes the tokio runtime, creates the change-notification channel.

```rust
pub fn identity_ssh(&self) -> String
```
- Return the SSH public key string for this device (for peer authorization).

```rust
pub fn take_change_receiver(&self) -> Option<std::sync::mpsc::Receiver<String>>
```
- Take (and drain) the receiver for "a peer's edit landed on folder `<id>`" notifications.
- Called once at startup by the Tauri shell; returns `None` if already taken.
- The shell emits a Tauri `vault-changed` event for each received id.

#### Vault Lifecycle

```rust
pub fn list_vaults(&self) -> Vec<VaultInfo>
```
- Return all currently-managed vaults (open folders + still-loading ones from startup).
- Loading vaults (those still being scanned in the background) appear as placeholder `VaultInfo` rows with `loading=true` and synthetic ids (`loading:<path>`).

```rust
pub fn publish_loading(&self)
```
- Called synchronously at startup **before** `reopen_saved()` to instantly show "Opening…" placeholders.
- Reads the saved-folder config (`~/.asp/desktop_folders.json`) and publishes the list as loading vaults (cheap, no disk scan).

```rust
pub fn reopen_saved(&self) -> Result<Vec<VaultInfo>>
```
- Called on a background thread at startup to re-open every saved folder from the previous session.
- Skips stale GUI-test harness temp vaults (under OS temp dir with `aspgui-*` basename).
- Synchronous: each folder is in `list_vaults()` by the time this returns.
- Returns the opened vaults; drops loading placeholders as they become real.

```rust
pub fn add_local_folder(&self, path: &Path) -> Result<VaultInfo>
```
- Initialize a new or open an existing vault at a local path.
- Captures current disk contents into the log (via `Engine::capture_rescan()`).
- Remembers the path in `~/.asp/desktop_folders.json` so it reopens on next launch.
- Returns the new `VaultInfo`.

```rust
pub fn remove_vault(&self, id: &str, _trash: bool) -> Result<()>
```
- Stop managing a vault: abort its listener/connector, drop it from the in-memory map, and remove it from the saved-folder list.
- The `_trash` parameter is accepted but ignored (OS-trash deletion is deferred; we never destroy data).

#### Peer Connection

```rust
pub fn set_allow_connections(&self, id: &str, on: bool, auth_key: Option<&str>) -> Result<Option<String>>
```
- Toggle "allow connections": bind (or tear down) a per-folder iroh listener (the `asp watch --listen` equivalent).
- If `on=true`, binds the folder's single shared endpoint lazily (reuses if already bound) and starts the listener.
  - Returns the iroh ticket (shareable connection code).
  - If already listening, returns the existing ticket.
- If `on=false`, aborts the listener and returns `None`.
- Each folder maintains exactly one endpoint (shared by listener and connector); neither closes it.

```rust
pub fn clone_remote(&self, dest: &Path, ticket: &str, auth_key: Option<&str>) -> Result<VaultInfo>
```
- Bootstrap a new vault by cloning from a remote peer (by iroh ticket / node id).
- Binds the folder's single shared endpoint and runs the bootstrap on it.
- **Starts a persistent reconnecting connector** to the source (so the clone stays live-synced).
- Persists the source ticket, so on reopen the connector reconnects automatically.
- Returns the new `VaultInfo`.

```rust
pub fn sync(&self, id: &str, ticket: &str, auth_key: Option<&str>) -> Result<()>
```
- One-shot sync of a folder against a peer (catch-up + the UI's "sync now" button).
- Reuses the folder's shared endpoint if one exists; otherwise binds a throwaway one.
- Runs a single (non-persistent) sync session via `iroh_net::sync_oneshot()`.

#### File Operations

```rust
pub fn list_files(&self, id: &str) -> Result<Vec<FileEntry>>
```
- Return all live (non-deleted) files in the vault as a flat list of slash-separated paths.
- UI builds the nested tree from this flat list (see `vault/tree.ts`).

```rust
pub fn read_file(&self, id: &str, path: &str) -> Result<String>
```
- Read a live file's current content from disk (the materialized truth).
- Returns lossy UTF-8 (invalid bytes → U+FFFD).

```rust
pub fn write_file(&self, id: &str, path: &str, content: &str) -> Result<()>
```
- Create or update a file by recording an edit (persists to log + disk via `asp-core` materialize).
- New paths author a `Create` row; existing paths author an `Edit` row.
- **Broadcasts** the authored `WireRow` to every connected peer in real time (live push).

```rust
pub fn rename_file(&self, id: &str, old: &str, new: &str) -> Result<()>
```
- Rename/move a file (preserves its stable `file_id`).
- Authors a `Rename` row, materialized to disk.
- **Broadcasts** to connected peers.

```rust
pub fn delete_file(&self, id: &str, path: &str) -> Result<()>
```
- Delete a file (authors a tombstone; removes it from disk on materialize).
- **Broadcasts** to connected peers.

```rust
pub fn create_dir(&self, id: &str, path: &str) -> Result<()>
```
- Create an empty directory (first-class entity in asp-core).
- Physically creates the directory with `mkdir`, then calls `capture_rescan()` to author the `Dir` row(s).
- **Broadcasts** all authored rows to connected peers.

#### History & Time-Travel

```rust
pub fn history(&self, id: &str) -> Result<Vec<HistEvent>>
```
- Project the append-only log into wall-clock history events for the time-travel scrubber.
- Resolves the path for every row (edits/deletes carry none; we track each `file_id`'s latest path in fold order).
- Returns events sorted by timestamp.

```rust
pub fn read_file_at(&self, id: &str, path: &str, ts: i64) -> Result<FileAt>
```
- Read a file's content as the vault was at wall-clock unix seconds `ts` (read-only time travel).
- Folds rows with `ts <= ts` via `asp-core::state_as_of()`.
- Returns `{ exists: true, content }` if the file existed then, else `{ exists: false, content: "" }`.

```rust
pub fn restore_file_at(&self, id: &str, path: &str, ts: i64) -> Result<()>
```
- Restore one file to its content as-of `ts` (records the historical bytes as a new edit).
- The log stays append-only; no-op if the file didn't exist then.
- Authors the new edit row, **broadcasts** to connected peers.

```rust
pub fn rescan(&self, id: &str) -> Result<()>
```
- Manual refresh of on-disk changes into the log (after external edits, git pulls, scripts).
- Calls `capture_rescan()` and **broadcasts** all authored rows to connected peers.

#### Snapshots

```rust
pub fn snapshot(&self, id: &str, name: &str) -> Result<String>
```
- Create a named snapshot of the vault's current state.
- Returns the snapshot id (for `restore()`).

```rust
pub fn restore(&self, id: &str, target: &str) -> Result<()>
```
- Restore the vault to a named snapshot.
- Authors rows that revert the vault to the target state.
- **Broadcasts** all rows to connected peers.

#### Vault Configuration

```rust
pub fn set_enabled(&self, id: &str, on: bool) -> Result<()>
```
- Toggle a vault's enabled state (UI-only; not protocol-significant).

```rust
pub fn authorize(&self, id: &str, pubkey: &str) -> Result<()>
```
- Authorize a peer's public key to access this vault.

```rust
pub fn list_authorized(&self, id: &str) -> Result<Vec<String>>
```
- Return the list of authorized peer node ids.

#### Status & Status Polling

```rust
pub fn status(&self, id: &str) -> Result<VaultStatus>
```
- Get live sync state: row count, file count, git HEAD ref, peers, listening ticket, and `last_ts`.

---

## 2. Feature Catalog

Every user-facing feature, grouped by screen.

### Connect Screen (Initial Vault List)

**State:**
- List of vaults (opened + loading placeholders).
- Statuses (peers, last sync time) per vault.

**Features:**

1. **List Vaults**
   - Calls: `listVaults()` → `getStatus()` for each.
   - Behavior: Shows opened vaults + loading placeholders. Loading rows are non-interactive ("Opening…").
   - Persistence: Vaults reopen via `reopen_saved()` at startup.

2. **Open Local Folder**
   - UI: Native OS folder picker (Tauri `open({ directory: true })`).
   - Calls: `addLocalFolder(path)`.
   - Behavior: Initializes a new vault or opens an existing one; captures disk into log.
   - Feedback: Vault appears in the list; status refreshes.

3. **Connect with Code (Clone)**
   - UI: Inline form: destination path picker, ticket input, optional auth key.
   - Calls: `cloneRemote(dest, ticket, authKey)`.
   - Behavior: Bootstraps the vault from the peer, starts persistent connector.
   - Feedback: Vault appears in the list with the source as its upstream peer.

4. **Vault Context Menu** (right-click vault)
   - Options:
     - **Customize** → modal with name, color hue, emoji.
     - **Share** → shows ticket + optional access-key option.
     - **Remove** → confirm modal with trash toggle.
   - Calls: `setAllowConnections(on, authKey)` for share; `removeVault(id, trash)` for remove.

5. **Share Modal**
   - Calls: `setAllowConnections(id, true, authKey)` → returns ticket.
   - UI: Displays the ticket (with copy button), toggle for optional access key.
   - Behavior: Access key is randomly generated (`XXXX-XXXX-XXXX-XXXX` format) and shown only once.

6. **Status Polling** (every 10s)
   - Calls: `listVaults()`, `getStatus()` for each.
   - Updates: vault list (catches late startup reopens), peer count, last sync time.

### Editor Screen (File Editing)

**State (per active vault):**
- File tree (built from flat list).
- Expanded directories (persisted per vault).
- Selected file (active editor file).
- Multi-selection (for batch operations).
- Open tabs (persisted per vault).
- Active file in URL hash.

**Features:**

#### Sidebar & File Tree

1. **Vault Switcher** (top of sidebar)
   - Calls: `listVaults()`.
   - Shows: Avatar (emoji/monogram + color), name, peer count, last sync.
   - Behavior: Click → open editor for that vault.
   - Persistence: Selected vault is implicit (active file URL hash).

2. **File Tree Render**
   - Calls: `listFiles(id)` on vault open.
   - Behavior:
     - Builds nested tree from flat paths (union explicit dirs + implied parents).
     - Sorts: ALL-CAPS note stems (e.g., README.md) float top, then folders, then files; natural name order within each group.
     - Flattens to indented rows; displays only expanded dirs.
   - Expansion state: Persisted per vault in React state; automatically expands path to selected file.

3. **Select File** (click in tree)
   - Calls: `readFile(id, path)`.
   - Behavior: Flushes unsaved edits of previous file, then opens new file. Adds file to open tabs.
   - URL hash updated to vault_id/path (for link sharing + refresh restore).

4. **Expand/Collapse** (click folder arrow)
   - Behavior: Toggles expanded state; no backend call.

5. **Expand All / Collapse All** (double-click folder header)
   - Behavior: Toggles all directories open/closed.

#### Context Menu (right-click file/folder)

1. **New File** (in parent)
   - Calls: `writeFile(id, path, content)`.
   - Behavior: 
     - Generates unique name (`untitled.md`, `untitled-1.md`, …).
     - Creates with boilerplate content (`# Untitled\n\n`).
     - Shows optimistically (before backend) so rapid second click picks distinct name.
   - Persistence: Rendered file appears in tree immediately.

2. **New Folder**
   - Calls: `createDir(id, path)`.
   - Behavior: Creates empty dir, inline-renames it (text input appears).

3. **Rename** (inline)
   - Calls: `renameFile(id, oldPath, newPath)` for each affected path.
   - Behavior:
     - Renames file + any nested children (if folder).
     - Remaps: open tabs, selected file, expanded dirs, content cache.
     - Flushes dirty buffer of old path before commit.
   - Edge case: Collision at destination is a no-op.

4. **Delete** (with confirmation)
   - Calls: `deleteFile(id, path)` for each victim.
   - Behavior:
     - Deletes file + any nested children.
     - If active file is deleted, switches to next/prev tab or vault default.
   - Persistence: File removed from tree; tabs/selection updated.

5. **Move** (drag into folder)
   - Calls: `renameFile(id, oldPath, newPath)` for each moved path.
   - Behavior: Same remapping as rename; guards: no-op if already in dest, into itself/descendant, or name collision.

#### Tab Bar

1. **Open Tabs** (horizontal bar below file tree)
   - State: List of file paths open in current vault (persisted in localStorage per vault_id).
   - Behavior: Click to switch active file; close button to close individual tab.
   - Persistence: Saved whenever tab list changes; restored on vault reopen.

2. **Close Tab**
   - Behavior: If active, switches to next tab (prefer right, then left); closes non-active silently.

3. **Close All / Close Others / Close to Left / Close to Right** (tab context menu)
   - Behavior: Updates tab list; active file reassignment if needed.

4. **Reorder Tabs** (drag within bar)
   - Behavior: Pure list transform; no backend call.

#### Editor

1. **Open File** (select in tree)
   - Calls: `readFile(id, path)` (live only, not time-travel).
   - Content cached in memory (key: `${id}::${path}`).
   - Behavior: Renders markdown (`.md`) as live WYSIWYG; code files (all others) as monospace with syntax highlighting.

2. **Edit File** (type in editor)
   - Behavior: 
     - Marks as dirty (`dirtyRef`).
     - Debounces 650ms; then calls `writeFile(id, path, content)`.
     - Shows "Saving…" indicator while in flight.
   - Persistence: Unsaved edits are NOT buffered locally; source of truth is the backend.
   - No localStorage edit buffer.

3. **Caret Preservation**
   - Behavior: On single-line re-render (live editor), caret math maps old→new position via character offset across line nodes.
   - Edge case: Table rows expand to real divs; diagrams are skipped in caret math.

4. **Live Refresh** (peer edit lands)
   - Calls: `readFile(id, path)` on `watchVault()` push (realtime desktop; 2s poll on web).
   - Behavior: Re-renders only if dirty=false AND viewing live (not time-travelling).
   - No repaint if bytes unchanged.

#### History & Time-Travel (bottom panel)

1. **Toggle History Panel**
   - Calls: `history(id)` on tab click; debounced 700ms.
   - Behavior: Shows timeline track with events (create/edit/rename/delete), current live now(), playhead for scrubbing.
   - Persistence: Minimal (open/closed); history is live-derived.

2. **Scrub Timeline**
   - Behavior: Drag playhead to any point in time.
   - Calls: `readFileAt(id, path, ts)` (for displayed file).
   - Content rendered read-only; "Restore this version" button appears.

3. **Restore File Version**
   - Calls: `restoreFileAt(id, path, ts)` → `readFile(id, path)` to refresh.
   - Behavior: Records the historical bytes as a new edit; updates history.
   - Broadcasts to peers.

4. **Now Button**
   - Behavior: Jumps playhead to current time; exits time-travel.

5. **Zoom In / Zoom Out** (+/- buttons)
   - Behavior: Scales the timeline view; clamped [10min, 60day] span.

6. **Event Log** (alternate tab)
   - Calls: `history(id)`, `status(id)`, `getIdentity()`.
   - Behavior: Renders derived log lines (net/peer/sync/merge/disk events) + the real history.
   - No backend call when tab is not visible.

#### Save & Sync Behavior

1. **Debounced Write**
   - User types → 650ms debounce → `writeFile()`.
   - Cancels on: vault switch, file switch, time-travel mode enter.
   - Flush on: vault switch (before `readFile` of new file), time-travel exit.

2. **Auto Sync** (every 2s)
   - Calls: `autoSync(id)` (desktop: no-op; web: pulls upstream).
   - Then: `getStatus()`, `listFiles()`, `readFile()` (live file), `history()` (every 5 ticks = ~10s).
   - Behavior: Live mirror of backend's anti-entropy; peer edits show within ~2s.

3. **Watch Vault** (realtime push)
   - Desktop: Listens for `vault-changed` Tauri event (from engine's change-notifier).
   - Web: Holds persistent relay connection (reconnect on drop).
   - Triggers: `getStatus()`, `listFiles()`, `readFile()` (if viewing live).

4. **Manual Sync** (UI button)
   - Calls: `syncNow(id, ticket, authKey)`.

---

## 3. State & Data Flow

### App-Level State (React)

**Persisted (localStorage):**
- `asp.prefs.v1`: User preferences (accent, theme, font, sidebar width, history bar height, etc.).
- `asp.vaultmeta.v1`: Per-vault cosmetics (name, color hue, emoji).
- `asp.tabs.<vault_id>`: Open tabs per vault (list of file paths).
- URL hash: Active vault_id and file path (for link sharing + refresh restore).

**In-Memory (React state):**
- Selected vault (active editor vault).
- File tree + expanded dirs (per vault).
- Selected file (active editor file).
- Multi-selection (Set of file paths).
- Open tabs (per vault).
- Dirty flag + content buffer + save timer.
- History (events list) + playhead (time-travel position).
- Viewport (history timeline geometry).
- UI state (menus, modals, renaming, dragging).

**Refs (for imperative closures):**
- `activeIdRef`, `selectedRef`, `selectedPathsRef`: Latest state for async handlers.
- `bufferRef`: Current editor content (for debounced save).
- `contentRef`: Cache of file contents (key: `${id}::${path}`).
- `filesRef`, `vaultsRef`, `openTabsRef`: Latest file/vault lists for tree transforms.
- `playheadRef`, `viewRef`, `nowRef`: History state for handlers.

### Backend Persistence

**Stored by asp-core:**
- `.asp/asp.db`: SQLite log (all rows with ts, lamport, kind, path, file_id).
- Materialized files: Actual file content on disk.
- git refs: `refs/heads/main` (HEAD).

**Stored by asp-desktop-engine:**
- `~/.asp/desktop_folders.json`: Saved folder list (paths + upstream peer ticket).
  - Format: `[{ path: "...", peer: "..." }, ...]`
  - Persisted on: `add_local_folder()`, `clone_remote()`, on-reopen peer update.
  - Pruned on: `remove_vault()`, stale harness detection at startup.

**Device Identity (shared with CLI):**
- `~/.asp/id_ed25519`: Hex-encoded 32-byte seed.
- `~/.asp/id_ed25519.pub`: SSH public key string.

### Data Flow

1. **Startup:**
   - `publish_loading()` (cheap config read) → shows placeholders instantly.
   - `reopen_saved()` (background) → opens each folder, drops loading placeholder as it finishes.
   - Subscribers call `listVaults()` + `getStatus()` once reopens are done.

2. **File Editing:**
   - User selects file → `readFile()` (live) or `readFileAt()` (time-travel) → caches in `contentRef`.
   - User types → debounce 650ms → `writeFile()`.
   - Peer edits → `watchVault()` push → `readFile()` (if viewing live, not dirty) → repaint.

3. **History:**
   - User opens history panel → `history()` (list all events) → builds timeline.
   - User scrubs → `readFileAt()` for displayed file → read-only render.
   - User restores → `restoreFileAt()` → new edit row authored + broadcast.

4. **Live Sync:**
   - In-app edits (`write_file`, `rename_file`, `delete_file`, `restore_file_at`) → authored `WireRow` → **broadcast to all connected peers** in real time.
   - Persistent connector (on cloned vaults) stays open; one-shot sync (manual) reuses folder's shared endpoint.
   - Desktop's change-notifier fires on remote integrate → Tauri `vault-changed` event → UI refreshes.

### Polling Intervals

**Fast (every 2s):**
- On editor screen + active vault:
  - `autoSync(id)` (web only; desktop no-op).
  - `getStatus(id)`.
  - `listFiles(id)`.
  - `readFile(id, path)` (if viewing live, not dirty).

**Slow (every 10s = every 5th tick):**
- Editor screen: `history(id)` (debounced 700ms).
- Connect screen: `listVaults()` + `getStatus()` for all.

### State Machine (Screen & File Selection)

- **Connect → Editor:** User selects vault → `openVault(id)` → loads files, tree, selected file (from URL hash / restored tabs / vault default), expands path, clears history.
- **Editor → Editor (vault switch):** Flush save → `openVault(new_id)`.
- **File select:** Flush previous save → `readFile(new_path)`.
- **Time-travel:** Set `playhead !== null` && `playhead < now` → `readFileAt()` (read-only), "Restore" button appears.
- **Exit time-travel:** Set `playhead = null` → "Now" button clicked or file switch → `readFile()` (live again).

---

## 4. Pure Logic Modules (`src/vault/*.ts`)

### history.ts

**Purpose:** Timeline geometry and event derivation for the time-travel scrubber.

**Structs:**
```typescript
interface View { start: number; end: number; }  // epoch ms window of the timeline
interface TrackEvent { id, ts: number (ms), kind, path }  // backend history converted to ms
interface AxisTick { label: string; pct: number }  // grid label for an axis
```

**Algorithms:**

- **`defaultView(now)`**: Initialize 7-day history + 0.4-day future.
- **`clampView(start, end, now)`**: Constrain view to [now - 90 days, now + span*0.4], shifting if out of bounds.
- **`toPct(ts, view)`**: Convert unix-ms timestamp to percent within view.
- **`chooseStep(span)`**: Pick axis-tick interval (5min, 15min, 30min, 1h, 3h, 6h, 12h, 1d, 2d, 7d, 14d, 30d) such that 6 ticks fit in span.
- **`zoomKeepingFocus(view, f, factor, now)`**: Scale span by factor, keeping point at fraction `f` of view fixed; clamp result.
- **`zoomAround(view, center, factor, now)`**: Zoom centered on center-point (playhead or now).
- **`viewForNow(view, now)`**: If now fell outside view, re-center on now (82% to left, 18% to right).
- **`fmtFull(ts)` / `fmtTick(ts, step)`**: Format timestamp for display (full: "Jan 1, 12:34"; tick: month/date or time based on step).
- **`colorOf(kind)`**: Map event kind → CSS color (create: green, edit: blue, rename: gold, delete/reclass: red).
- **`buildEvents(hist)`**: Convert backend `HistEvent[]` (unix seconds) to `TrackEvent[]` (epoch ms), sorted by ts.
- **`createTsByPath(events)`**: Build map of earliest event ts per path (for "file did not exist yet" indicator).

### tree.ts

**Purpose:** Nested file tree construction and flat-row rendering.

**Structs:**
```typescript
interface TreeNode {
  type: 'file' | 'dir';
  name: string;
  path: string;
  children?: TreeNode[];
}

interface FlatRow {
  node: TreeNode;
  depth: number;
}
```

**Algorithms:**

- **`buildTree(files: FileEntry[])`**: 
  - From flat slash-paths, construct a nested tree.
  - Union: explicit `is_dir` entries + implied parents (any path with `/`).
  - Sorts each level: ALL-CAPS note stems (e.g., README.md) float top, then folders, then files; natural (numeric) name order.
  - Returns: root's children.

- **`compareNodes(a, b)`**: Comparator: all-caps notes (rank 0) > folders > files; then natural name order.

- **`flatten(tree, expanded, depth)`**: Flatten tree to rows, honoring `expanded` map; returns `{ node, depth }` per row (depth = nesting level).

- **`allDirPaths(tree)`**: Return all directory paths (for "expand all").

- **`firstSelectable(tree)`**: Return first README (any depth) or first file (default selection when opening vault).

### tabs.ts

**Purpose:** Open-tabs list and URL hash encoding.

**URL Scheme:**
```
#<encodeURIComponent(vaultId)>/<encodeURIComponent(filePath)>
```
- Both parts fully percent-encoded; literal `/` separator unambiguous even with `/` in path.

**Algorithms:**

- **`buildHash(vaultId, path)`**: Encode vault + file into hash.
- **`parseHash(hash)`**: Decode hash back to `{ vaultId, path }`; returns null on malformed.
- **`withTab(tabs, path)`**: Add path to tabs if not already present; returns same ref if unchanged.
- **`closeTab(tabs, active, path)`**: Remove path; if active, pick next or previous; return `{ tabs, active }`.
- **`remapTabs(tabs, oldPath, newPath)`**: Rename/move path + its subtree in tabs; de-dup on collision; preserve first-seen order.
- **`removeTabs(tabs, paths)`**: Drop exact matches + subtree matches (folder deletes).
- **`reorderTabs(tabs, from, to)`**: Drag-to-reorder; no-op if out of range.
- **`closeOthers(tabs, path)`**: Close all except `path`.
- **`closeToLeft(tabs, path)` / `closeToRight(tabs, path)`**: Close left or right of `path`.
- **`closeAll()`**: Return `[]`.

**Persistence:**
- Per-vault in localStorage key `asp.tabs.<vaultId>`.

### markdown.ts

**Purpose:** Live contentEditable WYSIWYG markdown rendering and caret preservation.

**Invariant:** 1:1 mapping of source lines ↔ top-level `<div>` elements (empty line → `<br>`). Caret math and line remap depend on this.

**Rendering:**

- **`inlineMd(raw)`**: Escape HTML, wrap backticks + code in spans, render links + images (keeping literal syntax in hidden `.cm-mark` spans), bold/italic.

- **`renderLiveHtml(src, accent, fmStyle)`**: Full document render:
  - Frontmatter (YAML `---` … `---` block) → properties display (Card / Banner / Below style).
  - Code fences (` ``` `) → monospace + syntax highlighter (for recognized langs).
  - Diagrams (` ```mermaid` / ` ```diagram` ) → fence + rendered SVG preview (skipped in line walkers).
  - Tables (pipe rows) → grouped under `.tbl-scroll > .tbl-grid` (columns align, table scrolls).
  - Headings (`#–####`) → sized + bold.
  - Blockquotes (`> …`) → accent bar + italic.
  - Horizontal rules (`---` or `***`) → thin line.
  - Task lists (`- [ ] …` / `- [x] …`) → checkbox + strikethrough if done.
  - Unordered lists (`- …` / `* …`) → bullet.
  - Ordered lists (`1. …`) → number (colored accent).
  - Paragraphs → default serif prose.
  - All literal markdown syntax wrapped in `.cm-mark` (hidden) so `readLive()` reconstructs exactly.

- **`renderCodeHtml(src, lang)`**: Per-language syntax highlighter (js, py, rs, sh, yaml, json, sql, html, css, etc.); each line emitted as `<div>`.

- **`isCodeFile(path)` / `langOf(path)`**: Identify code files (non-`.md`) and resolve language key.

**Caret Preservation:**

- **`lineNodes(el)`**: Flatten table wrapping (`.tbl-scroll > .tbl-grid > .tbl-row`), skip diagrams, to get the true line-node list.

- **`lineIndexOf(el, node)`**: Find source-line index of a line node (in flattened space).

- **`caretOffset(el)`**: Map current selection endpoint to character offset across line nodes (+1 per line boundary).

- **`setCaret(el, target)`**: Restore caret to character offset; descends into text nodes via TreeWalker.

- **`localCaretOffset(line, container, endOffset)`**: Count text characters up to boundary within a single line (handles element boundaries).

- **`readLive(el)`**: Extract source markdown: one source line per line node's textContent; `<br>` → "".

**Code Highlighting:**

- **`lineHighlighterFor(lang)`**: Return per-line highlighter for a language key.
- **Tokenizer:** Regex-based for most languages (js, py, rs, sh, yaml, json, sql, toml); bespoke scanners for HTML (tag/attr/entity detection) and CSS (prop/value/selector).
- **Invariant:** Output preserves textContent exactly (only wraps in `<span>` wrappers, never inserts/drops characters).

**Helpers:**

- **`wordCountOf(content)`**: Count words in markdown (for word-count label).
- **`countLabel(content, path)`**: Return "X words" for markdown, "X lines" for code.
- **`renderDoc(src, path, accent, fmStyle)`**: Route to code or markdown renderer based on path.

### tabs.ts (File → Tab Transforms)

All transform operations documented above in **tabs.ts** section.

### prefs.ts

**Persistence:** localStorage `asp.prefs.v1` (JSON).

**Prefs struct:**
```typescript
interface Prefs {
  accent: string;              // hex color (#3d63dd default)
  frontmatterStyle: 'Card' | 'Banner' | 'Below';
  writingColumn: boolean;      // centered prose column
  theme: 'light' | 'dark';
  sidebarW: number;            // sidebar width (clamped [200, 460])
  histBarH: number;            // history bar height (clamped [96, 640])
  showHidden: boolean;         // show dotfiles
  prettyNames: boolean;        // titleize filenames
}
```

**Algorithms:**

- **`loadPrefs()` / `savePrefs(prefs)`**: Load/save from localStorage; defaults to `DEFAULT_PREFS` on missing/corrupt.
- **`applyTheme(theme)`**: Set `data-theme` attribute on `<html>` (CSS variables respond).
- **`fontFamilyOf()`**: Return serif font family (Newsreader; always serif for prose readability).
- **`clampSidebar(w)` / `clampHistBar(h)`**: Constrain sizes to valid ranges.

### vaultMeta.ts

**Persistence:** localStorage `asp.vaultmeta.v1` (JSON), keyed by stable `vault_id`.

**Metadata struct:**
```typescript
interface VaultMetaEntry {
  name?: string;              // custom display name
  hue: number;                // 0–359 (swatch hues: [222, 158, 32, 268, 344, 188, 46, 12])
  emoji?: string | null;
}
```

**Algorithms:**

- **`hash(str)`**: djb2 hash (matches design's `hash`); deterministic default hue from vault_id.
- **`hueForId(id)`**: `hash(id) % 360`.
- **`resolveMeta(map, vaultId, fallback)`**: Overlay saved metadata or return defaults (fallback name, hashed hue, no emoji).
- **`glyphOf(meta)`**: Avatar glyph: emoji if set, else first letter, else "·".
- **`avatarStyle(meta, size, radius)`**: CSS for pastel tinted avatar (hue-based background, theme-independent).

### format.ts

**Helpers:**

- **`basename(p)`**: Last component of path.
- **`relTime(sec)`**: Human "time ago" (unix seconds) or em-dash.
- **`shortFingerprint(identity)`**: Abbreviate ssh public key to readable form.
- **`makeAccessKey()`**: Random `XXXX-XXXX-XXXX-XXXX` access key (Crockford-ish, no ambiguous chars).
- **`freeName(siblings, ext)`**: Generate unique untitled name (`untitled.md`, `untitled-1.md`, …).

### log.ts

**Derivation:** Real history + status → synthetic event-log lines.

**Algorithms:**

- **`deriveLog(events, status, identity, opts)`**: 
  - Generates lines (up to 40 recent events by default).
  - Format: net (endpoint/listening), peer (connects), sync (row count), row (per-event), merge (clean merges), ok (final status).
  - Timestamps are derived from event ts, with tiny monotonic nudge for readability.
- **`shortFinger(identity)`**: Short device tag from ssh key.
- **`logColor(level, accent)`**: Map log level → CSS color.
- **`logText(lines)`**: Format lines for display/copy.

### diagram.ts

**Purpose:** Mermaid diagram rendering in markdown fences.

**Invariant:** Diagram preview (`.md-diagram`) is appended AFTER the closing fence, with `contenteditable=false`, `data-diagram-src` attribute (source stashed for async render). Line walkers SKIP `.md-diagram`, so caret math is unaffected.

**Algorithms:**

- **`isDiagramLang(info)`**: Check if fence info is `mermaid` or `diagram` (case-insensitive).
- **`fenceInfo(line)`**: Extract info-string from opening ``` line.
- **`diagramPreviewHtml(source)`**: Generate `.md-diagram` div with fallback `<pre>` code.
- **`applyCachedDiagrams(root)`**: Synchronously fill cached diagrams from in-memory SVG cache.
- **`renderDiagrams(root, load)`**: Async mermaid render; graceful degradation if library missing or parse fails.
- **SVG Cache:** Per-source in-memory; survives full editor re-renders so diagrams don't flicker.

### prettyNames.ts

**Algorithms:**

- **`isHidden(name)`**: Starts with `.`.
- **`prettyName(name, isDir)`**: Transform raw filename:
  - Dotfiles: shown verbatim.
  - Dirs: titleize (dash/underscore → space, capitalize words).
  - Notes (`.md`): titleize, italic flag if ALL-CAPS stem (e.g., README).
  - Other: shown verbatim.

### emoji.ts

**Emoji support:** Full emoji set (U+1F300–U+1F999); used in vault avatar customization.

---

## 5. Markdown Feature Support

**Exactly which constructs render (and how):**

### Block-Level

- **Headings** (`#`, `##`, `###`, `####`): Sized (26px, 21px, 17.5px, 15.5px), bold, leading spaces via `.cm-mark`.
- **Paragraphs**: Default (serif, 1.8 line-height); inline markdown applied.
- **Blockquotes** (`> …`): `.cm-quote` div with left accent bar; leading `>` in `.cm-mark`.
- **Lists (unordered)** (`- …`, `* …`): `.cm-ul` div; leading marker in `.cm-mark`.
- **Lists (ordered)** (`1. …`, `2. …`): Number in accent color; leading in `.cm-mark`.
- **Task lists** (`- [ ] …`, `- [x] …`): Zero-width clickable checkbox element (no source text); strikethrough if `[x]` (done).
- **Code fences** (` ``` ` + info): 
  - Syntax highlighting (per language).
  - Mermaid/diagram: fence + rendered SVG preview below.
  - All fence lines (including opening/closing `) stay editable, round-trip byte-for-byte.
- **Tables** (pipe rows): Grouped under `.tbl-scroll > .tbl-grid`; columns align horizontally; table scrolls.
- **Horizontal rules** (`---`, `***`): Thin border-bottom.
- **Frontmatter** (YAML `---` … `---` block, leading position): Display as properties (Card/Banner/Below style). Literal fences in `.cm-mark`; key/value parsing with regex.

### Inline

- **Code** (backticks): Hidden backticks in `.cm-mark`; content in `.cm-code` span (monospace).
- **Bold** (`**…**`): Hidden `**` in `.cm-mark`; content in `<strong>`.
- **Italic** (`*…*`): Hidden `*` in `.cm-mark`; content in `<em>`. Word-boundary rules (` *word* ` valid, `word*word` not).
- **Links** (`[text](url)`): Visible text, hidden syntax in `.cm-mark`, url in `data-href` on link span (clickable, no editable `<a>`).
- **Images** (`![alt](url)`): Visual image badge (`.cm-img` no-source), hidden literal in `.cm-mark`.
- **Image-in-link** (`[![alt](img)](url)`): Visual badge clickable to url, hidden literal.

### Not Rendered (Preserved as Text)

- Inline HTML.
- Strikethrough (`~~…~~`).
- Subscript/superscript.

### Special Cases

- **Empty lines**: Render as `<br>` (so caret can land in them).
- **Inline styling in headings/lists**: Inlines applied (bold, italic, links, code).
- **Emoji**: Rendered directly (no escaping); used in vault avatars (cosmetic only).

---

## 6. Edge Cases & Invariants

### Caret & Selection

- **Strict 1:1 line↔div mapping:** Every source line corresponds to exactly one top-level `<div>` (tables expand to `.tbl-row`, diagrams skipped). Caret math depends on this; any rendering change must preserve it.
- **Hidden `.cm-mark` spans:** Markdown syntax wrapped in `.cm-mark { display: none }`. `textContent` of line node includes them, so `readLive()` reconstructs exactly. Caret offset counts ALL text (including hidden marks).
- **Table rows:** Grouped under `.tbl-scroll > .tbl-grid` for column alignment + horizontal scroll, but each `.tbl-row` maps 1:1 to a source line. `lineNodes()` flattens this; caret walkers descend through both wrappers.
- **Diagram previews:** `.md-diagram` divs are `contenteditable=false` and skipped in line walkers. Rendering a diagram AFTER the fence keeps it outside the editable source.
- **Caret restoration:** On single-line re-render, `caretOffset()` → re-render → `setCaret()` preserves caret within the same line. Multi-line changes (paste, undo) reset caret to end.

### File Remap (Rename / Delete / Move)

- **Paths in state:**
  - `selectedPath`: If deleted, switch to next tab or vault default.
  - `selectedPaths` (multi-select): Remove deleted; remap renamed/moved.
  - `openTabs`: Remap on rename/move of file or any ancestor folder; drop on delete of file or ancestor folder.
  - `expanded`: Remap keys (folder paths) on rename/move.
  - `contentRef`: Remap cache keys `${id}::${oldPath}` → `${id}::${newPath}`.
- **Optimistic update:** File list updated in React state immediately (before backend response); if backend fails, state is briefly out-of-sync but self-corrects on next poll.
- **Folder subtree:** Rename/delete/move of a folder carries all nested children (implicit by path prefix).

### File Selection & Tabs

- **Dirty on switch:** Before switching files, flush unsaved edits via `writeFile()`.
- **Tab persistence:** Saved on every tab list change (add, close, reorder). Restored on vault reopen; drops any now-missing paths.
- **Active file:** URL hash is source of truth (for link sharing + refresh restore). On vault reopen: hash-named file > first restored tab > vault's default file.
- **Multi-selection + delete:** If any file in multi-select is deleted, whole selection is deleted (not just the one right-clicked).

### Time-Travel

- **Playhead:** Set to unix-milliseconds (stored as `playhead` state). When `playhead < now`, viewing history (read-only). When `playhead = null`, viewing live.
- **Content mode:** Live read from file via `readFile()`; history read via `readFileAt()` (past state as-of ts).
- **No-op:** `restoreFileAt()` is no-op if file didn't exist at that timestamp.
- **History derivation:** `history()` returns events; earliest event ts per path (via `createTsByPath()`) used to detect "file did not exist yet" state.

### Persistence & Keying

- **vault_id:** Stable cross-session identity (same across device restarts, link shares). Used for: URL hash key, localStorage tabs key, vaultMeta key.
- **id:** Per-session local handle (random 12-char id, new on every reopen). Used for: backend API calls, state tracking during session.
- **localStorage keys:**
  - `asp.prefs.v1`: Global prefs.
  - `asp.vaultmeta.v1`: All vault metadata (one object, keyed by vault_id).
  - `asp.tabs.<vault_id>`: Open tabs per vault.
- **No localStorage edit buffer:** Unsaved edits exist only in React state (`bufferRef`) during session. On crash, unsaved edits are lost. This is intentional (keep state-of-truth on backend).

### Multi-Selection

- **Set of paths** (`selectedPaths`).
- **Anchor:** Last plainly-clicked file (`anchorPath`), used for shift-range selection.
- **Delete:** If active file is in selection, whole selection deleted. If non-active file in selection is deleted individually, only that file removed.
- **Operations:** Rename/move/delete work on full selection (not just active file).

### Polling & Debounce

- **2s fast poll:** Active vault only, viewing editor screen. Auto-syncs, gets status, refreshes files + active file content + (every 5th tick) history.
- **10s slow poll:** Connect screen or when no active vault. Refreshes all vaults + all statuses.
- **700ms history debounce:** History re-fetched only after user stops interacting with history panel.
- **650ms save debounce:** Edits debounced; timer reset on every keystroke; cancels on vault/file switch.
- **Backoff:** No explicit backoff on backend errors; poll continues at normal interval.

### Network & Connectivity

- **Realtime push (desktop):** Engine fires change-notifier → Tauri event `vault-changed` → UI refreshes instantly. 2s poll remains as backstop.
- **Realtime push (web):** Persistent relay connection (reconnect on drop); 2s poll is backstop.
- **Single endpoint per folder:** Each folder owns ONE iroh endpoint (one device key, one socket), shared by listener (serve) and connector (connect). Neither closes the endpoint; clones and reconnects on restart.
- **Broadcast:** In-app edits (write/rename/delete/restore) broadcast authored `WireRow` to all connected peers in real time, before returning to UI.

### URL Hash Scheme

- `#<vault_id>/<file_path>` (both fully percent-encoded).
- Read at startup (after vault list refreshes) to restore selection.
- Updated on file select (replaceState, never pushes history).
- Cleared when user navigates away from any vault (back to connect screen).

### Vault Loading & Persistence

- **Startup:** `publish_loading()` (cheap) shows placeholders → `reopen_saved()` (background) opens each folder.
- **Saved list:** `~/.asp/desktop_folders.json`, JSON array of `{ path, peer? }`.
- **Stale harness:** Temp-dir vaults with `aspgui-*` basename skipped on reopen (GUI-test detritus).
- **Upstream peer:** Persisted in saved-folder entry; connector auto-reconnects on reopen.

### No Single Source of Truth for Edit Buffer

- **Intentional design:** Unsaved edits are NOT buffered in localStorage or persisted anywhere except React state during session.
- **On crash:** Unsaved edits lost.
- **On switch:** Unsaved edits flushed to backend before switching away (via `flushSave()` before `readFile()`).
- **Read-only:** Time-travel makes editor read-only; no unsaved edits possible in that mode.

### Folder Operations

- **Create directory:** `mkdir` on disk, then `capture_rescan()` to author the `Dir` row. Broadcast to peers.
- **Delete directory:** Single `deleteFile()` call deletes the directory entry + implicit children (via `asp-core` merge logic).
- **Rename directory:** Single `renameFile()` call for the dir path; implicit children remapped via `oldPath + '/'` prefix match.

### Error Handling

- **Backend failures:** Silently ignored in most cases (errors logged to console). UI remains in last-known-good state.
- **Transient errors:** Poll continues at normal interval; eventual consistency on next sync.
- **Permission errors:** If `authorize()` fails or a peer is not authorized, sync simply doesn't happen; no error surface to user.

---

## Summary: Key Method Signatures for Rust Implementation

**The gpui port should directly call these Rust methods (no Tauri command layer):**

```rust
// Lifecycle
pub fn new(identity: Identity) -> Result<DesktopEngine>
pub fn identity_ssh(&self) -> String
pub fn take_change_receiver(&self) -> Option<Receiver<String>>

// Vaults
pub fn list_vaults(&self) -> Vec<VaultInfo>
pub fn publish_loading(&self)
pub fn reopen_saved(&self) -> Result<Vec<VaultInfo>>
pub fn add_local_folder(&self, path: &Path) -> Result<VaultInfo>
pub fn remove_vault(&self, id: &str, _trash: bool) -> Result<()>
pub fn set_enabled(&self, id: &str, on: bool) -> Result<()>

// Peer connection
pub fn set_allow_connections(&self, id: &str, on: bool, auth_key: Option<&str>) -> Result<Option<String>>
pub fn clone_remote(&self, dest: &Path, ticket: &str, auth_key: Option<&str>) -> Result<VaultInfo>
pub fn sync(&self, id: &str, ticket: &str, auth_key: Option<&str>) -> Result<()>

// Files
pub fn list_files(&self, id: &str) -> Result<Vec<FileEntry>>
pub fn read_file(&self, id: &str, path: &str) -> Result<String>
pub fn write_file(&self, id: &str, path: &str, content: &str) -> Result<()>
pub fn rename_file(&self, id: &str, old: &str, new: &str) -> Result<()>
pub fn delete_file(&self, id: &str, path: &str) -> Result<()>
pub fn create_dir(&self, id: &str, path: &str) -> Result<()>

// History & time-travel
pub fn history(&self, id: &str) -> Result<Vec<HistEvent>>
pub fn read_file_at(&self, id: &str, path: &str, ts: i64) -> Result<FileAt>
pub fn restore_file_at(&self, id: &str, path: &str, ts: i64) -> Result<()>
pub fn rescan(&self, id: &str) -> Result<()>

// Snapshots
pub fn snapshot(&self, id: &str, name: &str) -> Result<String>
pub fn restore(&self, id: &str, target: &str) -> Result<()>

// Vault config
pub fn authorize(&self, id: &str, pubkey: &str) -> Result<()>
pub fn list_authorized(&self, id: &str) -> Result<Vec<String>>

// Status
pub fn status(&self, id: &str) -> Result<VaultStatus>
```

All structs (VaultInfo, VaultStatus, FileEntry, HistEvent, FileAt) are `#[derive(Clone, Serialize)]` and are the return/parameter types.
