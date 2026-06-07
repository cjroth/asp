//! The append-only global log — the source of truth (§Data model). One row per
//! net change to one `file_id`. Current files, any past state, the derived git
//! history and search indexes are all pure functions of this log.
//!
//! A row is content-addressed by its **Merkle id** (`id` = SHA-256 over its
//! canonical fields), so it is tamper-evident and self-deduplicating. The id is
//! computed over every semantic field *except* `id` and `sig` (the signature is
//! optional and must not change the id).

use crate::oid::merkle_id;
use serde::{Deserialize, Serialize};

/// What a row does to its `file_id`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Create,
    Edit,
    Rename,
    Delete,
    Reclass,
}

impl Kind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Kind::Create => "create",
            Kind::Edit => "edit",
            Kind::Rename => "rename",
            Kind::Delete => "delete",
            Kind::Reclass => "reclass",
        }
    }
    pub fn parse(s: &str) -> Option<Kind> {
        Some(match s {
            "create" => Kind::Create,
            "edit" => Kind::Edit,
            "rename" => Kind::Rename,
            "delete" => Kind::Delete,
            "reclass" => Kind::Reclass,
            _ => return None,
        })
    }
}

/// How a file's rows are merged. Set at create, constant for the `file_id`
/// except across an explicit `reclass` boundary (§The merge model).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MergeClass {
    Text,
    Code,
    Binary,
}

impl MergeClass {
    pub fn as_str(&self) -> &'static str {
        match self {
            MergeClass::Text => "text",
            MergeClass::Code => "code",
            MergeClass::Binary => "binary",
        }
    }
    pub fn parse(s: &str) -> Option<MergeClass> {
        Some(match s {
            "text" => MergeClass::Text,
            "code" => MergeClass::Code,
            "binary" => MergeClass::Binary,
            _ => return None,
        })
    }
}

/// One row of the append-only global log.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LogRow {
    /// Merkle id = hash of this row's canonical fields. Set by [`LogRow::sealed`].
    pub id: String,
    /// Authoring device = its ed25519 NodeId (hex).
    pub site_id: String,
    /// Logical clock = max(observed)+1; durably persisted. Drives the tiebreak.
    pub lamport: u64,
    /// Per-device DENSE counter (version vector, gap detection).
    pub seq: u64,
    /// Authoring wall-clock (unix seconds); PITR + post-v1 wall_clock experiment.
    pub ts: i64,
    /// STABLE per-file identity (survives renames).
    pub file_id: String,
    pub kind: Kind,
    /// Set at create; changes only via `reclass`.
    pub merge_class: MergeClass,
    /// Previous log id for this file_id (causal dependency; LCA chain).
    pub parent: Option<String>,
    /// Content hash the diff/edit applies to (None on create).
    pub base_hash: Option<String>,
    /// Resulting content hash (None on delete).
    pub result_hash: Option<String>,
    /// Set by create/rename: the file's path as of this row.
    pub path: Option<String>,
    /// Optional ed25519 author signature over the row (off by default).
    #[serde(with = "serde_bytes", default)]
    pub sig: Vec<u8>,
}

impl LogRow {
    /// Canonical byte encoding of the semantic fields (everything but `id` and
    /// `sig`), used to compute the Merkle id and the optional signature.
    fn canonical_fields(&self) -> Vec<Vec<u8>> {
        vec![
            self.site_id.as_bytes().to_vec(),
            self.lamport.to_be_bytes().to_vec(),
            self.seq.to_be_bytes().to_vec(),
            self.ts.to_be_bytes().to_vec(),
            self.file_id.as_bytes().to_vec(),
            self.kind.as_str().as_bytes().to_vec(),
            self.merge_class.as_str().as_bytes().to_vec(),
            self.parent.clone().unwrap_or_default().into_bytes(),
            self.base_hash.clone().unwrap_or_default().into_bytes(),
            self.result_hash.clone().unwrap_or_default().into_bytes(),
            self.path.clone().unwrap_or_default().into_bytes(),
        ]
    }

    /// Bytes that the Merkle id (and optional signature) cover.
    pub fn signing_payload(&self) -> Vec<u8> {
        let fields = self.canonical_fields();
        let refs: Vec<&[u8]> = fields.iter().map(|f| f.as_slice()).collect();
        let mut out = Vec::new();
        for f in &refs {
            out.extend_from_slice(&(f.len() as u64).to_be_bytes());
            out.extend_from_slice(f);
        }
        out
    }

    /// Compute and set this row's Merkle id from its canonical fields.
    pub fn seal(mut self) -> LogRow {
        let fields = self.canonical_fields();
        let refs: Vec<&[u8]> = fields.iter().map(|f| f.as_slice()).collect();
        self.id = merkle_id(&refs);
        self
    }

    /// Verify the stored `id` matches a recomputation over the fields.
    pub fn id_valid(&self) -> bool {
        let fields = self.canonical_fields();
        let refs: Vec<&[u8]> = fields.iter().map(|f| f.as_slice()).collect();
        merkle_id(&refs) == self.id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row() -> LogRow {
        LogRow {
            id: String::new(),
            site_id: "aa".into(),
            lamport: 1,
            seq: 0,
            ts: 100,
            file_id: "f1".into(),
            kind: Kind::Create,
            merge_class: MergeClass::Text,
            parent: None,
            base_hash: None,
            result_hash: Some("deadbeef".into()),
            path: Some("a.md".into()),
            sig: vec![],
        }
    }

    #[test]
    fn merkle_id_is_stable_and_tamper_evident() {
        let r = row().seal();
        assert!(r.id_valid());
        assert_eq!(r.id.len(), 64);
        // Re-sealing yields the same id.
        let r2 = row().seal();
        assert_eq!(r.id, r2.id);
        // A field change changes the id.
        let mut tampered = r.clone();
        tampered.path = Some("b.md".into());
        assert!(!tampered.id_valid());
    }

    #[test]
    fn sig_does_not_affect_id() {
        let mut r = row().seal();
        let id_before = r.id.clone();
        r.sig = vec![1, 2, 3];
        assert_eq!(r.id, id_before);
        assert!(r.id_valid());
    }
}
