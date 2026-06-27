# Context Desktop — Vault Editor

A **Tauri v2** desktop app **and** a static web app: a real Markdown vault editor
over the ASP engine. One React codebase builds two ways:

- **Desktop** (`bun run tauri dev` / `tauri build`) — the frontend calls Tauri
  commands → `asp-desktop-engine` → `asp-core` (the **full node**: real folder
  I/O, a real listen/serve iroh socket, real history from the on-disk log).
- **Web** (`bun run build:web`) — the same React served as static files; the
  frontend drives the `@asp/sdk` wasm engine in a Web Worker (iroh-in-wasm),
  OPFS-persisted. The **thin node** a browser runs.

The editor never branches on platform — it talks to a single `VaultApi`
abstraction with two backends (`TauriVaultApi`, `WebVaultApi`). HARD INVARIANT:
no protocol/merge/identity logic in the app; every behavior is a call into the
real engine (native or wasm).

## What it does

- **Vaults**: open a local folder (desktop) or create a browser vault (web);
  connect to a peer by iroh ticket (+ access key); remove a vault (desktop:
  optional OS-trash).
- **Files**: wysiwyg Markdown editor (live `contentEditable` + read-only
  preview), new/rename/delete, autosave.
- **Sync**: desktop "Share" prints a real iroh ticket (listen); "Sync now"
  pushes/pulls. Web syncs outbound to a remembered peer.
- **History**: a timeline of log events (create/edit/rename/delete) with scrub,
  zoom, and point-in-time travel + "Restore here".
- **Fonts**: JetBrains Mono + Newsreader are **embedded** as base64 WOFF2 (no
  network fetch — works offline / in a sandboxed WebView).

## Layout

```
desktop/
  engine/          asp-desktop-engine — the multi-vault manager (Rust, links
                   asp-core). NO Tauri dependency → builds & is tested on Linux.
    src/lib.rs       DesktopEngine: add_local_folder, clone_remote,
                     set_allow_connections, sync, files_tree, read/write/delete/
                     rename_file, history, file_at_time, restore_file_at,
                     snapshot/restore, remove_vault, status.
    tests/editor_surface.rs   comprehensive editor-surface coverage (file CRUD,
                              rename, delete, history, PITR, remove/trash, sync).
    tests/integration.rs      two managed folders converge in-process.
  src-tauri/       the Tauri shell (commands → DesktopEngine). Excluded from
                   the cargo workspace (links system webkit).
    src/commands.rs  thin #[tauri::command] pass-throughs (no protocol logic);
                     each delegates to a free fn tested in tests.rs.
    src/commands/tests.rs   command-contract tests (every command's shape + the
                             real iroh sync through the command surface).
  src/             React + TypeScript editor (Vite).
    App.tsx          the editor (ported from the dc mockup to live engine data).
    lib/api.ts       VaultApi + TauriVaultApi.
    lib/web-api.ts   WebVaultApi (wasm worker + OPFS + localStorage).
    lib/engine-worker.ts  the multi-vault wasm Web Worker (iroh-in-wasm).
    lib/markdown.ts  the wysiwyg + preview markdown renderer.
    fonts/fonts.ts   embedded fonts (base64 WOFF2).
  fonts/            source WOFF2 (JetBrains Mono + Newsreader, latin subset).
  e2e/              Playwright web-target e2e (editor UI + wasm engine + OPFS)
                    + a browser↔native sync e2e over a local relay.
  playwright.config.ts
```

## Build

```sh
cd desktop
bun install
bun run build:web      # static web build (dist/)
bun run tauri build    # desktop app (requires the Tauri toolchain + system webkit)
```

## Test

```sh
# 1. Engine (real asp-core, in-process): file CRUD, rename, delete, history,
#    PITR, snapshot/restore, remove/trash, real iroh sync.
cargo test -p asp-desktop-engine

# 2. Tauri commands (the contract the frontend depends on): every command's
#    shape + errors + real iroh sync through the command surface.
cd desktop/src-tauri && cargo test

# 3. Web-target Playwright e2e: the real editor UI in headless Chromium against
#    the real wasm engine + OPFS (file CRUD, rename, delete, history, preview,
#    persistence across reload, remove-vault) + a browser↔native sync e2e.
cd desktop && bunx playwright test
```

The browser↔native iroh sync direction is gated by the SDK parity test
(`sdks/typescript/test/parity.test.ts`) in CI with real networking; the
native↔native path is covered exhaustively by the Rust e2e suite
(`tests/e2e/`).
