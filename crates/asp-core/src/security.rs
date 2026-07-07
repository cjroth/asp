//! Verified security profile — optional per-vault ed25519 row signing
//! (scoped-sync §4.4). A **per-vault mode the user chooses at genesis**, made clean
//! by one frozen fact: `sig` is EXCLUDED from the Merkle id ([`crate::log::LogRow`]
//! `canonical_fields` / the `sig_does_not_affect_id` test), so a signed row and the
//! same row unsigned have the IDENTICAL id. Turning signing on/off never changes
//! ids, never breaks content-addressing or dedup, and **never forks a vault**.
//!
//! - **Trust mode** (default = today): rows carry no `sig`; integrate checks only
//!   `id_valid()`. Read-only/partial guarantees are topological (a single
//!   integrator / an enforced star).
//! - **Verified mode** (opt-in): every mutating row is signed by its author; a
//!   node **rejects** unsigned/wrong-author mutating rows at integrate, so the
//!   read-only + subdir-read guarantees hold in a true P2P mesh. The mode is
//!   genesis-inherited (a clone learns it and cannot locally downgrade) — the
//!   defense against the downgrade attack (a stripped-signature copy laundering
//!   through a lenient node).
//!
//! This module is the pure verification core (std-only, wasm-safe). The engines
//! sign at the builders and call [`row_admissible`] on the integrate path; the
//! profile + signing epoch live in each vault's config (genesis-inherited).

use crate::identity::verify_detached;
use crate::log::LogRow;
use crate::order::NodeId;

/// The config key holding a vault's security profile (`"verified"` ⇒ Verified,
/// anything else / absent ⇒ Trust). Genesis-set and inherited by every clone.
pub const PROFILE_KEY: &str = "security_profile";
/// The config key holding the Trust→Verified signing-epoch cutoff (a `lamport`
/// watermark): mutating rows with `lamport < epoch` are grandfathered as trusted.
/// Absent / `0` ⇒ Verified-at-genesis (no grandfathering; every mutating row signed).
pub const EPOCH_KEY: &str = "signing_epoch";

pub const VERIFIED: &str = "verified";
pub const TRUST: &str = "trust";

/// Does this profile string mean Verified mode?
pub fn is_verified(profile: Option<&str>) -> bool {
    profile == Some(VERIFIED)
}

/// Verify a row's own author signature (scoped-sync §4.4): the `sig` must be a
/// valid ed25519 signature over `signing_payload()` by the row's `site_id` (which
/// **is** the author's ed25519 NodeId). Proves the row was authored by the holder
/// of that key — an attacker cannot forge a row attributed to someone else.
pub fn row_signature_valid(row: &LogRow) -> bool {
    if row.sig.is_empty() {
        return false;
    }
    let Some(node) = NodeId::from_hex(&row.site_id) else {
        return false;
    };
    verify_detached(&node, &row.signing_payload(), &row.sig).is_ok()
}

/// May a Verified vault integrate `row`? (scoped-sync §4.4)
/// - Trust mode (`verified=false`): always yes (unchanged behavior).
/// - Non-file rows (metadata / git): always yes (not content mutations).
/// - Pre-epoch rows (`lamport < epoch`): grandfathered as trusted.
/// - Otherwise: yes iff the row carries a valid author signature.
///
/// The author→path ACL (which author may write which path) is enforced separately
/// on the engine, where the author's `authorized_keys` grant is available.
pub fn row_admissible(row: &LogRow, verified: bool, epoch: u64) -> bool {
    if !verified || !row.kind.is_file_mutation() || row.lamport < epoch {
        return true;
    }
    row_signature_valid(row)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;
    use crate::log::{Kind, LogRow};

    fn mutating_row(author: &Identity, lamport: u64) -> LogRow {
        LogRow {
            site_id: author.node_id().to_hex(),
            lamport,
            seq: 0,
            file_id: "f1".into(),
            kind: Kind::Create,
            result_hash: Some("deadbeef".into()),
            path: Some("a.md".into()),
            ..LogRow::default()
        }
        .seal()
    }

    fn sign(mut row: LogRow, author: &Identity) -> LogRow {
        row.sig = author.sign(&row.signing_payload());
        row
    }

    #[test]
    fn trust_mode_admits_everything() {
        let a = Identity::from_seed(&[1; 32]);
        let unsigned = mutating_row(&a, 5);
        assert!(row_admissible(&unsigned, false, 0), "Trust mode never gates on signatures");
    }

    #[test]
    fn verified_rejects_unsigned_but_accepts_valid_signature() {
        let a = Identity::from_seed(&[1; 32]);
        let unsigned = mutating_row(&a, 5);
        assert!(!row_admissible(&unsigned, true, 0), "Verified rejects an unsigned mutating row");
        let signed = sign(mutating_row(&a, 5), &a);
        assert!(row_signature_valid(&signed));
        assert!(row_admissible(&signed, true, 0), "Verified accepts a validly-signed row");
    }

    #[test]
    fn verified_rejects_a_forged_signature() {
        // A row claiming author A but signed by B (impersonation) must fail.
        let a = Identity::from_seed(&[1; 32]);
        let b = Identity::from_seed(&[2; 32]);
        let forged = sign(mutating_row(&a, 5), &b); // sig by B over A's payload
        assert!(!row_signature_valid(&forged), "a signature by the wrong key must not verify");
        assert!(!row_admissible(&forged, true, 0));
    }

    #[test]
    fn signature_does_not_change_the_merkle_id() {
        // The frozen property that makes signing additive (never forks a vault).
        let a = Identity::from_seed(&[1; 32]);
        let unsigned = mutating_row(&a, 5);
        let signed = sign(unsigned.clone(), &a);
        assert_eq!(signed.id, unsigned.id, "sig must not change the id");
        assert!(signed.id_valid());
    }

    #[test]
    fn signing_epoch_grandfathers_pre_cutoff_rows() {
        let a = Identity::from_seed(&[1; 32]);
        // epoch = 10: unsigned rows with lamport < 10 are trusted; >= 10 must be signed.
        assert!(row_admissible(&mutating_row(&a, 9), true, 10), "pre-epoch unsigned row grandfathered");
        assert!(!row_admissible(&mutating_row(&a, 10), true, 10), "post-epoch unsigned row rejected");
        assert!(row_admissible(&sign(mutating_row(&a, 10), &a), true, 10), "post-epoch signed row accepted");
    }

    #[test]
    fn non_file_rows_are_never_gated() {
        let a = Identity::from_seed(&[1; 32]);
        let mut branch = mutating_row(&a, 5);
        branch.kind = Kind::Branch;
        let branch = branch.seal();
        assert!(row_admissible(&branch, true, 0), "metadata rows are not content mutations");
    }
}
