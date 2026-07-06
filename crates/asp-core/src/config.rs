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

/// Vault-wide default commit author (`"Name <email>"`) for authored git plans
/// (git-bridge §5.1/§5.3). Consulted by `gitpush::author_plan` when the caller
/// passes no explicit author. Config-level so every surface (CLI push, interval
/// tick, desktop) agrees; the value is recorded *in* the synced plan, so reading
/// it at author time keeps synthesis deterministic.
pub const KEY_GIT_AUTHOR: &str = "git.author";
/// `interval` policy: max time (seconds) between plans while the vault has pending
/// rows before one is force-authored (git-bridge §5.3, default 4h).
pub const KEY_GIT_INTERVAL_WINDOW: &str = "git.interval.window_secs";
/// `interval` policy: quiet-period (seconds) after the last edit before a plan is
/// authored (git-bridge §5.3, default 10min).
pub const KEY_GIT_INTERVAL_QUIESCENCE: &str = "git.interval.quiescence_secs";
/// `interval` policy: dedup jitter (seconds) waited before authoring, during which
/// an equal-frontier plan from another bridge cancels ours (git-bridge §5.3).
pub const KEY_GIT_INTERVAL_JITTER: &str = "git.interval.jitter_secs";

/// `interval` policy defaults (git-bridge §5.3).
pub const DEFAULT_GIT_INTERVAL_WINDOW_SECS: i64 = 4 * 60 * 60;
pub const DEFAULT_GIT_INTERVAL_QUIESCENCE_SECS: i64 = 10 * 60;
pub const DEFAULT_GIT_INTERVAL_JITTER_SECS: i64 = 3;

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

    /// The vault-wide default git commit author (`"Name <email>"`), if set
    /// (git-bridge §5.1). `None` (or blank) falls back to `gitpush::default_author`.
    pub fn git_author(&self) -> AspResult<Option<String>> {
        Ok(self
            .store
            .get_config(KEY_GIT_AUTHOR)?
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()))
    }

    pub fn set_git_author(&self, v: &str) -> AspResult<()> {
        self.store.set_config(KEY_GIT_AUTHOR, v)
    }

    /// `interval` policy window/quiescence/jitter in seconds, config override else
    /// default (git-bridge §5.3).
    pub fn git_interval_window_secs(&self) -> AspResult<i64> {
        self.git_i64(KEY_GIT_INTERVAL_WINDOW, DEFAULT_GIT_INTERVAL_WINDOW_SECS)
    }
    pub fn git_interval_quiescence_secs(&self) -> AspResult<i64> {
        self.git_i64(KEY_GIT_INTERVAL_QUIESCENCE, DEFAULT_GIT_INTERVAL_QUIESCENCE_SECS)
    }
    pub fn git_interval_jitter_secs(&self) -> AspResult<i64> {
        self.git_i64(KEY_GIT_INTERVAL_JITTER, DEFAULT_GIT_INTERVAL_JITTER_SECS)
    }

    fn git_i64(&self, key: &str, default: i64) -> AspResult<i64> {
        Ok(self
            .store
            .get_config(key)?
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(default))
    }
}
