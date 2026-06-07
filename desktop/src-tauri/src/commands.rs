//! Tauri commands — thin pass-throughs to `asp_desktop_engine::DesktopEngine`.
//! HARD INVARIANT: no protocol logic here; every command is a call into the
//! engine (which calls into `asp-core`).

use asp_desktop_engine::{DesktopEngine, VaultInfo, VaultStatus};
use std::path::PathBuf;
use tauri::State;

pub struct AppState {
    pub engine: DesktopEngine,
}

type R<T> = Result<T, String>;
fn e<T>(r: anyhow::Result<T>) -> R<T> {
    r.map_err(|e| e.to_string())
}

#[tauri::command]
pub fn list_vaults(state: State<AppState>) -> Vec<VaultInfo> {
    state.engine.list_vaults()
}

#[tauri::command]
pub fn add_local_folder(state: State<AppState>, path: String) -> R<VaultInfo> {
    e(state.engine.add_local_folder(&PathBuf::from(path)))
}

#[tauri::command]
pub fn clone_remote(state: State<AppState>, dest: String, url: String, auth_key: Option<String>) -> R<VaultInfo> {
    e(state.engine.clone_remote(&PathBuf::from(dest), &url, auth_key.as_deref()))
}

#[tauri::command]
pub fn set_allow_connections(state: State<AppState>, id: String, on: bool, auth_key: Option<String>) -> R<Option<u16>> {
    e(state.engine.set_allow_connections(&id, on, auth_key.as_deref()))
}

#[tauri::command]
pub fn set_enabled(state: State<AppState>, id: String, on: bool) -> R<()> {
    e(state.engine.set_enabled(&id, on))
}

#[tauri::command]
pub fn sync_now(state: State<AppState>, id: String, url: String, auth_key: Option<String>) -> R<()> {
    e(state.engine.sync(&id, &url, auth_key.as_deref()))
}

#[tauri::command]
pub fn get_status(state: State<AppState>, id: String) -> R<VaultStatus> {
    e(state.engine.status(&id))
}

#[tauri::command]
pub fn get_identity(state: State<AppState>) -> String {
    state.engine.identity_ssh()
}

#[tauri::command]
pub fn authorize(state: State<AppState>, id: String, pubkey: String) -> R<()> {
    e(state.engine.authorize(&id, &pubkey))
}

#[tauri::command]
pub fn list_authorized(state: State<AppState>, id: String) -> R<Vec<String>> {
    e(state.engine.list_authorized(&id))
}

#[tauri::command]
pub fn create_snapshot(state: State<AppState>, id: String, name: String) -> R<String> {
    e(state.engine.snapshot(&id, &name))
}

#[tauri::command]
pub fn restore(state: State<AppState>, id: String, target: String) -> R<()> {
    e(state.engine.restore(&id, &target))
}
