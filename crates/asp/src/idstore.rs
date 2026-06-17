//! Node identity (§Data model, §Security). The keypair is **not** in the vault DB
//! and is **never synced**. By default it lives device-globally at
//! `$ASP_HOME/id_ed25519` (default `~/.asp/`), so one device identity serves every
//! vault it joins and survives deletion of any vault's `.asp/`.
//!
//! A vault can instead opt into its **own** key at `<vault>/.asp/id_ed25519` (via
//! `--no-home-key` on any command that opens it). That path is inside the always-ignored `.asp/`, so it
//! stays local and unsynced. The vault-local key, when present, always wins — its
//! mere existence is the signal — which lets several nodes run on one machine with
//! distinct identities (no `ASP_HOME` juggling). Stored as a 64-hex-char ed25519
//! seed with a sibling `.pub` OpenSSH line for sharing.

use anyhow::{anyhow, Context, Result};
use asp_core::Identity;
use std::fs;
use std::path::{Path, PathBuf};

/// Resolve the device home dir: `$ASP_HOME`, else `$HOME/.asp`.
pub fn home_dir() -> PathBuf {
    if let Ok(h) = std::env::var("ASP_HOME") {
        return PathBuf::from(h);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    Path::new(&home).join(".asp")
}

/// The device-global key path (`$ASP_HOME/id_ed25519`).
fn device_key_dir() -> PathBuf {
    home_dir()
}

/// The vault-local key dir (`<vault>/.asp`), inside the always-ignored private dir.
fn vault_key_dir(vault_root: &Path) -> PathBuf {
    vault_root.join(".asp")
}

/// Resolve the identity for a vault. A vault-local key (`<root>/.asp/id_ed25519`)
/// always wins if present. Otherwise, when `no_home_key` is set (`--no-home-key`),
/// generate a fresh vault-local key; else fall back to the device-global home key
/// (generating it on first use). `no_home_key` is a no-op once a vault-local key
/// exists — resolution stays stable across later commands.
pub fn load_or_generate(vault_root: &Path, no_home_key: bool) -> Result<Identity> {
    let local_dir = vault_key_dir(vault_root);
    if let Some(id) = load_at(&local_dir)? {
        return Ok(id);
    }
    if no_home_key {
        let id = Identity::generate();
        persist_at(&local_dir, &id)?;
        return Ok(id);
    }
    let device_dir = device_key_dir();
    if let Some(id) = load_at(&device_dir)? {
        return Ok(id);
    }
    let id = Identity::generate();
    persist_at(&device_dir, &id)?;
    Ok(id)
}

/// Load an identity from `<dir>/id_ed25519` if it exists.
fn load_at(dir: &Path) -> Result<Option<Identity>> {
    let path = dir.join("id_ed25519");
    match fs::read_to_string(&path) {
        Ok(s) => {
            let seed = parse_seed(s.trim()).ok_or_else(|| anyhow!("malformed key at {}", path.display()))?;
            Ok(Some(Identity::from_seed(&seed)))
        }
        Err(_) => Ok(None),
    }
}

fn persist_at(dir: &Path, id: &Identity) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    fs::write(dir.join("id_ed25519"), hex::encode(id.seed()))?;
    fs::write(dir.join("id_ed25519.pub"), format!("{}\n", id.to_ssh_string()))?;
    Ok(())
}

fn parse_seed(s: &str) -> Option<[u8; 32]> {
    let v = hex::decode(s).ok()?;
    if v.len() != 32 {
        return None;
    }
    let mut a = [0u8; 32];
    a.copy_from_slice(&v);
    Some(a)
}

/// Print (and return) the vault's effective public key in OpenSSH format.
pub fn public_line(vault_root: &Path, no_home_key: bool) -> Result<String> {
    Ok(load_or_generate(vault_root, no_home_key)?.to_ssh_string())
}
