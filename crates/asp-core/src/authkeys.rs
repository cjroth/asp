//! The node-local admission set (§Security). csp kept this in a
//! `.context/authorized_keys` **file**; ASP moves it into the `authorized_keys`
//! **SQLite table** (storage change, not policy change) — same OpenSSH key text,
//! same per-key expiry semantics, now transacted and queryable. This module is
//! I/O-free string/row logic; the actual table lives in [`crate::store`].
//!
//! Admit a peer iff: `never=1` OR (`expires_at IS NULL` — pre-migration grace,
//! never silently rejected) OR `now_unix < expires_at`.

use crate::identity::parse_ssh_pubkey;
use crate::order::NodeId;

const SECS_PER_DAY: u64 = 86_400;
const NEVER: &str = "never";

/// Per-connection context a listener uses to decide admission (§Security).
#[derive(Clone)]
pub struct AdmitCtx {
    pub no_tofu: bool,
    /// A valid auth key was presented at the WebSocket upgrade.
    pub auth_key_ok: bool,
    /// An auth key is configured on this listener (implicitly disables TOFU).
    pub auth_key_configured: bool,
    pub default_ttl_days: u64,
    pub now_unix: u64,
}

/// The admission decision — pure logic shared by the native and wasm engines.
pub enum AdmitDecision {
    /// Already enrolled and currently valid.
    Admit,
    /// Insert/refresh the peer's row with `source` and a fresh default TTL.
    Insert(&'static str),
    Deny(String),
}

/// Decide whether to admit `peer` given its existing row (if any) and whether the
/// admission set is currently empty. The load-bearing trust gate (§Security):
/// enrolled+valid → admit; auth-key present → enroll/refresh; empty set + TOFU →
/// trust-on-first-use; otherwise deny.
pub fn decide_admission(existing: Option<&AuthKey>, set_empty: bool, ctx: &AdmitCtx) -> AdmitDecision {
    if let Some(k) = existing {
        if k.admissible(ctx.now_unix) {
            return AdmitDecision::Admit;
        }
        // Expired: only an auth-key re-enrollment refreshes the TTL.
        if ctx.auth_key_ok {
            return AdmitDecision::Insert("enroll");
        }
        return AdmitDecision::Deny("key expired".into());
    }
    // Not enrolled. Auth-key enrollment is the front door for fresh peers.
    if ctx.auth_key_ok {
        return AdmitDecision::Insert("enroll");
    }
    // TOFU — only while the set is empty, no auth key configured, not disabled.
    if !ctx.no_tofu && !ctx.auth_key_configured && set_empty {
        return AdmitDecision::Insert("tofu");
    }
    AdmitDecision::Deny("not authorized".into())
}

/// One admission-set row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthKey {
    /// Full OpenSSH line: `ssh-ed25519 <base64> [comment]`.
    pub ssh_pubkey: String,
    /// ed25519 pubkey hex — the admission identity (a peer's `site_id`).
    pub node_id: String,
    /// Absolute UTC expiry (unix seconds). `None` = unset (apply default).
    pub expires_at: Option<u64>,
    /// 1 = explicit opt-out: never expires, never rewritten by migration.
    pub never: bool,
    pub added_at: u64,
    /// `init` | `env` | `cli` | `tofu` | `enroll`.
    pub source: String,
}

impl AuthKey {
    /// Build a row from an OpenSSH key line, resolving the NodeId.
    pub fn from_ssh(ssh_line: &str, expires_at: Option<u64>, never: bool, added_at: u64, source: &str) -> Option<AuthKey> {
        let node = parse_ssh_pubkey(ssh_line.trim())?;
        Some(AuthKey {
            ssh_pubkey: ssh_line.trim().to_string(),
            node_id: node.to_hex(),
            expires_at,
            never,
            added_at,
            source: source.to_string(),
        })
    }

    pub fn node(&self) -> Option<NodeId> {
        NodeId::from_hex(&self.node_id)
    }

    /// Admission test at `now_unix`.
    pub fn admissible(&self, now_unix: u64) -> bool {
        if self.never {
            return true;
        }
        match self.expires_at {
            None => true,             // unset → pre-migration grace
            Some(t) => now_unix < t,  // absolute expiry
        }
    }
}

/// `now_unix + ttl_days*86400`, rounded UP to the next UTC midnight so the
/// recorded expiry lands on a clean date (clock skew irrelevant at day grain).
pub fn expiry_from_ttl_days(now_unix: u64, ttl_days: u64) -> u64 {
    let target = now_unix.saturating_add(ttl_days * SECS_PER_DAY);
    (target / SECS_PER_DAY + 1) * SECS_PER_DAY
}

/// Parse `90d`, `1y`, `12w`, `30`, or `never`. `never` → `None`.
pub fn parse_duration_days(s: &str) -> Option<u64> {
    let s = s.trim().to_ascii_lowercase();
    if s == NEVER || s == "0" {
        return None;
    }
    if let Some(n) = s.strip_suffix('d') {
        return n.parse::<u64>().ok();
    }
    if let Some(n) = s.strip_suffix('w') {
        return n.parse::<u64>().ok().map(|w| w * 7);
    }
    if let Some(n) = s.strip_suffix('y') {
        return n.parse::<u64>().ok().map(|y| y * 365);
    }
    s.parse::<u64>().ok()
}

/// A TTL spec resolved against `now`: `never` → opt-out; `Nd` → absolute expiry.
pub enum TtlSpec {
    Never,
    Days(u64),
}

pub fn parse_ttl(s: &str) -> Option<TtlSpec> {
    if s.trim().eq_ignore_ascii_case(NEVER) {
        return Some(TtlSpec::Never);
    }
    parse_duration_days(s).map(TtlSpec::Days)
}

/// Parse `YYYY-MM-DD` → unix seconds at 00:00:00Z.
pub fn parse_date_ymd_utc(s: &str) -> Option<u64> {
    let mut it = s.split('-');
    let y: i64 = it.next()?.parse().ok()?;
    let mo: i64 = it.next()?.parse().ok()?;
    let d: i64 = it.next()?.parse().ok()?;
    if it.next().is_some() {
        return None;
    }
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || !(1970..=9999).contains(&y) {
        return None;
    }
    let days = days_from_civil(y, mo as u32, d as u32);
    u64::try_from(days * SECS_PER_DAY as i64).ok()
}

/// Format unix seconds → `YYYY-MM-DD` (UTC).
pub fn format_date_ymd_utc(t: u64) -> String {
    let days = (t / SECS_PER_DAY) as i64;
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let yy = if m <= 2 { y - 1 } else { y };
    let era = if yy >= 0 { yy } else { yy - 399 } / 400;
    let yoe = yy - era * 400;
    let mm = m as i64;
    let dd = d as i64;
    let doy = (153 * (if mm > 2 { mm - 3 } else { mm + 9 }) + 2) / 5 + dd - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let y = y + if m <= 2 { 1 } else { 0 };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;

    fn pk(seed: u8) -> String {
        Identity::from_seed(&[seed; 32]).to_ssh_string()
    }

    #[test]
    fn date_roundtrip() {
        assert_eq!(parse_date_ymd_utc("1970-01-01"), Some(0));
        assert_eq!(format_date_ymd_utc(0), "1970-01-01");
        for s in ["2026-05-20", "2026-08-18", "2024-02-29", "2100-03-01"] {
            let t = parse_date_ymd_utc(s).unwrap();
            assert_eq!(format_date_ymd_utc(t), s);
        }
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration_days("90d"), Some(90));
        assert_eq!(parse_duration_days("12w"), Some(84));
        assert_eq!(parse_duration_days("1y"), Some(365));
        assert_eq!(parse_duration_days("30"), Some(30));
        assert_eq!(parse_duration_days("never"), None);
    }

    #[test]
    fn admission() {
        let k = AuthKey::from_ssh(&pk(1), Some(1000), false, 0, "cli").unwrap();
        assert!(k.admissible(999));
        assert!(!k.admissible(1000));
        let unset = AuthKey::from_ssh(&pk(2), None, false, 0, "cli").unwrap();
        assert!(unset.admissible(u64::MAX)); // never silently rejected
        let never = AuthKey::from_ssh(&pk(3), Some(1), true, 0, "cli").unwrap();
        assert!(never.admissible(u64::MAX));
    }

    #[test]
    fn ttl_midnight() {
        let t = expiry_from_ttl_days(1_747_700_000, 90);
        assert_eq!(t % SECS_PER_DAY, 0);
    }
}
