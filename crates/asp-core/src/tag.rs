//! Tags — user-named markers at points in history. Where a branch is a *scoped
//! view* over the log, a tag is just a **labelled bookmark**: a name pinned to a
//! wall-clock instant (and the branch it was taken on) so a person can find and
//! return to a moment worth remembering among thousands of edits.
//!
//! Like branch records (§7), a tag rides the shared log as a synced `Kind::Tag`
//! row whose result blob is the JSON-encoded [`Tag`], keyed by `file_id = tag_id`,
//! so create → rename → delete converge last-writer-wins on any arrival order.
//! This module is pure and wasm-safe: native and web reconcile the identical tag
//! set from the identical rows.

use crate::log::{Kind, LogRow, MAIN_BRANCH_ID};
use crate::order::OrderKey;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A named marker at a point in history. `at_ts` is the wall-clock instant the tag
/// points at (unix seconds); `branch_id` is the branch it was taken on so the UI
/// can place it on the right lane. `deleted` is a soft-delete tombstone.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tag {
    pub tag_id: String,
    pub name: String,
    /// Wall-clock unix seconds the tag marks.
    pub at_ts: i64,
    /// Lamport of the tagged point (for stable graph placement / ordering).
    #[serde(default)]
    pub at_lamport: u64,
    /// The branch the tag was taken on (lane placement). Defaults to `main`.
    #[serde(default = "main_branch")]
    pub branch_id: String,
    pub created_lamport: u64,
    pub created_ts: i64,
    #[serde(default)]
    pub deleted: bool,
}

fn main_branch() -> String {
    MAIN_BRANCH_ID.to_string()
}

impl Tag {
    /// Stable content-hash id for a tag, derived from its target + lineage so two
    /// devices tagging "the same" moment independently get DISTINCT ids (concurrent
    /// creation → two tags), while a deterministic replay reproduces the id.
    pub fn derive_id(name: &str, at_ts: i64, branch_id: &str, created_lamport: u64, site_id: &str) -> String {
        let parts: Vec<Vec<u8>> = vec![
            name.as_bytes().to_vec(),
            at_ts.to_be_bytes().to_vec(),
            branch_id.as_bytes().to_vec(),
            created_lamport.to_be_bytes().to_vec(),
            site_id.as_bytes().to_vec(),
        ];
        let refs: Vec<&[u8]> = parts.iter().map(|p| p.as_slice()).collect();
        crate::oid::merkle_id(&refs)
    }
}

/// Validate a user-supplied tag name. An empty / whitespace-only name would create
/// a tag that can never be found by name — reject it at creation.
pub fn validate_tag_name(name: &str) -> crate::error::AspResult<()> {
    if name.trim().is_empty() {
        return Err(crate::error::AspError::Invalid("tag name must not be empty".into()));
    }
    Ok(())
}

/// JSON-encode a tag record into the bytes carried by its `Kind::Tag` row's result
/// blob. The row's `file_id` is the `tag_id`; multiple records for one tag (create
/// → rename → delete) converge by fold order key.
pub fn encode_tag_record(t: &Tag) -> Vec<u8> {
    serde_json::to_vec(t).unwrap_or_default()
}

/// Reconcile the synced tag set from the `Kind::Tag` records among `rows`: group by
/// `file_id` (= `tag_id`), keep the highest-order-key record (last-writer-wins on
/// name/deleted), and decode its blob via `blob`. Tombstoned tags are included —
/// their rows persist for history; callers filter for the live set. Deterministic
/// on any arrival order.
pub fn reconcile_tags(rows: &[LogRow], blob: impl Fn(&str) -> Option<Vec<u8>>) -> Vec<Tag> {
    let mut best: HashMap<String, (OrderKey, Tag)> = HashMap::new();
    for r in rows {
        if r.kind != Kind::Tag {
            continue;
        }
        let Some(h) = &r.result_hash else { continue };
        let Some(bytes) = blob(h) else { continue };
        let Ok(t) = serde_json::from_slice::<Tag>(&bytes) else { continue };
        let key = OrderKey { lamport: r.lamport, site_id: r.site_id.clone(), id: r.id.clone() };
        match best.get(&r.file_id) {
            Some((k, _)) if *k >= key => {}
            _ => {
                best.insert(r.file_id.clone(), (key, t));
            }
        }
    }
    let mut out: Vec<Tag> = best.into_values().map(|(_, t)| t).collect();
    // Stable order: by the instant they mark, then id.
    out.sort_by(|a, b| a.at_ts.cmp(&b.at_ts).then_with(|| a.tag_id.cmp(&b.tag_id)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::MergeClass;
    use crate::store::{BlobStore, MemBlobStore};

    fn mk_record(store: &MemBlobStore, tag_id: &str, name: &str, deleted: bool, lamport: u64, site: &str) -> LogRow {
        let t = Tag {
            tag_id: tag_id.into(),
            name: name.into(),
            at_ts: 1000,
            at_lamport: 5,
            branch_id: MAIN_BRANCH_ID.into(),
            created_lamport: 5,
            created_ts: 0,
            deleted,
        };
        let h = store.put_blob(&encode_tag_record(&t)).unwrap();
        LogRow {
            site_id: site.into(),
            lamport,
            seq: lamport,
            file_id: tag_id.into(),
            kind: Kind::Tag,
            merge_class: MergeClass::Text,
            result_hash: Some(h),
            path: Some(name.into()),
            ..LogRow::default()
        }
        .seal()
    }

    #[test]
    fn reconcile_tags_is_lww_and_order_invariant() {
        let store = MemBlobStore::new();
        // t1: created, then renamed (higher lamport wins), then a CONCURRENT rename
        // at the same lamport from a different site (LWW by site_id/id).
        let r_create = mk_record(&store, "t1", "release", false, 5, "aa");
        let r_rename = mk_record(&store, "t1", "release-v2", false, 7, "aa");
        let r_concurrent = mk_record(&store, "t1", "release-zz", false, 7, "zz");
        // t2: a tombstone.
        let r_del = mk_record(&store, "t2", "gone", true, 6, "aa");
        let rows = vec![r_create, r_rename, r_concurrent, r_del];

        let get = |h: &str| store.get_blob(h).ok().flatten();
        let recs = reconcile_tags(&rows, get);
        let t1 = recs.iter().find(|t| t.tag_id == "t1").unwrap();
        // Highest order key (lamport 7); among the two lamport-7 rows site "zz" > "aa".
        assert_eq!(t1.name, "release-zz");
        assert!(recs.iter().find(|t| t.tag_id == "t2").unwrap().deleted);

        // Order-invariant: shuffle the records, same result.
        let mut shuffled = rows.clone();
        shuffled.reverse();
        let recs2 = reconcile_tags(&shuffled, get);
        assert_eq!(recs2.iter().find(|t| t.tag_id == "t1").unwrap().name, "release-zz");
    }

    #[test]
    fn derive_id_is_stable_and_distinguishes_sites() {
        let a = Tag::derive_id("v1", 100, MAIN_BRANCH_ID, 3, "aa");
        let a2 = Tag::derive_id("v1", 100, MAIN_BRANCH_ID, 3, "aa");
        let b = Tag::derive_id("v1", 100, MAIN_BRANCH_ID, 3, "bb");
        assert_eq!(a, a2, "same inputs reproduce the id");
        assert_ne!(a, b, "different sites → distinct ids (concurrent tagging)");
    }
}
