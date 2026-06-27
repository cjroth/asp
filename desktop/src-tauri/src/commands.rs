//! Tauri commands — thin pass-throughs to `asp_desktop_engine::DesktopEngine`.
//! HARD INVARIANT: no protocol logic here; every command is a call into the
//! engine (which calls into `asp-core`).
//!
//! All commands use `#[tauri::command(async)]` so they run OFF the webview's
//! main thread. The engine calls are blocking (mutex locks, log folds,
//! `capture_rescan`, `rt.block_on` for network) — on the main thread they would
//! freeze rendering/input. `(async)` dispatches each to a worker thread.

use asp_desktop_engine::{DesktopEngine, FileAt, FileEntry, HistEvent, VaultInfo, VaultStatus};
use std::path::PathBuf;
use tauri::State;

pub struct AppState {
    pub engine: DesktopEngine,
}

type R<T> = Result<T, String>;
fn e<T>(r: anyhow::Result<T>) -> R<T> {
    r.map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub fn list_vaults(state: State<AppState>) -> Vec<VaultInfo> {
    state.engine.list_vaults()
}

#[tauri::command(async)]
pub fn add_local_folder(state: State<AppState>, path: String) -> R<VaultInfo> {
    e(state.engine.add_local_folder(&PathBuf::from(path)))
}

#[tauri::command(async)]
pub fn clone_remote(state: State<AppState>, dest: String, url: String, auth_key: Option<String>) -> R<VaultInfo> {
    e(state.engine.clone_remote(&PathBuf::from(dest), &url, auth_key.as_deref()))
}

#[tauri::command(async)]
pub fn set_allow_connections(state: State<AppState>, id: String, on: bool, auth_key: Option<String>) -> R<Option<String>> {
    e(state.engine.set_allow_connections(&id, on, auth_key.as_deref()))
}

#[tauri::command(async)]
pub fn set_enabled(state: State<AppState>, id: String, on: bool) -> R<()> {
    e(state.engine.set_enabled(&id, on))
}

#[tauri::command(async)]
pub fn sync_now(state: State<AppState>, id: String, url: String, auth_key: Option<String>) -> R<()> {
    e(state.engine.sync(&id, &url, auth_key.as_deref()))
}

#[tauri::command(async)]
pub fn get_status(state: State<AppState>, id: String) -> R<VaultStatus> {
    e(state.engine.status(&id))
}

#[tauri::command(async)]
pub fn get_identity(state: State<AppState>) -> String {
    state.engine.identity_ssh()
}

#[tauri::command(async)]
pub fn authorize(state: State<AppState>, id: String, pubkey: String) -> R<()> {
    e(state.engine.authorize(&id, &pubkey))
}

#[tauri::command(async)]
pub fn list_authorized(state: State<AppState>, id: String) -> R<Vec<String>> {
    e(state.engine.list_authorized(&id))
}

#[tauri::command(async)]
pub fn create_snapshot(state: State<AppState>, id: String, name: String) -> R<String> {
    e(state.engine.snapshot(&id, &name))
}

#[tauri::command(async)]
pub fn restore(state: State<AppState>, id: String, target: String) -> R<()> {
    e(state.engine.restore(&id, &target))
}

// ---- File surface (thin pass-throughs to the engine) ----

#[tauri::command(async)]
pub fn list_files(state: State<AppState>, id: String) -> R<Vec<FileEntry>> {
    e(state.engine.list_files(&id))
}

#[tauri::command(async)]
pub fn read_file(state: State<AppState>, id: String, path: String) -> R<String> {
    e(state.engine.read_file(&id, &path))
}

#[tauri::command(async)]
pub fn write_file(state: State<AppState>, id: String, path: String, content: String) -> R<()> {
    e(state.engine.write_file(&id, &path, &content))
}

#[tauri::command(async)]
pub fn rename_file(state: State<AppState>, id: String, old: String, new: String) -> R<()> {
    e(state.engine.rename_file(&id, &old, &new))
}

#[tauri::command(async)]
pub fn delete_file(state: State<AppState>, id: String, path: String) -> R<()> {
    e(state.engine.delete_file(&id, &path))
}

#[tauri::command(async)]
pub fn history(state: State<AppState>, id: String) -> R<Vec<HistEvent>> {
    e(state.engine.history(&id))
}

#[tauri::command(async)]
pub fn read_file_at(state: State<AppState>, id: String, path: String, ts: i64) -> R<FileAt> {
    e(state.engine.read_file_at(&id, &path, ts))
}

#[tauri::command(async)]
pub fn restore_file_at(state: State<AppState>, id: String, path: String, ts: i64) -> R<()> {
    e(state.engine.restore_file_at(&id, &path, ts))
}

#[tauri::command(async)]
pub fn rescan(state: State<AppState>, id: String) -> R<()> {
    e(state.engine.rescan(&id))
}

#[tauri::command(async)]
pub fn remove_vault(state: State<AppState>, id: String, trash: bool) -> R<()> {
    e(state.engine.remove_vault(&id, trash))
}
