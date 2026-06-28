//! Thin wrapper over `asp-desktop-engine` — the *only* path to vault behavior.
//! No protocol/merge/history logic lives here (HARD INVARIANT); every method
//! forwards to one `DesktopEngine` call, which forwards to `asp-core`.

use anyhow::Result;
use asp_core::Identity;
use asp_desktop_engine::{DesktopEngine, FileAt, FileEntry, HistEvent, VaultInfo, VaultStatus};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Clone)]
pub struct Backend {
    engine: Arc<DesktopEngine>,
}

impl Backend {
    pub fn new() -> Result<Self> {
        let engine = DesktopEngine::new(load_identity())?;
        Ok(Backend {
            engine: Arc::new(engine),
        })
    }

    /// Re-open every previously-saved vault (rescanning each from disk). This can
    /// be slow for large vaults, so callers run it off the UI thread — see
    /// `AspApp::new`. Errors are non-fatal (a missing/again-unreadable vault is
    /// simply skipped by the engine).
    pub fn reopen_saved(&self) {
        let _ = self.engine.reopen_saved();
    }

    pub fn identity_ssh(&self) -> String {
        self.engine.identity_ssh()
    }

    pub fn list_vaults(&self) -> Vec<VaultInfo> {
        self.engine.list_vaults()
    }

    pub fn add_local_folder(&self, path: &Path) -> Result<VaultInfo> {
        self.engine.add_local_folder(path)
    }

    pub fn clone_remote(&self, dest: &Path, ticket: &str, key: Option<&str>) -> Result<VaultInfo> {
        self.engine.clone_remote(dest, ticket, key)
    }

    pub fn remove_vault(&self, id: &str, trash: bool) -> Result<()> {
        self.engine.remove_vault(id, trash)
    }

    pub fn status(&self, id: &str) -> Result<VaultStatus> {
        self.engine.status(id)
    }

    pub fn list_files(&self, id: &str) -> Result<Vec<FileEntry>> {
        self.engine.list_files(id)
    }

    pub fn read_file(&self, id: &str, path: &str) -> Result<String> {
        self.engine.read_file(id, path)
    }

    pub fn write_file(&self, id: &str, path: &str, content: &str) -> Result<()> {
        self.engine.write_file(id, path, content)
    }

    pub fn rename_file(&self, id: &str, old: &str, new: &str) -> Result<()> {
        self.engine.rename_file(id, old, new)
    }

    pub fn delete_file(&self, id: &str, path: &str) -> Result<()> {
        self.engine.delete_file(id, path)
    }

    pub fn create_dir(&self, id: &str, path: &str) -> Result<()> {
        self.engine.create_dir(id, path)
    }

    pub fn history(&self, id: &str) -> Result<Vec<HistEvent>> {
        self.engine.history(id)
    }

    pub fn read_file_at(&self, id: &str, path: &str, ts: i64) -> Result<FileAt> {
        self.engine.read_file_at(id, path, ts)
    }

    pub fn restore_file_at(&self, id: &str, path: &str, ts: i64) -> Result<()> {
        self.engine.restore_file_at(id, path, ts)
    }

    pub fn set_allow_connections(&self, id: &str, on: bool, key: Option<&str>) -> Result<Option<String>> {
        self.engine.set_allow_connections(id, on, key)
    }
}

/// Device-global identity at `~/.asp/id_ed25519` (shared with the CLI and the
/// Tauri shell). Same logic as `desktop/src-tauri/src/lib.rs`.
fn load_identity() -> Identity {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    let dir = PathBuf::from(home).join(".asp");
    let path = dir.join("id_ed25519");
    if let Ok(s) = std::fs::read_to_string(&path) {
        if let Ok(seed) = hex::decode(s.trim()) {
            if seed.len() == 32 {
                let mut a = [0u8; 32];
                a.copy_from_slice(&seed);
                return Identity::from_seed(&a);
            }
        }
    }
    let id = Identity::generate();
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(&path, hex::encode(id.seed()));
    let _ = std::fs::write(dir.join("id_ed25519.pub"), format!("{}\n", id.to_ssh_string()));
    id
}
