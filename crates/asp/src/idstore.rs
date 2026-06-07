//! Device-global identity (§Data model, §Security). The node's keypair is **not**
//! in the vault DB and is **never synced**: it lives device-globally at
//! `$ASP_HOME/id_ed25519` (default `~/.asp/`), so one device identity serves
//! every vault it joins and survives deletion of any vault's `.asp/`. Stored as a
//! 64-hex-char ed25519 seed with a sibling `.pub` OpenSSH line for sharing.

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

fn key_path() -> PathBuf {
    home_dir().join("id_ed25519")
}

/// Load the device identity, generating + persisting it on first use.
pub fn load_or_generate() -> Result<Identity> {
    let path = key_path();
    if let Ok(s) = fs::read_to_string(&path) {
        let seed = parse_seed(s.trim()).ok_or_else(|| anyhow!("malformed key at {}", path.display()))?;
        return Ok(Identity::from_seed(&seed));
    }
    let id = Identity::generate();
    persist(&id)?;
    Ok(id)
}

fn persist(id: &Identity) -> Result<()> {
    let dir = home_dir();
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    fs::write(key_path(), hex::encode(id.seed()))?;
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

/// Print (and return) the device's public key in OpenSSH format.
pub fn public_line() -> Result<String> {
    Ok(load_or_generate()?.to_ssh_string())
}
