# Context Desktop

A background desktop app (Tauri v2) for managing and syncing context folders —
the spec's **native full-node** surface. It runs **one `asp-core` engine instance
per enabled folder** (the in-process equivalent of one `asp watch [--listen]` per
folder), linking `asp-core` directly at the full-node profile — architecturally a
sibling of the `asp` CLI, **not** a consumer of the wasm SDK and **not** an `asp`
subprocess.

## Layout

```
desktop/
  engine/          asp-desktop-engine — the multi-vault manager (Rust, links asp-core).
                   NO Tauri dependency → builds & is tested on plain Linux.
    src/lib.rs       DesktopEngine: add_local_folder, clone_remote,
                     set_allow_connections (per-folder listen socket), sync,
                     authorize/list_authorized, snapshot/restore, status.
    tests/integration.rs   two managed folders converge in-process (real sync).
  src-tauri/       the Tauri shell (commands → DesktopEngine). Excluded from the
                   cargo workspace (links system webkit; built with the Tauri toolchain).
    src/commands.rs  thin #[tauri::command] pass-throughs (no protocol logic).
    src/lib.rs       Builder, device identity (~/.asp/id_ed25519, shared with the CLI).
  src/             React + TypeScript frontend (Vite) calling the commands.
```

## HARD INVARIANT

No protocol/merge/identity/auth/history logic in the app. Every behavior is a
call into `asp-desktop-engine` → `asp-core`. Any behavioral difference from the
`asp` CLI is a bug.

## What's tested

The **engine crate is part of the workspace** and its integration tests run under
`cargo test --workspace`: two managed folders, one with "allow connections" on,
converge in-process through the same `asp-core` net driver + `Session` as the CLI
(`desktop/engine/tests/integration.rs`). The Tauri/React shell is GUI glue built
with the Tauri toolchain (system webkit + a display), so it is not exercised in
the headless CI — its contract is the thin command layer over the tested engine.

## Build (requires the Tauri toolchain + system webkit)

```sh
cd desktop
bun install
bun run tauri dev      # or: bun run tauri build
```
