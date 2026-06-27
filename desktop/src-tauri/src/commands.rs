//! Tauri commands — thin pass-throughs to `asp_desktop_engine::DesktopEngine`.
//! HARD INVARIANT: no protocol logic here; every command is a call into the
//! engine (which calls into `asp-core`).
//!
//! Each `#[tauri::command]` is a one-line wrapper over a free function that
//! takes `&DesktopEngine` directly. The split lets the contract (the exact call
//! + return shape the frontend `api.ts` depends on) be unit-tested without a
//! Tauri runtime or a display — the test stands up a real `DesktopEngine` and
//! drives every command the way the window would.

use asp_desktop_engine::{DesktopEngine, FileAtTime, HistoryEvent, TreeNode, VaultInfo, VaultStatus};
use std::path::PathBuf;
use tauri::State;

pub struct AppState {
    pub engine: DesktopEngine,
}

type R<T> = Result<T, String>;
fn e<T>(r: anyhow::Result<T>) -> R<T> {
    r.map_err(|e| e.to_string())
}

// ---------------- free functions (the testable contract) ----------------

pub fn list_vaults_cmd(eng: &DesktopEngine) -> Vec<VaultInfo> {
    eng.list_vaults()
}
pub fn add_local_folder_cmd(eng: &DesktopEngine, path: String) -> R<VaultInfo> {
    e(eng.add_local_folder(&PathBuf::from(path)))
}
pub fn clone_remote_cmd(eng: &DesktopEngine, dest: String, ticket: String, auth_key: Option<String>) -> R<VaultInfo> {
    e(eng.clone_remote(&PathBuf::from(dest), &ticket, auth_key.as_deref()))
}
pub fn set_allow_connections_cmd(eng: &DesktopEngine, id: String, on: bool, auth_key: Option<String>) -> R<Option<String>> {
    e(eng.set_allow_connections(&id, on, auth_key.as_deref()))
}
pub fn set_enabled_cmd(eng: &DesktopEngine, id: String, on: bool) -> R<()> {
    e(eng.set_enabled(&id, on))
}
pub fn sync_now_cmd(eng: &DesktopEngine, id: String, ticket: String, auth_key: Option<String>) -> R<()> {
    e(eng.sync(&id, &ticket, auth_key.as_deref()))
}
pub fn get_status_cmd(eng: &DesktopEngine, id: String) -> R<VaultStatus> {
    e(eng.status(&id))
}
pub fn get_identity_cmd(eng: &DesktopEngine) -> String {
    eng.identity_ssh()
}
pub fn authorize_cmd(eng: &DesktopEngine, id: String, pubkey: String) -> R<()> {
    e(eng.authorize(&id, &pubkey))
}
pub fn list_authorized_cmd(eng: &DesktopEngine, id: String) -> R<Vec<String>> {
    e(eng.list_authorized(&id))
}
pub fn create_snapshot_cmd(eng: &DesktopEngine, id: String, name: String) -> R<String> {
    e(eng.snapshot(&id, &name))
}
pub fn restore_cmd(eng: &DesktopEngine, id: String, target: String) -> R<()> {
    e(eng.restore(&id, &target))
}
pub fn files_tree_cmd(eng: &DesktopEngine, id: String) -> R<Vec<TreeNode>> {
    e(eng.files_tree(&id))
}
pub fn read_file_cmd(eng: &DesktopEngine, id: String, path: String) -> R<Option<String>> {
    e(eng.read_file(&id, &path))
}
pub fn write_file_cmd(eng: &DesktopEngine, id: String, path: String, content: String) -> R<()> {
    e(eng.write_file(&id, &path, &content))
}
pub fn delete_file_cmd(eng: &DesktopEngine, id: String, path: String) -> R<()> {
    e(eng.delete_file(&id, &path))
}
pub fn rename_file_cmd(eng: &DesktopEngine, id: String, from: String, to: String) -> R<()> {
    e(eng.rename_file(&id, &from, &to))
}
pub fn new_file_cmd(eng: &DesktopEngine, id: String, name: String, content: String) -> R<String> {
    e(eng.new_file(&id, &name, &content))
}
pub fn history_cmd(eng: &DesktopEngine, id: String) -> R<Vec<HistoryEvent>> {
    e(eng.history(&id))
}
pub fn file_at_time_cmd(eng: &DesktopEngine, id: String, path: String, ts: i64) -> R<FileAtTime> {
    e(eng.file_at_time(&id, &path, ts))
}
pub fn restore_file_at_cmd(eng: &DesktopEngine, id: String, path: String, ts: i64) -> R<bool> {
    e(eng.restore_file_at(&id, &path, ts))
}
pub fn remove_vault_cmd(eng: &DesktopEngine, id: String, trash: bool) -> R<String> {
    e(eng.remove_vault(&id, trash))
}

// ---------------- tauri command wrappers ----------------
//
// One line each — the macro owns State extraction, the free function owns the
// call. Keeping them mechanical means the contract under test is exactly what
// the window runs.

#[tauri::command]
pub fn list_vaults(state: State<AppState>) -> Vec<VaultInfo> {
    list_vaults_cmd(&state.engine)
}
#[tauri::command]
pub fn add_local_folder(state: State<AppState>, path: String) -> R<VaultInfo> {
    add_local_folder_cmd(&state.engine, path)
}
#[tauri::command]
pub fn clone_remote(state: State<AppState>, dest: String, ticket: String, auth_key: Option<String>) -> R<VaultInfo> {
    clone_remote_cmd(&state.engine, dest, ticket, auth_key)
}
#[tauri::command]
pub fn set_allow_connections(state: State<AppState>, id: String, on: bool, auth_key: Option<String>) -> R<Option<String>> {
    set_allow_connections_cmd(&state.engine, id, on, auth_key)
}
#[tauri::command]
pub fn set_enabled(state: State<AppState>, id: String, on: bool) -> R<()> {
    set_enabled_cmd(&state.engine, id, on)
}
#[tauri::command]
pub fn sync_now(state: State<AppState>, id: String, ticket: String, auth_key: Option<String>) -> R<()> {
    sync_now_cmd(&state.engine, id, ticket, auth_key)
}
#[tauri::command]
pub fn get_status(state: State<AppState>, id: String) -> R<VaultStatus> {
    get_status_cmd(&state.engine, id)
}
#[tauri::command]
pub fn get_identity(state: State<AppState>) -> String {
    get_identity_cmd(&state.engine)
}
#[tauri::command]
pub fn authorize(state: State<AppState>, id: String, pubkey: String) -> R<()> {
    authorize_cmd(&state.engine, id, pubkey)
}
#[tauri::command]
pub fn list_authorized(state: State<AppState>, id: String) -> R<Vec<String>> {
    list_authorized_cmd(&state.engine, id)
}
#[tauri::command]
pub fn create_snapshot(state: State<AppState>, id: String, name: String) -> R<String> {
    create_snapshot_cmd(&state.engine, id, name)
}
#[tauri::command]
pub fn restore(state: State<AppState>, id: String, target: String) -> R<()> {
    restore_cmd(&state.engine, id, target)
}
#[tauri::command]
pub fn files_tree(state: State<AppState>, id: String) -> R<Vec<TreeNode>> {
    files_tree_cmd(&state.engine, id)
}
#[tauri::command]
pub fn read_file(state: State<AppState>, id: String, path: String) -> R<Option<String>> {
    read_file_cmd(&state.engine, id, path)
}
#[tauri::command]
pub fn write_file(state: State<AppState>, id: String, path: String, content: String) -> R<()> {
    write_file_cmd(&state.engine, id, path, content)
}
#[tauri::command]
pub fn delete_file(state: State<AppState>, id: String, path: String) -> R<()> {
    delete_file_cmd(&state.engine, id, path)
}
#[tauri::command]
pub fn rename_file(state: State<AppState>, id: String, from: String, to: String) -> R<()> {
    rename_file_cmd(&state.engine, id, from, to)
}
#[tauri::command]
pub fn new_file(state: State<AppState>, id: String, name: String, content: String) -> R<String> {
    new_file_cmd(&state.engine, id, name, content)
}
#[tauri::command]
pub fn history(state: State<AppState>, id: String) -> R<Vec<HistoryEvent>> {
    history_cmd(&state.engine, id)
}
#[tauri::command]
pub fn file_at_time(state: State<AppState>, id: String, path: String, ts: i64) -> R<FileAtTime> {
    file_at_time_cmd(&state.engine, id, path, ts)
}
#[tauri::command]
pub fn restore_file_at(state: State<AppState>, id: String, path: String, ts: i64) -> R<bool> {
    restore_file_at_cmd(&state.engine, id, path, ts)
}
#[tauri::command]
pub fn remove_vault(state: State<AppState>, id: String, trash: bool) -> R<String> {
    remove_vault_cmd(&state.engine, id, trash)
}

#[cfg(test)]
mod tests;
