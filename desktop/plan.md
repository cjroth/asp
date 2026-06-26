# Replace the Context Desktop UI with the "Vault Editor" design

## Context

`/home/chris/asp/desktop` is the ASP **Context Desktop** app — a Tauri v2 shell whose
React frontend (`src/App.tsx`, ~97 lines) is a bare placeholder: a device key, an
"Add folder" button, a clone form, and a flat vault list. The user designed a full
replacement in Claude Design ("Asp browser file editor" → `Vault Editor.dc.html`) and
wants it to **become** the desktop app, wired to the real backend.

The design is a complete two-screen editor:
- **Connect screen** — your vaults, "Open a folder" / "Connect with a code".
- **Editor screen** — sidebar (vault switcher + file tree + footer), a live WYSIWYG
  Markdown editor, and a bottom **History time-travel scrubber** (pan/zoom/scrub the
  log, view the vault read-only as-of any past moment, restore a version).
- Modals: share-vault (ticket + optional access key), remove-vault (+ trash toggle),
  file/folder + vault context menus, inline rename.

**Key architecture finding:** `asp-core` already exposes every primitive needed —
`record_write`, `record_remove`, `record_rename` (engine.rs:146/201/226), `live_files`
(sqlite.rs:202), `all_rows` (log rows with wall-clock `ts`, sqlite.rs:119), and
`state_as_of(t) -> BTreeMap<path,bytes>` (engine.rs:713, already `pub`). Materialize
writes files through to disk. **So no `asp-core` changes are required.** All new code
lives in the layers the HARD INVARIANT permits — the `asp-desktop-engine` forwarder,
the Tauri command pass-throughs, and the React app. No protocol/merge/history logic
enters the engine crate or the app; both only call into `asp-core`.

User directive: **do not stop until it is 100% working, fully e2e-tested, and verified
bug-free.** Plan therefore includes a rigorous verification pass that iterates to green.

## Backend additions

### `desktop/engine/src/lib.rs` (`asp-desktop-engine`) — thin forwarders to asp-core
Each new method locks the target `Folder.engine` and calls one `asp-core` primitive:
- `list_files(id) -> Vec<FileEntry>` — from `store.live_files()`; `FileEntry { path, file_id, is_dir (merge_class==Dir), merge_class }` (new `#[derive(Serialize)]` struct).
- `read_file(id, path) -> String` — `std::fs::read(folder.path.join(path))` (disk is the materialized truth), lossy-UTF8.
- `write_file(id, path, content)` — `engine.record_write(path, content.as_bytes())`.
- `rename_file(id, old, new)` — `engine.record_rename(old, new)`.
- `delete_file(id, path)` — `engine.record_remove(path)`.
- `history(id) -> Vec<HistEvent>` — `store.all_rows()` mapped to `HistEvent { id, ts, lamport, kind (string), path }`; resolve `path` for `Edit`/`Delete` rows by tracking latest path per `file_id` while scanning in fold order (create/rename carry `path`).
- `read_file_at(id, path, ts) -> FileAt { exists, content }` — `engine.state_as_of(ts)?.get(path)`.
- `restore_file_at(id, path, ts)` — read at `ts`, then `record_write` (per-file "Restore this version"; matches the design, which restores only the selected file).
- `rescan(id)` — `engine.capture_rescan()` (manual refresh after external edits).
- `remove_vault(id, trash)` — abort listener, drop from `folders` map, update the persisted folder list. `trash=true` is recorded but OS-trash deletion is **deferred** (documented TODO — never `remove_dir_all` without real trash semantics).
- Persistence (allowed "small app config"): managed folder paths in `~/.asp/desktop_folders.json`; `add_local_folder`/`clone_remote` append, `remove_vault` removes, new `reopen_saved()` re-adds them on startup so vaults survive restarts.
- Add `last_ts: Option<i64>` to `VaultStatus` (max `all_rows().ts`) for the "lastSync"/"x ago" labels.

### `desktop/src-tauri/src/commands.rs` + `lib.rs`
Add `#[tauri::command]` pass-throughs for each new engine method (same thin style as the existing ones) and register them in the `generate_handler!` list. Call `engine.reopen_saved()` in `run()` after constructing the engine.

## Frontend (replace `src/App.tsx` entirely)

Platform is always Desktop (Tauri). Use the **native** folder dialog
(`@tauri-apps/plugin-dialog` `open({directory:true})`, already a dep) for "Open a folder"
and clone-destination — replacing the design's synthetic in-app folder chooser (the one
intentional deviation: native pickers are correct for a desktop app). All other screens,
modals, and the history scrubber are ported faithfully.

New modules under `desktop/src/`:
- `lib/api.ts` — extend with the new commands + `FileEntry`/`HistEvent`/`FileAt` types.
- `vault/types.ts`, `vault/icons.tsx` — shared types + the design's inline SVGs.
- `vault/markdown.ts` — port `renderLiveHtml`, `inline`, `readLive`, `caretOffset`/`setCaret`/`placeInNode` (the contentEditable WYSIWYG: hidden `cm-mark` syntax spans, one `<div>` per source line, headings/lists/tasks/code/links).
- `vault/tree.ts` — build a nested tree from the flat `FileEntry[]`; flatten to indented rows honoring `expanded`; path remap helpers for rename/delete.
- `vault/history.ts` — track geometry (`toPct`, `STEPS`, `chooseStep`, `clampView`, `zoomAround`, `fmtFull`/`fmtTick`) and event/tick derivation from backend `history()`; `resolveAt` via `read_file_at`.
- `vault/ConnectScreen.tsx`, `vault/EditorScreen.tsx`, `vault/Sidebar.tsx`, `vault/HistoryTrack.tsx`, `vault/modals/{ShareModal,RemoveModal,ContextMenus}.tsx`.
- `App.tsx` — top-level screen/selection state, data loading, all handlers, status polling (peers/last_ts pulse). Source of truth is the vault: select → `read_file`; edit → debounced `write_file`; no localStorage edit buffer. Persist only UI prefs (selected path, expanded, accent/font props) keyed by **stable `vault_id`**.
- `styles.css` + `index.html` — port the design's `<style>` block (cm-* classes, scrollbars, keyframes) and Google Fonts (JetBrains Mono, Newsreader).

Wiring of design seams → real backend:
- "Open a folder" → native dialog → `addLocalFolder`. "Connect with a code" → dialog (dest) + ticket + key → `cloneRemote`. Share → `setAllowConnections(on, authKey)` returns the ticket = share code. Remove → `removeVault(id, trash)`. New file / rename / delete / type → `writeFile`/`renameFile`/`deleteFile`. History track ← `history()`; time-travel read ← `readFileAt`; "Restore this version" → `restoreFileAt`.
- Props from the design (`accentColor`, `editorFont`, `writingColumn`) become in-app settings (persisted); `platform` is fixed to Desktop.

## Verification (iterate until all green)

1. **Engine e2e (runs on plain Linux, the real backend contract):** extend
   `desktop/engine/tests/integration.rs` with a test that, on a managed folder, exercises
   `list_files` (tree), `write_file`↔`read_file` round-trip, new file, `rename_file`
   (path remap), `delete_file`, `history` (events carry `ts`/`kind`/`path`), time-travel
   (`read_file_at` at a past `ts` differs from live; `restore_file_at` brings it back),
   and `reopen_saved` persistence. Run `cargo test -p asp-desktop-engine` (workspace).
2. **Rust shell builds:** `cargo build` the `context-desktop` crate (`cargo check` if the
   system-webkit/Tauri toolchain is unavailable headlessly — report clearly either way).
3. **Frontend:** `bun install`; `tsc --noEmit`; `vite build` — must compile clean.
4. **Frontend unit tests (vitest):** pure-logic coverage for `markdown.ts` (heading/
   list/task/link/code round-trip + caret offset), `history.ts` (`toPct`/`clampView`/
   `chooseStep`/zoom), and `tree.ts` (build/flatten/remap).
5. **GUI smoke:** if a display + webview are available, `bun run tauri dev` and verify
   the connect→editor→edit→history flow; otherwise rely on (1)–(4) and document the
   headless GUI limitation explicitly. The Tauri/React shell's contract is the thin
   command layer over the engine, which (1)–(2) fully cover.

I will not stop until 1–4 are green (and 5 where the environment permits), then report
exactly what was run, what passed, and any environment-bound caveats.

---

## Addendum — live peer sync (implemented + tested)

The persistent peer connection the design implies is now wired, using existing
`asp-core` primitives (no protocol logic added to the app/engine):
- `set_allow_connections` already runs a standing `iroh_net::serve` listener.
- `clone_remote` now auto-opens a **persistent reconnecting connector**
  (`iroh_net::connect`, `oneshot=false`) to its source and persists the peer
  ticket, so a cloned vault stays live-synced (and reconnects on restart via
  `reopen_saved`). Each folder also runs `net::spawn_watcher` for external edits.
- In-app edits (`write_file`/`rename_file`/`delete_file`/`restore_file_at`) now
  **broadcast** their authored `WireRow` (`Msg::Push`) to the folder's live
  `conns`, so connected peers receive edits in real time.

Verified by `live_push_propagates_edits_without_explicit_sync`: two engines, A
shares + B clones; A's edit reaches B and B's edit reaches A with **no `sync`
call** — bidirectional live convergence. The existing converge/file/reopen tests
still pass (the watcher never authors spurious rows because `record_*`
materializes before the debounced capture runs).

### Single shared endpoint per folder (no dual-key caveat)

Each folder owns **one** long-lived iroh endpoint (one device key, one socket),
shared by the listener (`serve`) and the connector (`connect`) as clones — exactly
like the CLI's single `ep` per `asp watch` (`server_ep`/`dial_ep`). `clone_remote`
keeps its bootstrap endpoint for the connector; `set_allow_connections` reuses the
folder's endpoint (binding lazily only if none exists); one-shot `sync` reuses it
too. Neither `serve` nor `connect` closes the endpoint, so the two roles coexist.
Verified: in `live_push_*`, node B is simultaneously a connector (to A) and a
listener on its one endpoint, and bidirectional live push still works.
