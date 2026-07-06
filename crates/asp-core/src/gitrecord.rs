//! Git-bridge log-record payloads (git-bridge §6.1). The three additive `Kind`
//! variants — [`Kind::GitCommit`], [`Kind::GitIngest`], [`Kind::GitPlan`] — are all
//! **content-free of file bytes**: like a [`Kind::Branch`] record, each row's real
//! payload is a blob referenced by `result_hash`, and the fold treats the row as a
//! no-op (`fold.rs`). This module owns those payloads, their msgpack encode/decode
//! (same `to_vec_named` convention as `wire.rs`), and helper constructors that build
//! a **sealed** [`LogRow`] carrying each one — the shape the importer/bridge reuse.
//!
//! Pure and wasm-safe: it depends only on [`BlobStore`], `serde`, and the `oid`
//! hashing helpers, so the native `Engine` and the wasm-safe `MemEngine` build
//! byte-identical rows (git-bridge §3.2 determinism).

use crate::error::AspResult;
use crate::log::{Kind, LogRow, MergeClass, MAIN_BRANCH_ID};
use crate::store::BlobStore;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Import marker — one per imported upstream commit (git-bridge §3.1, §6.1). The
/// batch's file rows are attributed to the git author via this marker; the row's
/// `site_id` stays the derived repo site.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitCommitMarker {
    /// The git commit sha (hex). Also stored in the row's `path` for a cheap
    /// indexed lookup (git-bridge §6.1).
    pub sha: String,
    pub author_name: String,
    pub author_email: String,
    /// Committer timestamp (unix seconds) — mirrored into the row's `ts` so the
    /// timeline UI can scrub git history for free (git-bridge §3.1).
    pub committer_ts: i64,
    /// Subject + body.
    pub message: String,
    /// Parent commit shas (hex), first-parent first. Empty for the root commit.
    pub parents: Vec<String>,
    /// The ASP branch (lane) this commit was assigned to (git-bridge §3.1).
    pub branch_id: String,
}

/// Ledger record — appended after each successfully ingested commit (git-bridge
/// §4.1). Lets every node answer "which git commit is the vault at?" from the fold.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitIngestRecord {
    pub commit_sha: String,
    /// `(site_id, seq)` of the batch's last row — the ingest frontier's upper edge.
    pub upto_site: String,
    pub upto_seq: u64,
    /// Mode-table delta: `path -> git mode` (e.g. `100755` for `+x`). ASP doesn't
    /// model the executable bit, so push synthesis replays this (git-bridge §3.3).
    pub modes: Vec<(String, u32)>,
    /// Paths whose git entry is a symlink (`120000`) — re-encoded as symlinks on
    /// push (git-bridge §3.3).
    pub symlinks: Vec<String>,
    /// Paths whose git entry is a gitlink/submodule (`160000`) — materialized as
    /// nothing, recorded so push preserves them (git-bridge §3.3).
    pub gitlinks: Vec<String>,
    /// The remote ref this ingest advanced (e.g. `refs/heads/main`).
    pub remote_ref: String,
    /// Set by `rebaseline` after an upstream history rewrite (git-bridge §4.4).
    pub rebaselined: bool,
}

/// Commit plan — "everything up to `frontier` becomes one commit with `message`"
/// (git-bridge §5.1). Drives deterministic commit synthesis so any node may push.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitPlanRecord {
    /// The version vector (`site_id -> highest seq`) of rows the commit includes —
    /// same repr as `wire.rs`'s `Vector` and `branch.rs`'s `VersionVector`.
    pub frontier: BTreeMap<String, i64>,
    /// Subject + body.
    pub message: String,
    /// `"Name <email>"` — the commit's author/committer identity.
    pub author: String,
    /// Becomes the commit's author/committer date (unix seconds).
    pub planned_ts: i64,
}

/// Domain-versioned derivation of a 32-hex id from a domain tag + a key, matching
/// the 16-byte / 32-hex-char width of ordinary `file_id`s (`engine::random_id`).
/// Domain-tagged and frozen (`/v1`) like the Merkle-id domains, so independent
/// clones derive byte-identical ids (git-bridge §3.2).
fn derive_id32(domain: &str, key: &str) -> String {
    let mut buf = Vec::with_capacity(domain.len() + key.len());
    buf.extend_from_slice(domain.as_bytes());
    buf.extend_from_slice(key.as_bytes());
    crate::oid::content_hash(&buf)[..32].to_string()
}

/// The dedicated marker `file_id` for a commit sha: `hex(sha256("asp-git-marker/v1"
/// ‖ sha))[..32]` (git-bridge §6.1). Repo-independent, so two clones agree.
pub fn commit_marker_file_id(sha: &str) -> String {
    derive_id32("asp-git-marker/v1", sha)
}

/// The `file_id` grouping every `GitIngest` record for one commit sha. Two bridges
/// racing to ingest the same sha (git-bridge §4.3) author records with distinct
/// Merkle ids but this shared `file_id`, so "any ingest for this sha exists?" is a
/// `file_id` lookup.
pub fn ingest_file_id(commit_sha: &str) -> String {
    derive_id32("asp-git-ingest/v1", commit_sha)
}

/// The `file_id` for a plan, derived from its payload blob hash so byte-identical
/// plans authored on two nodes share it (dedup) while distinct plans differ.
pub fn plan_file_id(payload_hash: &str) -> String {
    derive_id32("asp-git-plan/v1", payload_hash)
}

/// The identity fields a caller supplies when sealing a git-bridge marker row —
/// the parts the pure constructor can't derive (they depend on the authoring
/// node's clock/counters, or the imported chain's tip).
#[derive(Clone, Debug)]
pub struct GitRowIdentity {
    pub site_id: String,
    pub lamport: u64,
    pub seq: u64,
    pub ts: i64,
    /// Previous log id on this row's chain (the imported chain's tip, or `None`).
    pub parent: Option<String>,
}

/// Marker rows carry a binary msgpack payload blob and never fold, so `merge_class`
/// is cosmetic; `Binary` reflects the payload honestly and can never be mistaken
/// for foldable text content.
const MARKER_CLASS: MergeClass = MergeClass::Binary;

// ----- encode / decode -----

macro_rules! codec {
    ($enc:ident, $dec:ident, $ty:ty) => {
        /// msgpack-encode the payload (`to_vec_named`, matching `wire.rs`).
        pub fn $enc(v: &$ty) -> AspResult<Vec<u8>> {
            rmp_serde::to_vec_named(v).map_err(|e| crate::error::AspError::Protocol(e.to_string()))
        }
        /// msgpack-decode the payload; rejects garbage with a `Protocol` error.
        pub fn $dec(b: &[u8]) -> AspResult<$ty> {
            rmp_serde::from_slice(b).map_err(|e| crate::error::AspError::Protocol(e.to_string()))
        }
    };
}

codec!(encode_commit_marker, decode_commit_marker, GitCommitMarker);
codec!(encode_ingest_record, decode_ingest_record, GitIngestRecord);
codec!(encode_plan_record, decode_plan_record, GitPlanRecord);

// ----- sealed-row constructors -----

/// Build a sealed [`Kind::GitCommit`] row: payload blob → `result_hash`, `path` =
/// the commit sha (indexed lookup), `file_id` = the derived marker id, `branch_id`
/// = the commit's assigned lane, `ts` = committer time. Stores the payload blob.
pub fn build_commit_marker_row(store: &dyn BlobStore, ident: &GitRowIdentity, marker: &GitCommitMarker) -> AspResult<LogRow> {
    let payload = encode_commit_marker(marker)?;
    let result_hash = store.put_blob(&payload)?;
    Ok(LogRow {
        site_id: ident.site_id.clone(),
        lamport: ident.lamport,
        seq: ident.seq,
        ts: ident.ts,
        file_id: commit_marker_file_id(&marker.sha),
        kind: Kind::GitCommit,
        merge_class: MARKER_CLASS,
        parent: ident.parent.clone(),
        base_hash: None,
        result_hash: Some(result_hash),
        path: Some(marker.sha.clone()),
        branch_id: marker.branch_id.clone(),
        merge_parent: None,
        sig: vec![],
        id: String::new(),
    }
    .seal())
}

/// Build a sealed [`Kind::GitIngest`] ledger row on `main`: payload blob →
/// `result_hash`, `file_id` groups records for one sha, `path` = the commit sha for
/// a cheap "which commit" lookup. Stores the payload blob.
pub fn build_ingest_row(store: &dyn BlobStore, ident: &GitRowIdentity, rec: &GitIngestRecord) -> AspResult<LogRow> {
    let payload = encode_ingest_record(rec)?;
    let result_hash = store.put_blob(&payload)?;
    Ok(LogRow {
        site_id: ident.site_id.clone(),
        lamport: ident.lamport,
        seq: ident.seq,
        ts: ident.ts,
        file_id: ingest_file_id(&rec.commit_sha),
        kind: Kind::GitIngest,
        merge_class: MARKER_CLASS,
        parent: ident.parent.clone(),
        base_hash: None,
        result_hash: Some(result_hash),
        path: Some(rec.commit_sha.clone()),
        branch_id: MAIN_BRANCH_ID.to_string(),
        merge_parent: None,
        sig: vec![],
        id: String::new(),
    }
    .seal())
}

/// Build a sealed [`Kind::GitPlan`] row on `main`: payload blob → `result_hash`,
/// `file_id` derived from the payload hash (equal plans dedup). Stores the payload
/// blob.
pub fn build_plan_row(store: &dyn BlobStore, ident: &GitRowIdentity, rec: &GitPlanRecord) -> AspResult<LogRow> {
    let payload = encode_plan_record(rec)?;
    let result_hash = store.put_blob(&payload)?;
    Ok(LogRow {
        site_id: ident.site_id.clone(),
        lamport: ident.lamport,
        seq: ident.seq,
        ts: ident.ts,
        file_id: plan_file_id(&result_hash),
        kind: Kind::GitPlan,
        merge_class: MARKER_CLASS,
        parent: ident.parent.clone(),
        base_hash: None,
        result_hash: Some(result_hash),
        path: None,
        branch_id: MAIN_BRANCH_ID.to_string(),
        merge_parent: None,
        sig: vec![],
        id: String::new(),
    }
    .seal())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemBlobStore;

    fn marker() -> GitCommitMarker {
        GitCommitMarker {
            sha: "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2".into(),
            author_name: "Ada Lovelace".into(),
            author_email: "ada@example.com".into(),
            committer_ts: 1_700_000_000,
            message: "Initial commit\n\nBody line.".into(),
            parents: vec!["deadbeef".into()],
            branch_id: "main".into(),
        }
    }

    fn ingest() -> GitIngestRecord {
        GitIngestRecord {
            commit_sha: "cafebabecafebabecafebabecafebabecafebabe".into(),
            upto_site: "site-xyz".into(),
            upto_seq: 42,
            modes: vec![("bin/run".into(), 0o100755), ("README.md".into(), 0o100644)],
            symlinks: vec!["link/to/thing".into()],
            gitlinks: vec!["vendor/sub".into()],
            remote_ref: "refs/heads/main".into(),
            rebaselined: false,
        }
    }

    fn plan() -> GitPlanRecord {
        GitPlanRecord {
            frontier: BTreeMap::from([("aa".to_string(), 3i64), ("bb".to_string(), 7i64)]),
            message: "asp: 2 files changed".into(),
            author: "Bridge Bot <bot@example.com>".into(),
            planned_ts: 1_700_000_500,
        }
    }

    #[test]
    fn payloads_round_trip() {
        let m = marker();
        assert_eq!(decode_commit_marker(&encode_commit_marker(&m).unwrap()).unwrap(), m);
        let i = ingest();
        assert_eq!(decode_ingest_record(&encode_ingest_record(&i).unwrap()).unwrap(), i);
        let p = plan();
        assert_eq!(decode_plan_record(&encode_plan_record(&p).unwrap()).unwrap(), p);
    }

    #[test]
    fn payload_edge_cases_round_trip() {
        // Empty parents (root commit), unicode message, empty vectors.
        let root = GitCommitMarker {
            sha: "0000000000000000000000000000000000000000".into(),
            author_name: "Ünïcödé Nàme 🌳".into(),
            author_email: "".into(),
            committer_ts: -1, // pre-epoch commit, still valid
            message: "首次提交\n\n🎉 emoji body ✅".into(),
            parents: vec![],
            branch_id: "git/abc123".into(),
        };
        assert_eq!(decode_commit_marker(&encode_commit_marker(&root).unwrap()).unwrap(), root);

        // Huge mode table.
        let mut modes = Vec::new();
        for n in 0..5000u32 {
            modes.push((format!("dir{}/file{}.rs", n % 50, n), if n % 2 == 0 { 0o100644 } else { 0o100755 }));
        }
        let big = GitIngestRecord { modes, ..ingest() };
        let bytes = encode_ingest_record(&big).unwrap();
        assert_eq!(decode_ingest_record(&bytes).unwrap(), big);

        // Empty frontier plan.
        let empty_plan = GitPlanRecord { frontier: BTreeMap::new(), message: String::new(), ..plan() };
        assert_eq!(decode_plan_record(&encode_plan_record(&empty_plan).unwrap()).unwrap(), empty_plan);
    }

    #[test]
    fn decode_rejects_garbage() {
        for bad in [&b""[..], b"not msgpack", b"\xff\xff\xff\xff", &[0x81u8, 0xa3, b'x', b'y', b'z'][..]] {
            assert!(decode_commit_marker(bad).is_err());
            assert!(decode_ingest_record(bad).is_err());
            assert!(decode_plan_record(bad).is_err());
        }
    }

    /// Hand-rolled LCG fuzz (no proptest/cargo-fuzz): decode must never panic on
    /// arbitrary bytes — it returns `Ok` or `Err`, always. Mirrors the repo's
    /// seeded deterministic fuzz style.
    #[test]
    fn decode_never_panics_fuzz() {
        let mut state: u64 = 0x9E3779B97F4A7C15;
        let mut next = || {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (state >> 33) as u32
        };
        for _ in 0..4000 {
            let len = (next() % 64) as usize;
            let mut buf = Vec::with_capacity(len);
            for _ in 0..len {
                buf.push((next() & 0xff) as u8);
            }
            // Any of these may Ok or Err on random bytes; the assertion is that
            // none of them panic (the test failing == a panic unwound).
            let _ = decode_commit_marker(&buf);
            let _ = decode_ingest_record(&buf);
            let _ = decode_plan_record(&buf);
        }
    }

    #[test]
    fn derived_ids_are_stable_deterministic_and_32_hex() {
        let a = commit_marker_file_id("abc");
        let b = commit_marker_file_id("abc");
        assert_eq!(a, b);
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        // Different domains for the same key must not collide.
        assert_ne!(commit_marker_file_id("k"), ingest_file_id("k"));
        assert_ne!(ingest_file_id("k"), plan_file_id("k"));
        // Different keys differ.
        assert_ne!(commit_marker_file_id("x"), commit_marker_file_id("y"));
    }

    #[test]
    fn commit_marker_row_is_sealed_and_shaped() {
        let store = MemBlobStore::new();
        let m = marker();
        let ident = GitRowIdentity { site_id: "repo-site".into(), lamport: 1, seq: 0, ts: m.committer_ts, parent: None };
        let row = build_commit_marker_row(&store, &ident, &m).unwrap();
        assert!(row.id_valid(), "row must be sealed");
        assert_eq!(row.kind, Kind::GitCommit);
        assert_eq!(row.path.as_deref(), Some(m.sha.as_str()), "path is the sha for indexed lookup");
        assert_eq!(row.file_id, commit_marker_file_id(&m.sha));
        assert_eq!(row.branch_id, m.branch_id, "GitCommit rides its assigned branch");
        // The payload blob is recoverable via result_hash and decodes back.
        let blob = store.get_blob(row.result_hash.as_ref().unwrap()).unwrap().unwrap();
        assert_eq!(decode_commit_marker(&blob).unwrap(), m);
    }

    #[test]
    fn ingest_and_plan_rows_ride_main_and_are_sealed() {
        let store = MemBlobStore::new();
        let i = ingest();
        let ident = GitRowIdentity { site_id: "repo-site".into(), lamport: 5, seq: 2, ts: 100, parent: Some("tip".into()) };
        let irow = build_ingest_row(&store, &ident, &i).unwrap();
        assert!(irow.id_valid());
        assert_eq!(irow.kind, Kind::GitIngest);
        assert_eq!(irow.branch_id, MAIN_BRANCH_ID);
        assert_eq!(irow.file_id, ingest_file_id(&i.commit_sha));
        assert_eq!(decode_ingest_record(&store.get_blob(irow.result_hash.as_ref().unwrap()).unwrap().unwrap()).unwrap(), i);

        let p = plan();
        let prow = build_plan_row(&store, &ident, &p).unwrap();
        assert!(prow.id_valid());
        assert_eq!(prow.kind, Kind::GitPlan);
        assert_eq!(prow.branch_id, MAIN_BRANCH_ID);
        assert_eq!(prow.file_id, plan_file_id(prow.result_hash.as_ref().unwrap()));
        assert_eq!(decode_plan_record(&store.get_blob(prow.result_hash.as_ref().unwrap()).unwrap().unwrap()).unwrap(), p);
    }
}
