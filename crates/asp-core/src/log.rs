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

/// The stable id of the root branch every vault starts on. A fixed sentinel (not a
/// content hash) so a pre-branching vault — whose rows carry no `branch_id` — reads
/// back as `main` via the column/serde default, byte-identical to today (§9 migration).
pub const MAIN_BRANCH_ID: &str = "main";

/// What a row does to its `file_id`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Create,
    Edit,
    Rename,
    Delete,
    Reclass,
    /// A 2-parent merge marker authored by `merge_branch` (§2.6): `parent` is the
    /// destination branch tip, `merge_parent` the source tip. Carries no content of
    /// its own (the per-file merge edits are separate rows); it exists so the graph
    /// and the derived git history show an explicit merge node.
    Merge,
    /// A synced **branch record** (§7): creation/rename/delete of a branch rides the
    /// same anti-entropy path as content rows. The branch metadata is carried in the
    /// row's `path`/`base_hash`/`result_hash` fields (see `sqlite::Branch`).
    Branch,
    /// A synced **tag record**: a user-named marker at a point in history (a moment
    /// worth returning to among thousands of edits). Like `Kind::Branch` it is
    /// metadata — the JSON-encoded `Tag` rides in `result_hash`'s blob, keyed by
    /// `file_id = tag_id`, and converges last-writer-wins. Never touches the fold.
    Tag,
    /// A **git import marker** (git-bridge §3.1, §6.1): one per imported upstream
    /// commit, attributing the batch's file rows to the git author. Like `Branch`
    /// it is content-free of file bytes — its msgpack `GitCommitMarker` payload
    /// (`gitrecord.rs`) rides in `result_hash`'s blob, and `path` = the commit sha
    /// for a cheap indexed lookup. A no-op on the fold.
    GitCommit,
    /// A synced **ingest ledger record** (git-bridge §4.1, §6.1): appended after
    /// each ingested commit so every node can answer "which git commit is the vault
    /// at?" from the fold. Metadata only — its `GitIngestRecord` payload rides in
    /// `result_hash`'s blob. A no-op on the fold.
    GitIngest,
    /// A synced **commit plan** (git-bridge §5.1, §6.1): "everything up to frontier
    /// F becomes one commit with message M". Drives deterministic commit synthesis
    /// so any node may push idempotently. Metadata only — its `GitPlanRecord`
    /// payload rides in `result_hash`'s blob. A no-op on the fold.
    GitPlan,
}

impl Kind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Kind::Create => "create",
            Kind::Edit => "edit",
            Kind::Rename => "rename",
            Kind::Delete => "delete",
            Kind::Reclass => "reclass",
            Kind::Merge => "merge",
            Kind::Branch => "branch",
            Kind::Tag => "tag",
            Kind::GitCommit => "gitcommit",
            Kind::GitIngest => "gitingest",
            Kind::GitPlan => "gitplan",
        }
    }
    pub fn parse(s: &str) -> Option<Kind> {
        Some(match s {
            "create" => Kind::Create,
            "edit" => Kind::Edit,
            "rename" => Kind::Rename,
            "delete" => Kind::Delete,
            "reclass" => Kind::Reclass,
            "merge" => Kind::Merge,
            "branch" => Kind::Branch,
            "tag" => Kind::Tag,
            "gitcommit" => Kind::GitCommit,
            "gitingest" => Kind::GitIngest,
            "gitplan" => Kind::GitPlan,
            _ => return None,
        })
    }

    /// A row that mutates file CONTENT — the kinds a read-only peer (B) may not push
    /// and a Verified vault (scoped-sync §4.4) requires signed. Metadata rows
    /// (`Merge`/`Branch`/`Tag` and the git kinds) are not file mutations.
    pub fn is_file_mutation(&self) -> bool {
        matches!(self, Kind::Create | Kind::Edit | Kind::Rename | Kind::Delete | Kind::Reclass)
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
    /// A content-free **directory entity** — `result_hash` is always NULL and it
    /// never merges. Materialized as a real directory (`mkdir`), so an empty
    /// folder replicates without a marker file (§Capture: empty directories).
    Dir,
}

impl MergeClass {
    pub fn as_str(&self) -> &'static str {
        match self {
            MergeClass::Text => "text",
            MergeClass::Code => "code",
            MergeClass::Binary => "binary",
            MergeClass::Dir => "dir",
        }
    }
    pub fn parse(s: &str) -> Option<MergeClass> {
        Some(match s {
            "text" => MergeClass::Text,
            "code" => MergeClass::Code,
            "binary" => MergeClass::Binary,
            "dir" => MergeClass::Dir,
            _ => return None,
        })
    }
}

/// Classify a path's merge behavior (§The merge model). Constant per `file_id`
/// from creation; changes only via an explicit `reclass`. Surface-independent
/// (used identically by the native engine and the wasm node).
pub fn classify(path: &str, bytes: &[u8]) -> MergeClass {
    if std::str::from_utf8(bytes).is_err() || bytes.contains(&0) {
        return MergeClass::Binary;
    }
    let ext = std::path::Path::new(path).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    const CODE: &[&str] = &[
        "rs", "py", "js", "ts", "tsx", "jsx", "go", "c", "h", "cpp", "cc", "hpp", "java", "rb",
        "sh", "bash", "zsh", "php", "swift", "kt", "scala", "lua", "pl", "r", "sql", "toml", "yaml",
        "yml", "json", "xml", "html", "css", "scss", "vue", "ex", "exs", "erl", "hs", "ml", "fs",
        "cs", "dart", "zig", "nim",
    ];
    if CODE.contains(&ext.as_str()) {
        MergeClass::Code
    } else {
        MergeClass::Text
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
    /// The branch this row was authored on (§2). A row is only visible to folds
    /// scoped to this branch or its descendants (up to their fork point). Pre-
    /// branching rows default to [`MAIN_BRANCH_ID`].
    #[serde(default = "default_branch_id")]
    pub branch_id: String,
    /// Second parent — set only on [`Kind::Merge`] (§2.6); the source branch tip.
    #[serde(default)]
    pub merge_parent: Option<String>,
    /// Optional ed25519 author signature over the row (off by default).
    #[serde(with = "serde_bytes", default)]
    pub sig: Vec<u8>,
}

fn default_branch_id() -> String {
    MAIN_BRANCH_ID.to_string()
}

impl Default for LogRow {
    /// A blank, `main`-branch row — every field empty/None. Construction sites fill
    /// in what they need with `..LogRow::default()`, so adding a field doesn't
    /// require touching every literal.
    fn default() -> LogRow {
        LogRow {
            id: String::new(),
            site_id: String::new(),
            lamport: 0,
            seq: 0,
            ts: 0,
            file_id: String::new(),
            kind: Kind::Edit,
            merge_class: MergeClass::Text,
            parent: None,
            base_hash: None,
            result_hash: None,
            path: None,
            branch_id: MAIN_BRANCH_ID.to_string(),
            merge_parent: None,
            sig: vec![],
        }
    }
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
            self.branch_id.clone().into_bytes(),
            self.merge_parent.clone().unwrap_or_default().into_bytes(),
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
            site_id: "aa".into(),
            lamport: 1,
            ts: 100,
            file_id: "f1".into(),
            kind: Kind::Create,
            result_hash: Some("deadbeef".into()),
            path: Some("a.md".into()),
            ..LogRow::default()
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
    fn branch_fields_are_covered_by_the_id() {
        // §3.1: branch_id and merge_parent are part of the Merkle id, so a row
        // re-tagged onto another branch (or given a second parent) is a distinct row.
        let r = row().seal();
        let mut on_b = r.clone();
        on_b.branch_id = "b-xyz".into();
        assert!(!on_b.id_valid(), "branch_id must change the id");
        let resealed = on_b.clone().seal();
        assert_ne!(resealed.id, r.id);

        let mut merged = r.clone();
        merged.kind = Kind::Merge;
        merged.merge_parent = Some("other-tip".into());
        let merged = merged.seal();
        let mut tampered = merged.clone();
        tampered.merge_parent = Some("forged".into());
        assert!(!tampered.id_valid(), "merge_parent must change the id");
    }

    #[test]
    fn kind_branch_and_merge_round_trip() {
        for k in [Kind::Merge, Kind::Branch] {
            assert_eq!(Kind::parse(k.as_str()), Some(k));
        }
    }

    #[test]
    fn kind_git_variants_round_trip() {
        // git-bridge §6.1: the three new kinds must survive as_str/parse (a
        // Kind::parse -> None at the sqlite read boundary silently drops rows) and
        // serde msgpack (rename_all = "lowercase" => "gitcommit"/etc), which is what
        // makes an old proto-3 peer reject them (git-bridge §6.2).
        for (k, s) in [
            (Kind::GitCommit, "gitcommit"),
            (Kind::GitIngest, "gitingest"),
            (Kind::GitPlan, "gitplan"),
        ] {
            assert_eq!(k.as_str(), s);
            assert_eq!(Kind::parse(s), Some(k));
            let bytes = rmp_serde::to_vec_named(&k).unwrap();
            let back: Kind = rmp_serde::from_slice(&bytes).unwrap();
            assert_eq!(back, k);
            // The msgpack payload encodes the lowercase string, so an old peer's
            // decoder (which lacks these variants) fails on it.
            assert_eq!(rmp_serde::from_slice::<String>(&bytes).unwrap(), s);
        }
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
