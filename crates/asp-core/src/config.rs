//! Synced vault config (§Data model). Most keys are ordinary folded settings;
//! the fold-parameterizing key `tiebreak_key` is **genesis-immutable** — set once
//! at `init` and never changed on a populated vault (avoiding the chicken-and-egg
//! where synced config would need an order to converge but *is* what defines the
//! order). Deployment knobs (flag > env > config) are resolved by the CLI driver;
//! their persisted form is here.

use crate::error::{AspError, AspResult};
use crate::sqlite::SqliteStore;

pub const KEY_TIEBREAK: &str = "tiebreak_key";
pub const KEY_VAULT_ID: &str = "vault_id";
pub const KEY_DEFAULT_KEY_TTL: &str = "default_key_ttl";
pub const KEY_DEBOUNCE_MS: &str = "debounce_ms";

/// Typed view over the `config` table.
pub struct VaultConfig<'a> {
    store: &'a SqliteStore,
}

impl<'a> VaultConfig<'a> {
    pub fn new(store: &'a SqliteStore) -> Self {
        VaultConfig { store }
    }

    /// Initialize genesis-immutable + default config at `init`. `tiebreak_key` is
    /// fixed to `lamport` in v1 and may never change once rows exist.
    pub fn init_genesis(&self, vault_id: &str) -> AspResult<()> {
        if self.store.get_config(KEY_TIEBREAK)?.is_none() {
            self.store.set_config(KEY_TIEBREAK, "lamport")?;
        }
        if self.store.get_config(KEY_VAULT_ID)?.is_none() {
            self.store.set_config(KEY_VAULT_ID, vault_id)?;
        }
        Ok(())
    }

    pub fn tiebreak_key(&self) -> AspResult<String> {
        Ok(self.store.get_config(KEY_TIEBREAK)?.unwrap_or_else(|| "lamport".into()))
    }

    pub fn vault_id(&self) -> AspResult<Option<String>> {
        self.store.get_config(KEY_VAULT_ID)
    }

    /// Reject mutating a fold-parameterizing key on a populated vault.
    pub fn set_tiebreak(&self, value: &str) -> AspResult<()> {
        if self.store.row_count()? > 0 {
            return Err(AspError::Invalid(
                "tiebreak_key is genesis-immutable and cannot change on a populated vault".into(),
            ));
        }
        self.store.set_config(KEY_TIEBREAK, value)
    }

    pub fn default_key_ttl(&self) -> AspResult<String> {
        Ok(self.store.get_config(KEY_DEFAULT_KEY_TTL)?.unwrap_or_else(|| "90d".into()))
    }

    pub fn set_default_key_ttl(&self, v: &str) -> AspResult<()> {
        self.store.set_config(KEY_DEFAULT_KEY_TTL, v)
    }

    pub fn debounce_ms(&self) -> AspResult<u64> {
        Ok(self
            .store
            .get_config(KEY_DEBOUNCE_MS)?
            .and_then(|s| s.parse().ok())
            .unwrap_or(400))
    }
}
