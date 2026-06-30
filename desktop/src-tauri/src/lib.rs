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
use tauri::{Emitter, Manager};

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
        .setup(|app| {
            // Re-open previously-managed folders OFF the startup critical path.
            // Each reopen runs capture_rescan (O(files) hashing), so doing it
            // synchronously before the window is built froze first paint on a large
            // saved vault. Spawn it on a background thread (the engine guards its own
            // state with a mutex, so commands that arrive meanwhile are safe), and
            // emit `vaults-ready` so the UI refreshes its list the moment the
            // reopened folders are live instead of waiting for the next poll.
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                let _ = handle.state::<AppState>().engine.reopen_saved();
                let _ = handle.emit("vaults-ready", ());
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_vaults,
            commands::vaults_ready,
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
            commands::list_files,
            commands::read_file,
            commands::write_file,
            commands::rename_file,
            commands::create_dir,
            commands::delete_file,
            commands::history,
            commands::read_file_at,
            commands::restore_file_at,
            commands::rescan,
            commands::remove_vault,
            commands::reveal_path,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Context Desktop");
}
