//! Context Desktop — Tauri shell. Initializes one `DesktopEngine` (which runs one
//! `asp-core` engine per enabled folder), registers the command surface, and
//! keeps syncing in the background when the window is closed (hide to tray). The
//! engine links `asp-core` natively — architecturally a sibling of the `asp` CLI,
//! not a consumer of the wasm SDK.

mod commands;

use asp_core::Identity;
use asp_desktop_engine::DesktopEngine;
use commands::AppState;
use std::fs;
use std::path::PathBuf;

/// Device-global identity at `~/.asp/id_ed25519` (shared with the CLI).
fn load_identity() -> Identity {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    let dir = PathBuf::from(home).join(".asp");
    let path = dir.join("id_ed25519");
    if let Ok(s) = fs::read_to_string(&path) {
        if let Ok(seed) = hex::decode(s.trim()) {
            if seed.len() == 32 {
                let mut a = [0u8; 32];
                a.copy_from_slice(&seed);
                return Identity::from_seed(&a);
            }
        }
    }
    let id = Identity::generate();
    let _ = fs::create_dir_all(&dir);
    let _ = fs::write(&path, hex::encode(id.seed()));
    let _ = fs::write(dir.join("id_ed25519.pub"), format!("{}\n", id.to_ssh_string()));
    id
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let engine = DesktopEngine::new(load_identity()).expect("init desktop engine");
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .manage(AppState { engine })
        .invoke_handler(tauri::generate_handler![
            commands::list_vaults,
            commands::add_local_folder,
            commands::clone_remote,
            commands::set_allow_connections,
            commands::set_enabled,
            commands::sync_now,
            commands::get_status,
            commands::get_identity,
            commands::authorize,
            commands::list_authorized,
            commands::create_snapshot,
            commands::restore,
            commands::files_tree,
            commands::read_file,
            commands::write_file,
            commands::delete_file,
            commands::rename_file,
            commands::new_file,
            commands::history,
            commands::file_at_time,
            commands::restore_file_at,
            commands::remove_vault,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Context Desktop");
}
