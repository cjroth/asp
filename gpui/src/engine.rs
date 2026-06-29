//! (dead_code allowed: thin engine API surface kept for later wiring)
#![allow(dead_code)]
//! Thin wrapper over `asp_desktop_engine::DesktopEngine` — the same native
//! backend the Tauri shell uses (links `asp-core` directly). The gpui app calls
//! these instead of Tauri commands. No protocol/merge/history logic lives here.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use asp_core::Identity;
use asp_desktop_engine::{DesktopEngine, FileAt, FileEntry, HistEvent, VaultInfo, VaultStatus};

/// Device-global identity at `~/.asp/id_ed25519` (shared with the CLI + Tauri
/// shell). Replicated from `desktop/src-tauri/src/lib.rs` so the gpui app shares
/// the same device key.
pub fn load_identity() -> Identity {
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

/// The app's handle to the native backend.
pub struct Engine {
    inner: DesktopEngine,
}

impl Engine {
    /// Construct with the device identity and re-open previously-managed folders.
    pub fn new() -> Result<Self> {
        let inner = DesktopEngine::new(load_identity())?;
        let _ = inner.reopen_saved();
        Ok(Self { inner })
    }

    /// Construct with an explicit identity (tests / deterministic peers).
    pub fn with_identity(identity: Identity) -> Result<Self> {
        Ok(Self { inner: DesktopEngine::new(identity)? })
    }

    pub fn identity_ssh(&self) -> String {
        self.inner.identity_ssh()
    }

    // -- vaults --
    pub fn list_vaults(&self) -> Vec<VaultInfo> {
        self.inner.list_vaults()
    }
    pub fn add_local_folder(&self, path: &Path) -> Result<VaultInfo> {
        self.inner.add_local_folder(path)
    }
    pub fn clone_remote(&self, dest: &Path, ticket: &str, auth_key: Option<&str>) -> Result<VaultInfo> {
        self.inner.clone_remote(dest, ticket, auth_key)
    }
    pub fn remove_vault(&self, id: &str, trash: bool) -> Result<()> {
        self.inner.remove_vault(id, trash)
    }
    pub fn set_allow_connections(&self, id: &str, on: bool, key: Option<&str>) -> Result<Option<String>> {
        self.inner.set_allow_connections(id, on, key)
    }
    pub fn status(&self, id: &str) -> Result<VaultStatus> {
        self.inner.status(id)
    }

    // -- files --
    pub fn list_files(&self, id: &str) -> Result<Vec<FileEntry>> {
        self.inner.list_files(id)
    }
    pub fn read_file(&self, id: &str, path: &str) -> Result<String> {
        self.inner.read_file(id, path)
    }
    pub fn write_file(&self, id: &str, path: &str, content: &str) -> Result<()> {
        self.inner.write_file(id, path, content)
    }
    pub fn rename_file(&self, id: &str, old: &str, new: &str) -> Result<()> {
        self.inner.rename_file(id, old, new)
    }
    pub fn delete_file(&self, id: &str, path: &str) -> Result<()> {
        self.inner.delete_file(id, path)
    }
    pub fn create_dir(&self, id: &str, path: &str) -> Result<()> {
        self.inner.create_dir(id, path)
    }

    // -- history / time-travel --
    pub fn history(&self, id: &str) -> Result<Vec<HistEvent>> {
        self.inner.history(id)
    }
    pub fn read_file_at(&self, id: &str, path: &str, ts: i64) -> Result<FileAt> {
        self.inner.read_file_at(id, path, ts)
    }
    pub fn restore_file_at(&self, id: &str, path: &str, ts: i64) -> Result<()> {
        self.inner.restore_file_at(id, path, ts)
    }
    pub fn rescan(&self, id: &str) -> Result<()> {
        self.inner.rescan(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// End-to-end wiring test against the real backend: open a folder, then
    /// exercise list/read/write/rename/delete/history/time-travel. Proves the
    /// gpui app's engine contract (the desktop engine itself is separately
    /// tested under desktop/engine/tests).
    #[test]
    fn engine_roundtrip_on_a_real_vault() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("README.md"), b"# Hello\n").unwrap();
        std::fs::create_dir(dir.path().join("notes")).unwrap();
        std::fs::write(dir.path().join("notes").join("a.md"), b"alpha\n").unwrap();

        let eng = Engine::with_identity(Identity::from_seed(&[42u8; 32])).unwrap();
        let info = eng.add_local_folder(dir.path()).unwrap();
        let id = info.id.clone();

        // list_files sees the seeded files (+ the implied dir).
        let files = eng.list_files(&id).unwrap();
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert!(paths.contains(&"README.md"), "paths={paths:?}");
        assert!(paths.iter().any(|p| p.starts_with("notes")));

        // read round-trips the materialized bytes.
        assert_eq!(eng.read_file(&id, "README.md").unwrap(), "# Hello\n");

        // write a new file, then read it back.
        eng.write_file(&id, "fresh.md", "brand new\n").unwrap();
        assert_eq!(eng.read_file(&id, "fresh.md").unwrap(), "brand new\n");

        // rename remaps on disk + log.
        eng.rename_file(&id, "fresh.md", "renamed.md").unwrap();
        assert!(eng.read_file(&id, "fresh.md").is_err());
        assert_eq!(eng.read_file(&id, "renamed.md").unwrap(), "brand new\n");

        // edit README, capture a pre-edit timestamp, then time-travel. History
        // `ts` is unix-SECONDS, so the edit must land in a later second than the
        // seeded rows for time-travel to distinguish pre/post state.
        let pre = eng
            .history(&id)
            .unwrap()
            .iter()
            .map(|e| e.ts)
            .max()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        eng.write_file(&id, "README.md", "# Hello\nmore\n").unwrap();
        assert_eq!(eng.read_file(&id, "README.md").unwrap(), "# Hello\nmore\n");

        // history records events with kinds + paths.
        let hist = eng.history(&id).unwrap();
        assert!(hist.iter().any(|e| e.path == "README.md"));
        assert!(hist.iter().any(|e| e.kind == "edit" || e.kind == "create"));

        // read_file_at(pre) returns the pre-edit content (or at least differs
        // from live once edits exist).
        let at = eng.read_file_at(&id, "README.md", pre).unwrap();
        assert!(at.exists);
        assert_eq!(at.content, "# Hello\n");

        // restore brings the old version back as a new edit.
        eng.restore_file_at(&id, "README.md", pre).unwrap();
        assert_eq!(eng.read_file(&id, "README.md").unwrap(), "# Hello\n");

        // delete removes the file from the live set.
        eng.delete_file(&id, "renamed.md").unwrap();
        let files2 = eng.list_files(&id).unwrap();
        assert!(!files2.iter().any(|f| f.path == "renamed.md"));
    }
}
