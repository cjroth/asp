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
            // Push a `vault-changed` event to the webview the instant a peer's
            // change integrates, so the desktop UI updates in realtime (the
            // in-process analogue of the web node's live on_change callback)
            // instead of waiting for its periodic re-read.
            let handle = app.handle().clone();
            app.state::<AppState>().engine.set_change_listener(move |vault_id| {
                let _ = handle.emit("vault-changed", vault_id);
            });
            // Re-open saved folders in the BACKGROUND so the window opens instantly.
            // A folder's open includes a startup reconcile that reads every file on
            // disk — ~tens of seconds for a 28k-file vault — which used to block the
            // whole app from appearing. Folders reopen concurrently and emit a
            // realtime `vaults-changed` the instant each lands (no polling), so the
            // UI surfaces each vault as it's ready. A vault becomes shareable only
            // after its reconcile, so a clone can never contend with one mid-scan.
            let h2 = app.handle().clone();
            std::thread::spawn(move || {
                let emit_handle = h2.clone();
                h2.state::<AppState>()
                    .engine
                    .reopen_saved_streaming(move |_info| {
                        let _ = emit_handle.emit("vaults-changed", ());
                    });
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_vaults,
            commands::vaults_ready,
            commands::add_local_folder,
            commands::clone_remote,
            commands::set_allow_connections,
            commands::set_local_relay,
            commands::get_local_relay,
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
            commands::list_branches,
            commands::current_branch,
            commands::branch_graph,
            commands::create_branch,
            commands::checkout_branch,
            commands::fork_branch_at,
            commands::delete_branch,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Context Desktop");
}
