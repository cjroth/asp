//! gitimport — the pure, wasm-safe **git-history model** for the git bridge
//! (git-bridge §3). Bytes in → deterministic in-memory model out; the row-synthesis
//! half turns that model into `LogRow`s.
//!
//! This module owns two things and nothing else:
//!
//! 1. [`GitObjectDb`] — an in-memory git object database assembled from packfile
//!    bytes ([`GitObjectDb::from_pack`], with full ofs-/ref-delta resolution via
//!    `gix-pack`), plus a `base_lookup` seam for thin packs / incremental fetches.
//! 2. [`plan_import`] — the deterministic **replay model** ([`ImportPlan`]): the
//!    commit DAG reachable from a tip, linearized by a frozen canonical topological
//!    sort, decomposed into ASP lanes (git-bridge §3.1 lane assignment), with a
//!    per-commit first-parent tree diff ([`FileOp`]s) and merge markers.
//!
//! **Everything here is a pure function of the git history** (git-bridge §3.2): no
//! filesystem, no tokio, no clock, no RNG. Two nodes that decode the same pack
//! compute byte-identical [`ImportPlan`]s. It compiles to `wasm32` unchanged —
//! `gix-pack`/`gix-object`/`gix-hash`/`gix-features` are the wasm-verified deps.
//!
//! ## Identity-bearing determinism (READ THIS — these choices are frozen, `"v1"`)
//!
//! Downstream Merkle ids key off the canonical order and lane structure, so any
//! tie-break here is load-bearing and MUST NOT change once shipped:
//!
//! * **Canonical topo order** ([`canonical_topo_sort_v1`]): parents before children;
//!   among ready commits, order by `(committer_seconds, sha)`. `sha` is unique so the
//!   order is a *total* order — no further tie-break exists or is needed.
//! * **Lane assignment** ([`plan_import`]): lane 0 is `main` = HEAD's first-parent
//!   chain. Side lanes are created by expanding merge commits **in canonical order**;
//!   for each merge, its non-first parents are handled **in parent-index order**, each
//!   walking its own first-parent chain back to the first already-assigned (or
//!   boundary) commit. Lane ids are allocated in that deterministic expansion order.
//! * **Branch naming** ([`branch_name_for`]): parsed from the consuming merge's
//!   subject; collisions dedup with `-2`, `-3`… **in lane-creation order**.
//! * **Op order within a commit**: first-parent tree diff, ops sorted **bytewise** by
//!   their resulting path (`to` for a rename, `path` otherwise). Paths are UTF-8
//!   (lossily decoded); sorting is over the UTF-8 bytes (identical to git's bytewise
//!   order for valid UTF-8 — see the open question in the module tests).
//! * **Exact-rename pairing** ([`finalize_diff_ops`]): when one blob oid vanishes at several
//!   paths and appears at several, delete/create sides are paired in **bytewise path
//!   order** (sorted deletes zipped with sorted creates); leftovers stay Delete/Create.

use std::collections::{BTreeMap, BTreeSet, BinaryHeap, HashMap, HashSet};

use gix_features::zlib;
use gix_object::{CommitRef, Kind as GixKind, TagRef, TreeRef};
use gix_pack::cache;
use gix_pack::data;
use gix_pack::data::input;

use crate::oid::content_hash;

/// The byte prefix that marks a git-LFS pointer file (git-bridge §3.3). Checked once at
/// decode time (for a spilled blob) and by [`is_lfs_pointer`] (non-spilled path).
const LFS_POINTER_PREFIX: &[u8] = b"version https://git-lfs.github.com/spec/v1";

// ===========================================================================
// Errors
// ===========================================================================

/// Every fallible entry point here returns this typed error. Hand-rolled `Display`
/// matches the rest of `asp-core`'s conventions and keeps the crate dependency-light.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitImportError {
    /// The packfile header/trailer or an entry could not be decoded.
    Pack(String),
    /// A delta object's base could not be resolved from the pack or `base_lookup`
    /// (a truly thin pack whose bases we were not given). Names the unresolved base.
    UnresolvedBase(String),
    /// An object referenced by the walk is absent from the db (and not a depth/shallow
    /// boundary). Names the missing sha.
    MissingObject(String),
    /// A git object failed to parse (commit/tree/tag decode).
    Decode(String),
    /// The requested tip is not a commit (nor a tag chain ending at one).
    NotACommit(String),
    /// The commit graph contained a cycle (impossible for real git history; a
    /// safety net for synthesized/fuzzed inputs).
    Cycle,
}

impl std::fmt::Display for GitImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GitImportError::Pack(s) => write!(f, "gitimport: pack decode: {s}"),
            GitImportError::UnresolvedBase(s) => write!(f, "gitimport: unresolved delta base {s}"),
            GitImportError::MissingObject(s) => write!(f, "gitimport: missing object {s}"),
            GitImportError::Decode(s) => write!(f, "gitimport: object decode: {s}"),
            GitImportError::NotACommit(s) => write!(f, "gitimport: {s} is not a commit"),
            GitImportError::Cycle => write!(f, "gitimport: commit graph has a cycle"),
        }
    }
}

impl std::error::Error for GitImportError {}

// ===========================================================================
// Object kinds & entry modes
// ===========================================================================

/// A git object type. Mirror of [`gix_object::Kind`] with `serde`/`Copy` and no
/// lifetime, so it can live in the object db and the plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum GitObjKind {
    Commit,
    Tree,
    Blob,
    Tag,
}

impl From<GixKind> for GitObjKind {
    fn from(k: GixKind) -> Self {
        match k {
            GixKind::Commit => GitObjKind::Commit,
            GixKind::Tree => GitObjKind::Tree,
            GixKind::Blob => GitObjKind::Blob,
            GixKind::Tag => GitObjKind::Tag,
        }
    }
}

impl From<GitObjKind> for GixKind {
    fn from(k: GitObjKind) -> Self {
        match k {
            GitObjKind::Commit => GixKind::Commit,
            GitObjKind::Tree => GixKind::Tree,
            GitObjKind::Blob => GixKind::Blob,
            GitObjKind::Tag => GixKind::Tag,
        }
    }
}

/// The kind of a tree leaf, projected onto what ASP models. Git modes map as:
/// `100644`→[`Normal`](EntryMode::Normal), `100755`→[`Executable`](EntryMode::Executable),
/// `120000`→[`Symlink`](EntryMode::Symlink), `160000`→[`Gitlink`](EntryMode::Gitlink).
/// (`040000` trees are directories, not leaves — see [`FileOp::DirCreate`].)
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
pub enum EntryMode {
    /// Regular file, `100644`.
    Normal,
    /// Executable file, `100755`. ASP doesn't model the +x bit itself; the ledger
    /// (§3.3) carries it for push fidelity — this mode is how the importer surfaces it.
    Executable,
    /// Symlink, `120000`. The referenced blob's bytes are the link target text.
    Symlink,
    /// Gitlink / submodule pointer, `160000`. Never emitted as a [`FileOp`]
    /// (materializes as nothing, git-bridge §3.3) — surfaced as an [`ImportWarning`].
    Gitlink,
}

impl EntryMode {
    /// The canonical git octal mode for this entry (e.g. `0o100644`).
    pub fn git_mode(self) -> u32 {
        match self {
            EntryMode::Normal => 0o100644,
            EntryMode::Executable => 0o100755,
            EntryMode::Symlink => 0o120000,
            EntryMode::Gitlink => 0o160000,
        }
    }
}

// ===========================================================================
// GitObjectDb — in-memory object database from pack bytes
// ===========================================================================

/// The small, byte-free locator kept for a **spilled** blob: its content lives in an
/// external [`BlobStore`](crate::store::BlobStore) (keyed by `content_hash`), and these
/// three facts — everything the downstream import + row synthesis need — are computed
/// once at decode time so no consumer ever re-reads or re-hashes the blob bytes.
#[derive(Debug, Clone)]
pub struct BlobLoc {
    /// The blob's `content_hash` (SHA-256 hex) — the key its bytes live under in the
    /// spill store, and the `result_hash` row synthesis stamps for it.
    pub content_hash: String,
    /// Whether the blob classifies as binary (`!utf8 || contains(0)`) — the one bit
    /// row synthesis needs from the bytes to pick a `MergeClass`.
    pub is_binary: bool,
    /// Whether the blob is a git-LFS pointer (git-bridge §3.3) — consulted by the
    /// tree diff to raise an `LfsPointers` warning.
    pub is_lfs: bool,
}

/// An in-memory git object database: `sha (40-hex) → (kind, decompressed payload)`.
///
/// The payload is the *object body* (the bytes after the `"<kind> <len>\0"` loose
/// header), i.e. exactly what [`gix_object::CommitRef::from_bytes`] &co. expect.
///
/// ## Blob spill (full-history clone memory ceiling)
///
/// A full clone's decompressed blob content dwarfs its commits+trees (multi-GB for a
/// big repo) and used to sit here in `objects`, OOMing the decode. When
/// [`from_pack_spilling`](GitObjectDb::from_pack_spilling) is given a spill store, blob
/// bytes stream straight into it during decode and only a byte-free [`BlobLoc`] stays in
/// `blob_meta`; `objects` then holds just commits/trees/tags. The tree diff never reads
/// blob bytes (it only records their oids), and row synthesis reads `content_hash` /
/// `is_binary` from the locator — so no consumer needs the bytes back except the (rare,
/// full-pack-absent) ref-delta whose base is a spilled blob, which reads it back from
/// the spill store. Without a spill store every object stays in `objects` as before
/// (tests, wasm, incremental pulls) — `blob_meta` stays empty.
#[derive(Debug, Default, Clone)]
pub struct GitObjectDb {
    objects: HashMap<String, (GitObjKind, Vec<u8>)>,
    blob_meta: HashMap<String, BlobLoc>,
}

/// A `base_lookup` that never resolves anything — the common case for a
/// self-contained pack (a full clone). Pass this to [`GitObjectDb::from_pack`].
pub fn no_base_lookup(_sha: &str) -> Option<(GitObjKind, Vec<u8>)> {
    None
}

impl GitObjectDb {
    /// An empty db (used by incremental builds and tests).
    pub fn new() -> Self {
        GitObjectDb { objects: HashMap::new(), blob_meta: HashMap::new() }
    }

    /// Insert a loose object by kind + body, returning its computed sha (40-hex).
    /// Handy for incremental packs, thin-pack bases, and tests that synthesize a DAG.
    pub fn insert_loose(&mut self, kind: GitObjKind, payload: &[u8]) -> Result<String, GitImportError> {
        let id = gix_object::compute_hash(gix_hash::Kind::Sha1, kind.into(), payload)
            .map_err(|e| GitImportError::Decode(e.to_string()))?;
        let hex = id.to_hex().to_string();
        self.objects.insert(hex.clone(), (kind, payload.to_vec()));
        Ok(hex)
    }

    /// Decode every object in `pack` (resolving ofs- and ref-deltas), merging into a
    /// fresh db. `base_lookup` supplies external bases for thin packs / incremental
    /// fetches (`sha → (kind, body)`); use [`no_base_lookup`] for a full clone.
    ///
    /// All-or-nothing: a torn/corrupt pack yields `Err` and no partial db (mirrors
    /// the clone's all-or-nothing genesis, git-bridge §9).
    pub fn from_pack(
        pack: &[u8],
        base_lookup: impl Fn(&str) -> Option<(GitObjKind, Vec<u8>)>,
    ) -> Result<Self, GitImportError> {
        Self::from_pack_with_progress(pack, base_lookup, |_, _, _| {})
    }

    /// Like [`from_pack`], but reports `(phase, done, total)` to `progress` as the pack
    /// is processed. Two sub-phases, each `(done, num_objects)` where `num_objects` is
    /// the pack header's object count (known up front): `"scanning"` (the initial
    /// offset-enumeration sweep) then `"replaying"` (the dominant decode stretch). The
    /// clone driver forwards the phase string straight to its progress sink.
    ///
    /// [`from_pack`]: GitObjectDb::from_pack
    pub fn from_pack_with_progress(
        pack: &[u8],
        base_lookup: impl Fn(&str) -> Option<(GitObjKind, Vec<u8>)>,
        progress: impl FnMut(&str, u64, u64),
    ) -> Result<Self, GitImportError> {
        let mut db = GitObjectDb::new();
        db.absorb_pack_with_progress(pack, base_lookup, progress)?;
        Ok(db)
    }

    /// Like [`from_pack_with_progress`], but **spills blob bytes** into `spill` (a
    /// content-addressed store) as the pack decodes, keeping only a byte-free
    /// [`BlobLoc`] per blob in the db (git-bridge full-history clone memory ceiling —
    /// see [`GitObjectDb`] docs). Takes the pack **by value** so it moves straight into
    /// the decoder with no second copy (the old `from_data(pack.to_vec())` doubled a
    /// ~600 MB pack). Commits/trees/tags stay in memory as before; the resulting plan +
    /// synthesized rows are byte-identical to the non-spilling path.
    ///
    /// [`from_pack_with_progress`]: GitObjectDb::from_pack_with_progress
    pub fn from_pack_spilling(
        pack: Vec<u8>,
        base_lookup: impl Fn(&str) -> Option<(GitObjKind, Vec<u8>)>,
        spill: &dyn crate::store::BlobStore,
        progress: impl FnMut(&str, u64, u64),
    ) -> Result<Self, GitImportError> {
        let mut db = GitObjectDb::new();
        db.decode_pack(pack, base_lookup, Some(spill), progress)?;
        Ok(db)
    }

    /// Decode `pack` into an existing db (incremental fetch). See [`from_pack`].
    ///
    /// [`from_pack`]: GitObjectDb::from_pack
    pub fn absorb_pack(
        &mut self,
        pack: &[u8],
        base_lookup: impl Fn(&str) -> Option<(GitObjKind, Vec<u8>)>,
    ) -> Result<(), GitImportError> {
        self.absorb_pack_with_progress(pack, base_lookup, |_, _, _| {})
    }

    /// Like [`absorb_pack`], but reports `(phase, done, total)` to `progress`: the
    /// `"scanning"` offset-enumeration sweep then the `"replaying"` decode, each against
    /// the pack header's object count. Coarse — emitted every ~1000 entries plus a final
    /// tick — so a huge pack can't flood the sink (these are hot, just-optimized loops).
    ///
    /// [`absorb_pack`]: GitObjectDb::absorb_pack
    pub fn absorb_pack_with_progress(
        &mut self,
        pack: &[u8],
        base_lookup: impl Fn(&str) -> Option<(GitObjKind, Vec<u8>)>,
        progress: impl FnMut(&str, u64, u64),
    ) -> Result<(), GitImportError> {
        // No spill store: every object (blobs included) stays in `objects`, as before.
        // A borrowed slice must be owned for gix's random-access decoder, so this path
        // pays one `to_vec` (fine for the small packs its callers pass — tests and
        // incremental pulls); the full-clone path uses `from_pack_spilling` (by value).
        self.decode_pack(pack.to_vec(), base_lookup, None, progress)
    }

    /// The single pack-decode implementation, shared by the spilling and non-spilling
    /// entry points. Owns `pack` so it moves into the decoder with no copy. When
    /// `spill` is `Some`, blob bodies stream into it (keyed by `content_hash`) and only
    /// a [`BlobLoc`] is kept in `blob_meta`; otherwise every object stays in `objects`.
    fn decode_pack(
        &mut self,
        pack: Vec<u8>,
        base_lookup: impl Fn(&str) -> Option<(GitObjKind, Vec<u8>)>,
        spill: Option<&dyn crate::store::BlobStore>,
        mut progress: impl FnMut(&str, u64, u64),
    ) -> Result<(), GitImportError> {
        // Object count lives in the pack header ("PACK"(4) + version(4) + count(4),
        // big-endian) — known before the scan, so the "scanning" bar shows its
        // denominator from the first tick. A truncated header yields 0 (indeterminate),
        // and the stream parse below will surface the real corruption error.
        const PROGRESS_STRIDE: u64 = 1000;
        let header_objects: u64 = if pack.len() >= 12 {
            u32::from_be_bytes([pack[8], pack[9], pack[10], pack[11]]) as u64
        } else {
            0
        };
        // Phase 1 — enumerate entry offsets (the pack has no index, so we stream the
        // header to learn where each entry begins). Data is ignored here; phase 2
        // re-reads each entry by offset with full delta resolution.
        let iter = input::BytesToEntriesIter::new_from_header(
            std::io::Cursor::new(pack.as_slice()),
            input::Mode::AsIs,
            input::EntryDataMode::Ignore,
            gix_hash::Kind::Sha1,
        )
        .map_err(|e| GitImportError::Pack(e.to_string()))?;

        progress("scanning", 0, header_objects);
        let mut offsets: Vec<u64> = Vec::new();
        for entry in iter {
            let entry = entry.map_err(|e| GitImportError::Pack(e.to_string()))?;
            offsets.push(entry.pack_offset);
            if (offsets.len() as u64).is_multiple_of(PROGRESS_STRIDE) {
                progress("scanning", offsets.len() as u64, header_objects);
            }
        }
        progress("scanning", offsets.len() as u64, header_objects);
        // Ascending offset order means non-delta bases and ofs-delta bases (which
        // always precede their deltas in the pack) are decoded before dependents,
        // so the fixpoint below usually converges in a single pass.
        offsets.sort_unstable();

        // Total object count (pack header) drives the "replaying" progress bar; each
        // successful decode below bumps `decoded`. Emit an initial (0, n) so the phase
        // shows its denominator immediately.
        let num_objects = offsets.len() as u64;
        let mut decoded: u64 = 0;
        progress("replaying", 0, num_objects);

        // Move the pack straight into the decoder (`iter` above borrowed it only for the
        // scan and is now dropped) — no second full copy of a ~600 MB pack.
        let file = data::File::from_data(pack, "<memory>".into(), gix_hash::Kind::Sha1)
            .map_err(|e| GitImportError::Pack(e.to_string()))?;
        let mut inflate = zlib::Inflate::default();

        // Delta-base cache. Git objects are delta-compressed into chains up to ~50
        // deep; with `cache::Never` every delta re-inflates its ENTIRE base chain
        // from scratch, so decode time scales with pack size (a top-3 clone cost).
        // gix keys this cache by `(pack_id, in-pack offset)` and short-circuits a
        // chain at the first cached ancestor (`resolve_deltas`), so a bounded LRU
        // over recently-decoded bases turns O(chain) re-inflation into O(1) hits.
        // We decode offset-ascending and ofs-delta bases always point backward, so
        // a base is decoded (and cached) before every delta that consumes it.
        //
        // The cache only stores/returns already-decoded bytes — the reconstructed
        // object bytes (and thus every synthesized SHA / fold) are byte-identical to
        // the `cache::Never` path. It is memory-CAPPED (not count-capped) so a huge
        // pack can't blow the budget; a full clone already peaks multi-GB and this
        // adds a bounded slice on top.
        //
        // Single instance for the whole (single-threaded) loop maximizes reuse. The
        // `pack-cache-lru-dynamic`/`clru` backing is pure-Rust and wasm-safe (no
        // getrandom, no on-disk index, no parallelism), so the same path serves
        // native + wasm32.
        const DELTA_CACHE_CAP_BYTES: usize = 128 * 1024 * 1024;
        let mut cache = cache::lru::MemoryCappedHashmap::new(DELTA_CACHE_CAP_BYTES);

        // Spill buffer (only used when `spill` is Some): decoded blob bodies accumulate
        // here as `(content_hash, bytes)` and flush to the store in batches, so a
        // disk-backed store commits ONE transaction per ~32 MB instead of one per blob
        // (millions of autocommits would otherwise dominate a full-history clone). A
        // ref-delta whose base is an already-spilled blob (rare — full packs use
        // ofs-deltas) reads the base back through `resolve` below (from the buffer if it
        // hasn't flushed yet, else from the store).
        const SPILL_FLUSH_BYTES: usize = 32 * 1024 * 1024;
        let mut spill_buf: Vec<(String, Vec<u8>)> = Vec::new();
        let mut spill_buf_bytes: usize = 0;

        // Phase 2 — decode each entry, retrying deltas whose (ref-)base isn't decoded
        // yet, until either everything resolves or a pass makes no progress.
        let mut pending = offsets;
        while !pending.is_empty() {
            let mut progressed = false;
            let mut still: Vec<u64> = Vec::new();
            let mut last_unresolved: Option<String> = None;

            for off in pending {
                let entry = file
                    .entry(off)
                    .map_err(|e| GitImportError::Pack(e.to_string()))?;
                let mut out: Vec<u8> = Vec::new();
                // Interior mutability so `resolve` stays `Fn` (decode_entry requires it).
                let missing_base: std::cell::RefCell<Option<String>> = std::cell::RefCell::new(None);
                let decoded_ref = &self.objects;
                let blob_meta_ref = &self.blob_meta;
                let spill_buf_ref = &spill_buf;
                let resolve = |id: &gix_hash::oid, buf: &mut Vec<u8>| -> Option<data::decode::entry::ResolvedBase> {
                    let hex = id.to_hex().to_string();
                    if let Some((k, bytes)) = decoded_ref.get(&hex) {
                        buf.extend_from_slice(bytes);
                        return Some(data::decode::entry::ResolvedBase::OutOfPack {
                            kind: (*k).into(),
                            end: buf.len(),
                        });
                    }
                    // A spilled blob base (its bytes are no longer in `objects`): serve it
                    // from the unflushed buffer if still there, else read it back from the
                    // spill store. Only reached for a ref-delta onto a blob — negligible on
                    // a full pack, so the linear buffer scan here is not hot.
                    if let (Some(loc), Some(store)) = (blob_meta_ref.get(&hex), spill) {
                        if let Some((_, bytes)) = spill_buf_ref.iter().find(|(h, _)| *h == loc.content_hash) {
                            buf.extend_from_slice(bytes);
                            return Some(data::decode::entry::ResolvedBase::OutOfPack {
                                kind: GixKind::Blob,
                                end: buf.len(),
                            });
                        }
                        if let Ok(Some(bytes)) = store.get_blob(&loc.content_hash) {
                            buf.extend_from_slice(&bytes);
                            return Some(data::decode::entry::ResolvedBase::OutOfPack {
                                kind: GixKind::Blob,
                                end: buf.len(),
                            });
                        }
                    }
                    if let Some((k, bytes)) = base_lookup(&hex) {
                        buf.extend_from_slice(&bytes);
                        return Some(data::decode::entry::ResolvedBase::OutOfPack {
                            kind: k.into(),
                            end: buf.len(),
                        });
                    }
                    *missing_base.borrow_mut() = Some(hex);
                    None
                };
                let decode_result =
                    file.decode_entry(entry, &mut out, &mut inflate, &resolve, &mut cache);
                match decode_result {
                    Ok(outcome) => {
                        let kind: GitObjKind = outcome.kind.into();
                        let id = gix_object::compute_hash(gix_hash::Kind::Sha1, outcome.kind, &out)
                            .map_err(|e| GitImportError::Decode(e.to_string()))?;
                        let hex = id.to_hex().to_string();
                        match (kind, spill) {
                            // Spill blob bytes out of RAM: compute the three facts
                            // consumers need (content_hash / is_binary / is_lfs) once,
                            // buffer the bytes for a batched store write, and keep only
                            // the byte-free locator. Commits/trees still live in `objects`.
                            (GitObjKind::Blob, Some(_)) => {
                                let content_hash = content_hash(&out);
                                let is_binary =
                                    std::str::from_utf8(&out).is_err() || out.contains(&0);
                                let is_lfs = out.starts_with(LFS_POINTER_PREFIX);
                                self.blob_meta.insert(
                                    hex,
                                    BlobLoc { content_hash: content_hash.clone(), is_binary, is_lfs },
                                );
                                spill_buf_bytes += out.len();
                                spill_buf.push((content_hash, out));
                            }
                            _ => {
                                self.objects.insert(hex, (kind, out));
                            }
                        }
                        progressed = true;
                        decoded += 1;
                        // Coarse: don't fire the sink per object (a big pack is 100k+).
                        if decoded.is_multiple_of(PROGRESS_STRIDE) {
                            progress("replaying", decoded, num_objects);
                        }
                    }
                    Err(_) => {
                        if let Some(b) = missing_base.into_inner() {
                            last_unresolved = Some(b);
                        }
                        still.push(off);
                    }
                }

                // Flush the spill buffer once it crosses the batch threshold. Safe here:
                // `resolve` (which borrows `spill_buf`) was dropped when `decode_entry`
                // returned above, so the buffer is free to drain.
                if spill_buf_bytes >= SPILL_FLUSH_BYTES {
                    if let Some(store) = spill {
                        store
                            .put_blobs_with_hash_owned(std::mem::take(&mut spill_buf))
                            .map_err(|e| GitImportError::Pack(e.to_string()))?;
                        spill_buf_bytes = 0;
                    }
                }
            }

            if !progressed {
                return Err(GitImportError::UnresolvedBase(
                    last_unresolved.unwrap_or_else(|| "<unknown>".into()),
                ));
            }
            pending = still;
        }
        // Drain any remaining spilled blobs.
        if let Some(store) = spill {
            if !spill_buf.is_empty() {
                store
                    .put_blobs_with_hash_owned(std::mem::take(&mut spill_buf))
                    .map_err(|e| GitImportError::Pack(e.to_string()))?;
            }
        }
        // Final tick — the stride above can leave the last <1000 objects unreported.
        progress("replaying", decoded, num_objects);
        Ok(())
    }

    /// Look up an object body by sha (40-hex).
    pub fn get(&self, sha: &str) -> Option<(GitObjKind, &[u8])> {
        self.objects.get(sha).map(|(k, v)| (*k, v.as_slice()))
    }

    /// Number of objects held (commits/trees/tags; spilled blobs are counted in
    /// [`blob_count`](GitObjectDb::blob_count) instead).
    pub fn len(&self) -> usize {
        self.objects.len()
    }

    /// Whether the db is empty.
    pub fn is_empty(&self) -> bool {
        self.objects.is_empty() && self.blob_meta.is_empty()
    }

    /// Number of spilled blobs (empty on the non-spilling path — those blobs sit in
    /// `objects` and count toward [`len`](GitObjectDb::len)).
    pub fn blob_count(&self) -> usize {
        self.blob_meta.len()
    }

    /// The precomputed `git blob sha -> (content_hash, is_binary)` map for **spilled**
    /// blobs, or `None` when nothing was spilled (non-spilling decode). Row synthesis
    /// consumes this to stamp `result_hash` / pick `MergeClass` without touching a
    /// single blob byte (the bytes already live, content-addressed, in the spill store).
    pub fn spilled_blob_meta(&self) -> Option<HashMap<String, (String, bool)>> {
        if self.blob_meta.is_empty() {
            return None;
        }
        Some(
            self.blob_meta
                .iter()
                .map(|(sha, loc)| (sha.clone(), (loc.content_hash.clone(), loc.is_binary)))
                .collect(),
        )
    }

    /// Iterate all `(sha, kind)` pairs (sha-sorted for deterministic traversal).
    pub fn iter_shas(&self) -> impl Iterator<Item = (&str, GitObjKind)> {
        let mut v: Vec<_> = self.objects.iter().map(|(s, (k, _))| (s.as_str(), *k)).collect();
        v.sort_unstable_by(|a, b| a.0.cmp(b.0));
        v.into_iter()
    }

    // --- internal typed accessors -----------------------------------------

    fn commit(&self, sha: &str) -> Result<CommitRef<'_>, GitImportError> {
        let (k, bytes) = self.get(sha).ok_or_else(|| GitImportError::MissingObject(sha.to_string()))?;
        if k != GitObjKind::Commit {
            return Err(GitImportError::NotACommit(sha.to_string()));
        }
        CommitRef::from_bytes(bytes, gix_hash::Kind::Sha1).map_err(|e| GitImportError::Decode(e.to_string()))
    }

    fn tree(&self, sha: &str) -> Result<TreeRef<'_>, GitImportError> {
        let (k, bytes) = self.get(sha).ok_or_else(|| GitImportError::MissingObject(sha.to_string()))?;
        if k != GitObjKind::Tree {
            return Err(GitImportError::Decode(format!("{sha} is not a tree")));
        }
        TreeRef::from_bytes(bytes, gix_hash::Kind::Sha1).map_err(|e| GitImportError::Decode(e.to_string()))
    }

    /// Peel a tag chain to the underlying commit sha (a no-op for a commit).
    pub fn peel_to_commit(&self, sha: &str) -> Result<String, GitImportError> {
        let mut cur = sha.to_string();
        loop {
            let (k, bytes) = self.get(&cur).ok_or_else(|| GitImportError::MissingObject(cur.clone()))?;
            match k {
                GitObjKind::Commit => return Ok(cur),
                GitObjKind::Tag => {
                    let tag = TagRef::from_bytes(bytes, gix_hash::Kind::Sha1)
                        .map_err(|e| GitImportError::Decode(e.to_string()))?;
                    cur = tag.target().to_hex().to_string();
                }
                _ => return Err(GitImportError::NotACommit(cur)),
            }
        }
    }
}

// ===========================================================================
// Plan model
// ===========================================================================

/// A lane index. Lane 0 is always `main` (HEAD's first-parent chain).
pub type LaneId = usize;

/// The `main` lane (HEAD's first-parent chain), git-bridge §3.1.
pub const MAIN_LANE: LaneId = 0;

/// A merge edge on a merge commit: one per non-first parent (octopus → several).
/// The row-synthesis layer emits a `Kind::Merge` row per `MergeInfo`, in order.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MergeInfo {
    /// The lane the merged-in side branch lives on.
    pub source_lane: LaneId,
    /// The tip commit of the merged-in side (this merge's non-first parent).
    pub source_tip_sha: String,
}

/// Where a side lane forks off its parent lane (git-bridge §3.1 `fork_vv` placement).
/// `None` on a lane whose first commit has no in-plan first parent (a repo root, a
/// grafted unrelated root, or a depth-cut boundary).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ForkPoint {
    /// The lane forked from.
    pub lane: LaneId,
    /// The fork-base commit (already assigned on `lane`).
    pub commit_sha: String,
    /// Index of the fork-base commit in [`ImportPlan::commits`] (canonical order).
    pub commit_index: usize,
}

/// A single file-level change in a commit's first-parent tree diff (git-bridge §3.1).
/// `blob_sha` values reference blobs in the [`GitObjectDb`]; for a [`Symlink`] the
/// blob's bytes are the link target text.
///
/// [`Symlink`]: EntryMode::Symlink
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum FileOp {
    /// Path added.
    Create { path: String, blob_sha: String, mode: EntryMode },
    /// Path modified (content and/or mode). A pure mode flip carries
    /// `old_blob_sha == blob_sha` with `old_mode != mode`.
    Edit {
        path: String,
        old_blob_sha: String,
        blob_sha: String,
        mode: EntryMode,
        old_mode: EntryMode,
    },
    /// Path removed.
    Delete { path: String },
    /// Exact rename: the same blob oid left `from` and appeared at `to`
    /// (content-similarity renames stay Delete+Create, git-bridge §3.1).
    RenameExact { from: String, to: String, blob_sha: String, mode: EntryMode },
    /// A new, otherwise-empty directory (a git tree with no blob descendants).
    /// ASP materializes real dirs; ordinary non-empty dirs are implied by their
    /// files' [`Create`](FileOp::Create)s and get no `DirCreate`.
    DirCreate { path: String },
}

impl FileOp {
    /// The bytewise sort key that orders ops within a commit's batch (§3.2 seq
    /// allocation): the resulting path (`to` for a rename, `path` otherwise).
    pub fn sort_key(&self) -> &str {
        match self {
            FileOp::Create { path, .. } => path,
            FileOp::Edit { path, .. } => path,
            FileOp::Delete { path } => path,
            FileOp::RenameExact { to, .. } => to,
            FileOp::DirCreate { path } => path,
        }
    }
}

/// One commit's fully-derived import batch (git-bridge §3.1).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PlannedCommit {
    /// The git commit sha (40-hex). For a depth-cut snapshot this is the cut-point sha.
    pub sha: String,
    /// The assigned lane.
    pub lane: LaneId,
    /// All parent shas, first-parent first (empty for a root / the snapshot).
    pub parents: Vec<String>,
    /// Merge edges (empty for a non-merge). One per non-first parent, parent order.
    pub merges: Vec<MergeInfo>,
    pub author_name: String,
    pub author_email: String,
    /// Committer timestamp in **milliseconds** (git-bridge §3.1 `ts`).
    pub committer_ts_ms: i64,
    /// Full message (subject, then a blank line + body when a body is present).
    pub message: String,
    /// First-parent tree diff, ops sorted bytewise by [`FileOp::sort_key`].
    pub ops: Vec<FileOp>,
    /// `true` for the synthetic snapshot batch that fronts a `--depth` import
    /// (git-bridge §3.4): `ops` is the full tree at the cut point, `parents` empty.
    pub is_depth_cut_snapshot: bool,
}

/// A synthesized ASP branch (git-bridge §3.1 lane assignment).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PlannedLane {
    pub id: LaneId,
    /// `"main"` for lane 0; otherwise derived from the consuming merge subject
    /// (with `-2`/`-3`… dedup) — see [`branch_name_for`].
    pub name: String,
    /// Where this lane forks off its parent lane (`None` for `main` / boundary roots).
    pub fork: Option<ForkPoint>,
    /// The lane's first (oldest) commit — where its create record is authored.
    pub created_at_commit: String,
    /// The merge commit that consumes this lane (`None` for `main` and for every
    /// **live** open-branch lane — an unmerged branch is never consumed). The delete
    /// record lands right after this merge's marker.
    pub merged_at_commit: Option<String>,
    /// Emit a branch-delete record right after the merge marker (git-bridge §3.1;
    /// `false` for `main`, for a live open branch, and for every lane when
    /// `keep_imported_branches`).
    pub deleted_after_merge: bool,
    /// This lane is a **live imported open branch** (`specs/git-open-branches.md`
    /// §1–§2): a create record, no delete tombstone. `false` for `main`, for merged
    /// side lanes (phase 1), and for the internal sub-lanes of an open branch (which
    /// ARE tombstoned relative to their branch). The emission layer / clone report
    /// reads this to count/identify the imported open branches.
    pub live: bool,
}

/// A degraded/skipped-content notice surfaced in the clone report (git-bridge §3.3).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ImportWarning {
    /// A gitlink/submodule entry — materialized as nothing; `.gitmodules` still
    /// imports as a normal file.
    Submodule { path: String, target_sha: String },
    /// One or more git-LFS pointer files were imported as their pointer text
    /// (one notice per repo, git-bridge §3.3).
    LfsPointers { paths: Vec<String> },
}

/// Options for [`plan_import`].
///
/// `Default` = full DAG import (`depth: None`) with delete-after-merge records and
/// **no** open branches (phase 1 only). All existing behavior is byte-identical to a
/// plan without the [`open_branch_tips`](ImportOptions::open_branch_tips) field when
/// that vec is empty (the load-bearing zero-regression property).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportOptions {
    /// Keep only the last `n` first-parent commits of `main` + side ancestry merged
    /// within that window, fronted by one synthetic snapshot (git-bridge §3.4).
    /// `None` = full DAG. Determinism holds for equal `depth`.
    pub depth: Option<u32>,
    /// Skip the delete-after-merge records so imported branches stay live
    /// (`git.keep_imported_branches`, git-bridge §3.1).
    pub keep_imported_branches: bool,
    /// Open (unmerged) branch tips to import as **live** ASP branches — phase 2 of
    /// genesis (`specs/git-open-branches.md` §1–§2). Each entry is
    /// `(ref_name, tip_sha)`; the ref name (e.g. `"cjroth/acp"`) becomes the live
    /// branch's name verbatim (deduped `-2`/`-3` against phase-1 names). **Empty =
    /// phase-1-only, byte-identical to the base spec.** Import order is canonical:
    /// **ref name bytewise** (frozen tie-break).
    pub open_branch_tips: Vec<(String, String)>,
}

/// The deterministic replay model everything in git-bridge §3 derives from.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ImportPlan {
    /// Commits in canonical topo order (git-bridge §3.1): parents first; ready set
    /// ordered by `(committer_seconds, sha)`.
    pub commits: Vec<PlannedCommit>,
    /// Lane 0 is `main`; side lanes follow in creation order.
    pub lanes: Vec<PlannedLane>,
    /// The root of the plan's `main` first-parent chain — the key for `site_id` /
    /// `vault_id` derivation (git-bridge §3.2). Under `--depth` this is the cut-point
    /// sha (a depth clone is intentionally a distinct baseline).
    pub root_sha: String,
    /// The imported tip (the peeled commit of the requested tip).
    pub tip_sha: String,
    /// Degraded/skipped content, deterministically ordered.
    pub warnings: Vec<ImportWarning>,
    /// Open-branch ref names skipped because their tip is already emitted — reachable
    /// from HEAD (old release pointers, just-merged branches) or from an earlier
    /// open branch (`specs/git-open-branches.md` §1). In the ref-name-bytewise import
    /// order. Empty when `open_branch_tips` is empty. The clone report surfaces the
    /// count (`refs_skipped_reachable`).
    pub skipped_reachable: Vec<String>,
}

// ===========================================================================
// Canonical topological sort ("v1", FROZEN)
// ===========================================================================

/// A min-heap entry ordered by `(committer_seconds, sha)`.
#[derive(PartialEq, Eq)]
struct Ready {
    ts: i64,
    sha: String,
}
impl Ord for Ready {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Reversed for a min-heap on (ts, sha).
        other.ts.cmp(&self.ts).then_with(|| other.sha.cmp(&self.sha))
    }
}
impl PartialOrd for Ready {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// The frozen canonical topological sort (git-bridge §3.1 `"v1"`).
///
/// `nodes` maps each in-plan commit sha to `(parents_within_plan, committer_seconds)`
/// — parents already filtered to shas present in `nodes` (out-of-plan parents are
/// boundaries and must be dropped by the caller). Returns the linearization with
/// parents strictly before children; among commits whose parents are all emitted,
/// the one with the smallest `(committer_seconds, sha)` goes first. Because `sha` is
/// unique this is a **total** order — the property downstream Merkle ids rely on.
pub fn canonical_topo_sort_v1(
    nodes: &BTreeMap<String, (Vec<String>, i64)>,
) -> Result<Vec<String>, GitImportError> {
    // Children adjacency + in-degree (number of in-plan parents).
    let mut children: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut indegree: HashMap<&str, usize> = HashMap::new();
    for sha in nodes.keys() {
        indegree.entry(sha.as_str()).or_insert(0);
    }
    for (sha, (parents, _)) in nodes {
        *indegree.entry(sha.as_str()).or_insert(0) += parents.len();
        for p in parents {
            children.entry(p.as_str()).or_default().push(sha.as_str());
        }
    }

    let mut heap: BinaryHeap<Ready> = BinaryHeap::new();
    for (sha, (_, ts)) in nodes {
        if indegree[sha.as_str()] == 0 {
            heap.push(Ready { ts: *ts, sha: sha.clone() });
        }
    }

    let mut order: Vec<String> = Vec::with_capacity(nodes.len());
    while let Some(Ready { sha, .. }) = heap.pop() {
        if let Some(kids) = children.get(sha.as_str()) {
            // Collect first to avoid borrowing `children` while mutating the heap.
            let kids: Vec<&str> = kids.clone();
            for c in kids {
                let d = indegree.get_mut(c).unwrap();
                *d -= 1;
                if *d == 0 {
                    let ts = nodes[c].1;
                    heap.push(Ready { ts, sha: c.to_string() });
                }
            }
        }
        order.push(sha);
    }

    if order.len() != nodes.len() {
        return Err(GitImportError::Cycle);
    }
    Ok(order)
}

// ===========================================================================
// Branch naming (pure function of the consuming merge)
// ===========================================================================

/// Derive a side-branch **base name** from its consuming merge's `subject` and its
/// side tip sha (git-bridge §3.1). Collision dedup (`-2`, `-3`…) is applied by
/// [`plan_import`] over lane-creation order — this function is collision-free.
///
/// Patterns (in order):
/// * `Merge pull request #N from owner/branch` → `branch` (everything after the
///   first `/`; the branch part may itself contain slashes).
/// * `Merge branch 'name'` (optionally `… into other`) → `name`.
/// * otherwise → `git/<first 7 hex of the side tip sha>`.
pub fn branch_name_for(subject: &str, side_tip_sha: &str) -> String {
    let s = subject.trim();
    if let Some(name) = parse_pull_request(s) {
        return name;
    }
    if let Some(name) = parse_merge_branch(s) {
        return name;
    }
    format!("git/{}", &side_tip_sha[..side_tip_sha.len().min(7)])
}

/// `Merge pull request #<n> from <owner>/<branch…>` → `Some(branch…)`.
fn parse_pull_request(s: &str) -> Option<String> {
    let rest = s.strip_prefix("Merge pull request #")?;
    let mut chars = rest.char_indices();
    // Require at least one digit for the PR number.
    let mut saw_digit = false;
    let mut from_idx = None;
    for (i, c) in chars.by_ref() {
        if c.is_ascii_digit() {
            saw_digit = true;
            continue;
        }
        from_idx = Some(i);
        break;
    }
    if !saw_digit {
        return None;
    }
    let after_num = &rest[from_idx?..];
    let owner_branch = after_num.strip_prefix(" from ")?.trim();
    // "owner/branch..." → branch part after the first '/'.
    let (_owner, branch) = owner_branch.split_once('/')?;
    if branch.is_empty() {
        None
    } else {
        Some(branch.to_string())
    }
}

/// `Merge branch '<name>'` (optionally ` into <x>`) → `Some(name)`.
fn parse_merge_branch(s: &str) -> Option<String> {
    let rest = s.strip_prefix("Merge branch '")?;
    let end = rest.find('\'')?;
    let name = &rest[..end];
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

// ===========================================================================
// Tree snapshots & first-parent diff
// ===========================================================================

/// The leaf [`EntryMode`] for a git tree entry kind, or `None` for trees / gitlinks
/// (which are not blob leaves).
fn leaf_mode(kind: gix_object::tree::EntryKind) -> Option<EntryMode> {
    use gix_object::tree::EntryKind::*;
    match kind {
        Blob => Some(EntryMode::Normal),
        BlobExecutable => Some(EntryMode::Executable),
        Link => Some(EntryMode::Symlink),
        Tree | Commit => None,
    }
}

/// A tree entry's raw name as bytes (deref-coerced from `&BStr`), borrowed from the
/// underlying object bytes so it can key a lookup map across the diff.
fn entry_name<'a>(e: &gix_object::tree::EntryRef<'a>) -> &'a [u8] {
    e.filename
}

/// Raw changed leaves accumulated by the recursive tree diff, before rename pairing.
#[derive(Default)]
struct DiffAccum {
    /// (path, blob sha, mode) present only in the parent tree.
    deletes: Vec<(String, String, EntryMode)>,
    /// (path, blob sha, mode) present only in the child tree.
    creates: Vec<(String, String, EntryMode)>,
    /// Ready-made [`FileOp::Edit`]s (path in both, content and/or mode differ).
    edits: Vec<FileOp>,
    /// Paths of new, blob-less directories (a child subtree with no blob descendants).
    dir_creates: Vec<String>,
}

fn join_path(prefix: &str, name: &[u8]) -> String {
    let n = String::from_utf8_lossy(name);
    if prefix.is_empty() {
        n.into_owned()
    } else {
        format!("{prefix}/{n}")
    }
}

impl GitObjectDb {
    /// Record a child blob leaf as a create, plus an LFS notice if it is a pointer.
    /// `is_lfs_pointer` is gated on the mode exactly as the old flat scan (symlinks are
    /// never LFS-checked).
    fn note_added_blob(
        &self,
        path: String,
        sha: String,
        mode: EntryMode,
        creates: &mut Vec<(String, String, EntryMode)>,
        lfs_paths: &mut BTreeSet<String>,
    ) {
        if matches!(mode, EntryMode::Normal | EntryMode::Executable) && is_lfs_pointer(self, &sha) {
            lfs_paths.insert(path.clone());
        }
        creates.push((path, sha, mode));
    }

    /// Walk a wholly-new child subtree (`tree_sha` at `prefix`): emit a create for every
    /// blob leaf, a gitlink warning for every submodule, an LFS notice for every pointer
    /// blob, and a [`FileOp::DirCreate`] for every blob-less directory strictly inside
    /// it. Returns whether the subtree has any blob descendant, so the caller decides the
    /// DirCreate for its root. This is the O(added-content) walk that fronts a root /
    /// depth-cut snapshot / out-of-plan-parent commit.
    fn walk_added_subtree(
        &self,
        tree_sha: &str,
        prefix: &str,
        acc: &mut DiffAccum,
        gitlink_warns: &mut BTreeMap<String, String>,
        lfs_paths: &mut BTreeSet<String>,
    ) -> Result<bool, GitImportError> {
        let tree = self.tree(tree_sha)?;
        let mut has_blob = false;
        for e in &tree.entries {
            let path = join_path(prefix, e.filename);
            let oid = e.oid.to_hex().to_string();
            use gix_object::tree::EntryKind::*;
            match e.mode.kind() {
                Tree => {
                    let child_has = self.walk_added_subtree(&oid, &path, acc, gitlink_warns, lfs_paths)?;
                    if !child_has {
                        acc.dir_creates.push(path);
                    }
                    has_blob |= child_has;
                }
                Commit => {
                    gitlink_warns.entry(path).or_insert(oid);
                }
                _ => {
                    let mode = leaf_mode(e.mode.kind()).expect("blob leaf");
                    self.note_added_blob(path, oid, mode, &mut acc.creates, lfs_paths);
                    has_blob = true;
                }
            }
        }
        Ok(has_blob)
    }

    /// Walk a wholly-removed parent subtree: emit a delete for every blob leaf. Gitlinks
    /// and directories produce no ops (ASP has no submodule / directory-delete op — an
    /// emptied directory is implied by its files' deletes).
    fn walk_removed_subtree(
        &self,
        tree_sha: &str,
        prefix: &str,
        acc: &mut DiffAccum,
    ) -> Result<(), GitImportError> {
        let tree = self.tree(tree_sha)?;
        for e in &tree.entries {
            let path = join_path(prefix, e.filename);
            use gix_object::tree::EntryKind::*;
            match e.mode.kind() {
                Tree => self.walk_removed_subtree(&e.oid.to_hex().to_string(), &path, acc)?,
                Commit => {}
                _ => {
                    let mode = leaf_mode(e.mode.kind()).expect("blob leaf");
                    acc.deletes.push((path, e.oid.to_hex().to_string(), mode));
                }
            }
        }
        Ok(())
    }

    /// Recursively diff two DIFFERING trees git-style — **skipping equal-oid subtrees** —
    /// accumulating exactly the changed-entry set the old flat diff produced. Reads tree
    /// objects straight from the db and recurses only into subtrees whose oid differs, so
    /// the cost is O(changed entries × depth), never O(tree). Gitlink/LFS warnings are
    /// collected from the child side of each change: this is equivalent to the old
    /// full-tree scan because the earliest commit (canonical order) to contain an entry
    /// is also the earliest whose first-parent diff introduces it (parents precede
    /// children), and both use `or_insert` / set-membership semantics.
    fn collect_tree_diff(
        &self,
        parent_tree: &str,
        child_tree: &str,
        prefix: &str,
        acc: &mut DiffAccum,
        gitlink_warns: &mut BTreeMap<String, String>,
        lfs_paths: &mut BTreeSet<String>,
    ) -> Result<(), GitImportError> {
        use gix_object::tree::EntryKind::*;
        let ptree = self.tree(parent_tree)?;
        // Index parent entries by raw name; matched child entries consume them, and the
        // leftovers are deletes. The `&[u8]` keys and `EntryRef` values borrow the tree
        // object bytes (owned by the db), so they outlive this call.
        let mut p_by_name: HashMap<&[u8], gix_object::tree::EntryRef<'_>> =
            HashMap::with_capacity(ptree.entries.len());
        for e in &ptree.entries {
            p_by_name.insert(entry_name(e), *e);
        }

        let ctree = self.tree(child_tree)?;
        for ce in &ctree.entries {
            let path = join_path(prefix, ce.filename);
            let coid = ce.oid.to_hex().to_string();
            let ck = ce.mode.kind();
            match p_by_name.remove(entry_name(ce)) {
                None => match ck {
                    // Brand-new name in the child.
                    Tree => {
                        let hb = self.walk_added_subtree(&coid, &path, acc, gitlink_warns, lfs_paths)?;
                        if !hb {
                            acc.dir_creates.push(path);
                        }
                    }
                    Commit => {
                        gitlink_warns.entry(path).or_insert(coid);
                    }
                    _ => {
                        let mode = leaf_mode(ck).expect("blob leaf");
                        self.note_added_blob(path, coid, mode, &mut acc.creates, lfs_paths);
                    }
                },
                Some(pe) => {
                    let poid = pe.oid.to_hex().to_string();
                    match (pe.mode.kind(), ck) {
                        (Tree, Tree) => {
                            if poid != coid {
                                self.collect_tree_diff(&poid, &coid, &path, acc, gitlink_warns, lfs_paths)?;
                            }
                        }
                        (Commit, Commit) => {
                            // Both gitlinks: a changed target re-notes (`or_insert` keeps
                            // the earliest); an identical one was noted upstream already.
                            if poid != coid {
                                gitlink_warns.entry(path).or_insert(coid);
                            }
                        }
                        (Tree, Commit) => {
                            self.walk_removed_subtree(&poid, &path, acc)?;
                            gitlink_warns.entry(path).or_insert(coid);
                        }
                        (Tree, _) => {
                            // directory → blob leaf
                            self.walk_removed_subtree(&poid, &path, acc)?;
                            let mode = leaf_mode(ck).expect("blob leaf");
                            self.note_added_blob(path, coid, mode, &mut acc.creates, lfs_paths);
                        }
                        (Commit, Tree) => {
                            let hb = self.walk_added_subtree(&coid, &path, acc, gitlink_warns, lfs_paths)?;
                            if !hb {
                                acc.dir_creates.push(path);
                            }
                        }
                        (_, Tree) => {
                            // blob leaf → directory
                            let mode = leaf_mode(pe.mode.kind()).expect("blob leaf");
                            acc.deletes.push((path.clone(), poid, mode));
                            let hb = self.walk_added_subtree(&coid, &path, acc, gitlink_warns, lfs_paths)?;
                            if !hb {
                                acc.dir_creates.push(path);
                            }
                        }
                        (Commit, _) => {
                            // gitlink → blob leaf: the vanished gitlink was noted where it
                            // was introduced; the blob is a plain create.
                            let mode = leaf_mode(ck).expect("blob leaf");
                            self.note_added_blob(path, coid, mode, &mut acc.creates, lfs_paths);
                        }
                        (_, Commit) => {
                            // blob leaf → gitlink: delete the blob, note the submodule.
                            let mode = leaf_mode(pe.mode.kind()).expect("blob leaf");
                            acc.deletes.push((path.clone(), poid, mode));
                            gitlink_warns.entry(path).or_insert(coid);
                        }
                        (_, _) => {
                            // both blob leaves (Normal / Executable / Symlink, any mix)
                            let cmode = leaf_mode(ck).expect("blob leaf");
                            let pmode = leaf_mode(pe.mode.kind()).expect("blob leaf");
                            if poid != coid || pmode != cmode {
                                acc.edits.push(FileOp::Edit {
                                    path: path.clone(),
                                    old_blob_sha: poid,
                                    blob_sha: coid.clone(),
                                    mode: cmode,
                                    old_mode: pmode,
                                });
                                if matches!(cmode, EntryMode::Normal | EntryMode::Executable)
                                    && is_lfs_pointer(self, &coid)
                                {
                                    lfs_paths.insert(path);
                                }
                            }
                        }
                    }
                }
            }
        }

        // Parent entries with no child counterpart: removed.
        for (name, pe) in p_by_name {
            let path = join_path(prefix, name);
            match pe.mode.kind() {
                Tree => self.walk_removed_subtree(&pe.oid.to_hex().to_string(), &path, acc)?,
                Commit => {}
                _ => {
                    let mode = leaf_mode(pe.mode.kind()).expect("blob leaf");
                    acc.deletes.push((path, pe.oid.to_hex().to_string(), mode));
                }
            }
        }
        Ok(())
    }

    /// The first-parent tree diff for one commit. `parent_tree` is `None` for a root /
    /// depth-cut snapshot / out-of-plan first parent (diff against the empty tree).
    /// Produces [`FileOp`]s in the frozen canonical order (byte-identical to the old flat
    /// diff for every input) and records gitlink/LFS warnings for the child.
    fn diff_commit_trees(
        &self,
        parent_tree: Option<&str>,
        child_tree: &str,
        gitlink_warns: &mut BTreeMap<String, String>,
        lfs_paths: &mut BTreeSet<String>,
    ) -> Result<Vec<FileOp>, GitImportError> {
        let mut acc = DiffAccum::default();
        match parent_tree {
            // Diff vs empty: the whole child tree is added. The root path "" was never a
            // tracked dir in the flat model, so its DirCreate is intentionally not emitted.
            None => {
                self.walk_added_subtree(child_tree, "", &mut acc, gitlink_warns, lfs_paths)?;
            }
            // Identical trees → no changes and no warnings (already noted upstream).
            Some(pt) if pt == child_tree => {}
            Some(pt) => {
                self.collect_tree_diff(pt, child_tree, "", &mut acc, gitlink_warns, lfs_paths)?;
            }
        }
        let ops = finalize_diff_ops(acc);
        // An exact rename moves a blob to a new path without a create/edit — but the old
        // full-tree scan recorded the pointer's NEW path too, so post-scan the renames to
        // keep the LfsPointers warning byte-identical (the `from` path was already
        // recorded when the pointer was introduced).
        for op in &ops {
            if let FileOp::RenameExact { to, blob_sha, mode, .. } = op {
                if matches!(mode, EntryMode::Normal | EntryMode::Executable)
                    && is_lfs_pointer(self, blob_sha)
                {
                    lfs_paths.insert(to.clone());
                }
            }
        }
        Ok(ops)
    }
}

/// Turn the accumulated changed leaves into canonical [`FileOp`]s (git-bridge §3.1):
/// pair exact renames (same blob oid deleted at one path, created at another) in
/// bytewise path order, emit leftovers as Delete/Create, append the new empty
/// directories, then sort bytewise by result path. The push order (edits → renames →
/// deletes → creates → dir-creates) plus the stable final sort reproduce the old flat
/// `diff_trees` ordering byte-for-byte — in particular a `Delete(P)` still precedes a
/// colliding `DirCreate(P)` (a blob replaced by an empty dir). Gitlinks produce no ops.
fn finalize_diff_ops(acc: DiffAccum) -> Vec<FileOp> {
    let DiffAccum { deletes, creates, mut edits, dir_creates } = acc;
    let mut ops: Vec<FileOp> = Vec::new();
    ops.append(&mut edits);

    // --- exact-rename pairing: same blob oid deleted at one path, created at another ---
    // Group by blob sha; within a sha, pair sorted-deletes with sorted-creates in
    // bytewise path order (deterministic when counts are uneven).
    let mut del_by_sha: BTreeMap<String, Vec<(String, EntryMode)>> = BTreeMap::new();
    for (p, sha, m) in &deletes {
        del_by_sha.entry(sha.clone()).or_default().push((p.clone(), *m));
    }
    let mut cre_by_sha: BTreeMap<String, Vec<(String, EntryMode)>> = BTreeMap::new();
    for (p, sha, m) in &creates {
        cre_by_sha.entry(sha.clone()).or_default().push((p.clone(), *m));
    }

    let mut consumed_del: HashSet<String> = HashSet::new();
    let mut consumed_cre: HashSet<String> = HashSet::new();
    for (sha, dels) in &del_by_sha {
        if let Some(cres) = cre_by_sha.get(sha) {
            let mut dels = dels.clone();
            let mut cres = cres.clone();
            dels.sort();
            cres.sort();
            for ((from, _dm), (to, tm)) in dels.iter().zip(cres.iter()) {
                ops.push(FileOp::RenameExact {
                    from: from.clone(),
                    to: to.clone(),
                    blob_sha: sha.clone(),
                    mode: *tm,
                });
                consumed_del.insert(from.clone());
                consumed_cre.insert(to.clone());
            }
        }
    }

    // --- leftover deletes / creates ---
    for (path, _sha, _mode) in &deletes {
        if !consumed_del.contains(path) {
            ops.push(FileOp::Delete { path: path.clone() });
        }
    }
    for (path, sha, mode) in &creates {
        if !consumed_cre.contains(path) {
            ops.push(FileOp::Create { path: path.clone(), blob_sha: sha.clone(), mode: *mode });
        }
    }

    // --- new empty directories (collected during the walk, blob-less child subtrees) ---
    for dir in dir_creates {
        ops.push(FileOp::DirCreate { path: dir });
    }

    ops.sort_by(|a, b| a.sort_key().cmp(b.sort_key()));
    ops
}

// ===========================================================================
// plan_import — the top-level model builder
// ===========================================================================

/// Build the deterministic [`ImportPlan`] for the DAG reachable from `tip_sha`
/// (git-bridge §3). `tip_sha` may be a commit or a tag chain resolving to one.
///
/// Pure and total over its inputs: the same `db` + `tip` + `opts` always yields a
/// byte-identical plan, regardless of pack layout or object insertion order.
pub fn plan_import(
    db: &GitObjectDb,
    tip_sha: &str,
    opts: &ImportOptions,
) -> Result<ImportPlan, GitImportError> {
    let tip = db.peel_to_commit(tip_sha)?;

    // --- gather commit metadata (parents + committer seconds) for reachable set ---
    // reachable = commits present in db reachable from tip via ALL parent edges.
    let mut meta: HashMap<String, (Vec<String>, i64)> = HashMap::new();
    {
        let mut stack = vec![tip.clone()];
        while let Some(sha) = stack.pop() {
            if meta.contains_key(&sha) {
                continue;
            }
            // A parent absent from the db is a shallow boundary: skip it (do not
            // recurse); the child then diffs against the empty tree.
            let Some((k, _)) = db.get(&sha) else { continue };
            if k != GitObjKind::Commit {
                return Err(GitImportError::NotACommit(sha));
            }
            let commit = db.commit(&sha)?;
            let parents: Vec<String> = commit.parents().map(|p| p.to_hex().to_string()).collect();
            let ts = commit.committer().map_err(|e| GitImportError::Decode(e.to_string()))?.seconds();
            for p in &parents {
                if db.get(p).is_some() {
                    stack.push(p.clone());
                }
            }
            meta.insert(sha, (parents, ts));
        }
    }

    // Everything reachable from HEAD (BEFORE any depth cut). Phase 2 uses this — not
    // the post-cut planned set — to decide which open-branch commits are "unique":
    // a commit reachable from HEAD is never a branch's own commit, even under
    // `--depth` (its ancestry is HEAD's, just cut from the plan). A branch ref whose
    // tip is in here is skipped (`specs/git-open-branches.md` §1). FROZEN tie-break.
    let head_reachable: std::collections::HashSet<String> = meta.keys().cloned().collect();

    // --- first-parent chain of tip (newest → oldest), within reachable ---
    let mut main_chain: Vec<String> = Vec::new();
    {
        let mut cur = Some(tip.clone());
        while let Some(sha) = cur {
            main_chain.push(sha.clone());
            cur = meta
                .get(&sha)
                .and_then(|(ps, _)| ps.first().cloned())
                .filter(|p| meta.contains_key(p));
        }
    }

    // --- depth cut (§3.4) ---
    let depth_snapshot: Option<String> = match opts.depth {
        Some(n) if (n as usize) < main_chain.len() => {
            let cut_point = main_chain[n as usize].clone();
            // old_set = cut_point + all its ancestors (any parent edge).
            let mut old_set: HashSet<String> = HashSet::new();
            let mut stack = vec![cut_point.clone()];
            while let Some(sha) = stack.pop() {
                if !old_set.insert(sha.clone()) {
                    continue;
                }
                if let Some((ps, _)) = meta.get(&sha) {
                    for p in ps {
                        if meta.contains_key(p) {
                            stack.push(p.clone());
                        }
                    }
                }
            }
            // Drop everything at/older than the cut point; the snapshot re-adds
            // cut_point as a synthetic root.
            old_set.remove(&cut_point);
            meta.retain(|sha, _| !old_set.contains(sha));
            Some(cut_point)
        }
        _ => None,
    };

    // The set of shas that appear as PlannedCommits (governs "in-plan" first-parent
    // diff bases and fork points). Grows as phase 2 appends open-branch commits.
    let mut included: HashSet<String> = meta.keys().cloned().collect();

    // --- canonical topo order over the in-plan set ---
    // Snapshot node (if any) is a root: parents cleared.
    let mut nodes: BTreeMap<String, (Vec<String>, i64)> = BTreeMap::new();
    for (sha, (parents, ts)) in &meta {
        let is_snapshot = depth_snapshot.as_deref() == Some(sha.as_str());
        let parents_in: Vec<String> = if is_snapshot {
            Vec::new()
        } else {
            parents.iter().filter(|p| included.contains(*p)).cloned().collect()
        };
        nodes.insert(sha.clone(), (parents_in, *ts));
    }
    let mut order = canonical_topo_sort_v1(&nodes)?;

    // --- lane assignment (§3.1) ---
    // Merge subjects (title lines) drive branch naming; gather them up front.
    let mut subjects: HashMap<String, String> = HashMap::new();
    for sha in &order {
        if meta.get(sha).map(|(p, _)| p.len() >= 2).unwrap_or(false) {
            let title = db.commit(sha)?.message().title.to_string();
            subjects.insert(sha.clone(), title);
        }
    }
    let lanes_raw = assign_lanes(&order, &meta, &included, depth_snapshot.as_deref(), &tip, &subjects);
    let AssignResult { mut lane_of, mut lanes, mut merges_of } = lanes_raw;

    // --- branch naming + collision dedup (in lane-creation order) ---
    // `name_counts` stays alive so phase-2 open-branch names dedup against phase-1's.
    let mut name_counts: HashMap<String, usize> = HashMap::new();
    for lane in lanes.iter_mut() {
        if lane.id == MAIN_LANE {
            continue;
        }
        let base = lane.name.clone(); // holds the raw derived base at this point
        lane.name = dedup_name(&base, &mut name_counts);
    }

    // --- per-commit batches (phase 1) ---
    // No snapshot cache: each batch diffs its child tree against its first-parent tree
    // straight out of the db, recursing only into differing subtrees. Memory scales with
    // the pack, not commits × tree.
    let mut gitlink_warns: BTreeMap<String, String> = BTreeMap::new();
    let mut lfs_paths: BTreeSet<String> = BTreeSet::new();

    let mut commits: Vec<PlannedCommit> = Vec::with_capacity(order.len());
    for sha in &order {
        commits.push(build_commit_batch(
            db, sha, &meta, &included, depth_snapshot.as_deref(), &lane_of, &merges_of,
            &mut gitlink_warns, &mut lfs_paths,
        )?);
    }

    // ===================================================================
    // Phase 2 — open (unmerged) branches (specs/git-open-branches.md §2)
    // ===================================================================
    //
    // Canonical order = ref name BYTEWISE (frozen). Each branch imports its UNIQUE
    // commits — reachable from the branch tip, minus everything reachable from HEAD
    // (`head_reachable`) and minus every earlier open branch (`emitted`). The
    // branch's own first-parent chain becomes a LIVE lane (no delete); internal
    // merges recurse into tombstoned sub-lanes exactly as phase 1. Fork point = the
    // first already-PLANNED (`included`) commit walking the tip's first-parent chain;
    // if that walk exits the planned set at a root, an unrelated root (orphan), or a
    // depth-cut ancestor, the lane forks nowhere (`fork=None`) and its oldest commit
    // snapshots vs the empty tree — same rule as a depth cut / grafted root.
    let mut skipped_reachable: Vec<String> = Vec::new();
    if !opts.open_branch_tips.is_empty() {
        // `emitted` = reachable-from-HEAD ∪ every open branch imported so far. It is
        // the boundary for "unique" gathering and for the skip decision. Distinct
        // from `included` (the planned set) so `--depth` behaves correctly: a cut
        // ancestor is in `emitted` (not unique) but not in `included` (→ snapshot).
        let mut emitted: HashSet<String> = head_reachable;
        let mut tips = opts.open_branch_tips.clone();
        tips.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
        for (ref_name, raw_tip) in &tips {
            let btip = db.peel_to_commit(raw_tip)?;
            if emitted.contains(&btip) {
                // Tip already carries nothing new (reachable from HEAD or an earlier
                // open branch) — skip, report the ref (§1).
                skipped_reachable.push(ref_name.clone());
                continue;
            }
            plan_open_branch(
                db, ref_name, &btip, &mut emitted, &mut meta, &mut included, &mut lanes,
                &mut lane_of, &mut merges_of, &mut name_counts, &mut order, &mut commits,
                &mut gitlink_warns, &mut lfs_paths,
            )?;
        }
    }

    // Resolve fork commit indexes now that `commits` (both phases) is final.
    let index_of: HashMap<&str, usize> =
        commits.iter().enumerate().map(|(i, c)| (c.sha.as_str(), i)).collect();
    for lane in lanes.iter_mut() {
        if let Some(fork) = lane.fork.as_mut() {
            fork.commit_index = *index_of.get(fork.commit_sha.as_str()).unwrap_or(&0);
        }
        if opts.keep_imported_branches {
            lane.deleted_after_merge = false;
        }
    }

    // --- warnings (deterministic order) ---
    let mut warnings: Vec<ImportWarning> = Vec::new();
    for (path, target) in gitlink_warns {
        warnings.push(ImportWarning::Submodule { path, target_sha: target });
    }
    if !lfs_paths.is_empty() {
        warnings.push(ImportWarning::LfsPointers { paths: lfs_paths.into_iter().collect() });
    }

    // root_sha = the plan's main-lane root (snapshot under depth, else the true root).
    let root_sha = depth_snapshot
        .clone()
        .unwrap_or_else(|| main_chain.last().cloned().unwrap_or_else(|| tip.clone()));

    Ok(ImportPlan { commits, lanes, root_sha, tip_sha: tip, warnings, skipped_reachable })
}

/// True if a blob's content is a git-LFS pointer (git-bridge §3.3). For a spilled blob
/// the answer was computed at decode time and read from the locator (no byte access);
/// otherwise it sniffs the bytes still held in `objects`.
fn is_lfs_pointer(db: &GitObjectDb, blob_sha: &str) -> bool {
    if let Some(loc) = db.blob_meta.get(blob_sha) {
        return loc.is_lfs;
    }
    match db.get(blob_sha) {
        Some((GitObjKind::Blob, bytes)) => bytes.starts_with(LFS_POINTER_PREFIX),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Lane assignment internals
// ---------------------------------------------------------------------------

struct AssignResult {
    lane_of: HashMap<String, LaneId>,
    lanes: Vec<PlannedLane>,
    merges_of: HashMap<String, Vec<MergeInfo>>,
}

/// §3.1 lane assignment. Deterministic: `main` = tip's first-parent chain; then
/// merges are expanded in canonical order, non-first parents in parent-index order,
/// each walking its first-parent chain back to the first already-assigned commit.
fn assign_lanes(
    order: &[String],
    meta: &HashMap<String, (Vec<String>, i64)>,
    included: &HashSet<String>,
    snapshot: Option<&str>,
    tip: &str,
    subjects: &HashMap<String, String>,
) -> AssignResult {
    let mut lane_of: HashMap<String, LaneId> = HashMap::new();
    let mut lanes: Vec<PlannedLane> = Vec::new();
    let mut merges_of: HashMap<String, Vec<MergeInfo>> = HashMap::new();

    let parent0 = |sha: &str| -> Option<String> {
        meta.get(sha)
            .and_then(|(ps, _)| ps.first().cloned())
            .filter(|p| included.contains(p))
    };
    let is_snapshot = |sha: &str| snapshot == Some(sha);

    // --- lane 0 = main: tip's first-parent chain within the plan ---
    let mut main_root = tip.to_string();
    {
        let mut cur = Some(tip.to_string());
        while let Some(sha) = cur {
            lane_of.insert(sha.clone(), MAIN_LANE);
            main_root = sha.clone();
            // A snapshot node has its parents cleared → chain stops there.
            cur = if is_snapshot(&sha) { None } else { parent0(&sha) };
        }
    }
    lanes.push(PlannedLane {
        id: MAIN_LANE,
        name: "main".to_string(),
        fork: None,
        created_at_commit: main_root,
        merged_at_commit: None,
        deleted_after_merge: false,
        live: false,
    });

    let is_merge = |sha: &str| meta.get(sha).map(|(ps, _)| ps.len() >= 2).unwrap_or(false);

    // --- expand merges in canonical order (worklist over assigned, unexpanded) ---
    let mut expanded: HashSet<String> = HashSet::new();
    loop {
        // Earliest-in-canonical-order merge that is assigned but not yet expanded.
        let next = order
            .iter()
            .find(|s| is_merge(s) && lane_of.contains_key(*s) && !expanded.contains(*s))
            .cloned();
        let Some(m) = next else { break };
        expanded.insert(m.clone());

        let subject = subjects.get(&m).cloned().unwrap_or_default();
        let parents = meta.get(&m).map(|(p, _)| p.clone()).unwrap_or_default();
        let mut infos: Vec<MergeInfo> = Vec::new();

        for pi in parents.iter().skip(1) {
            if !included.contains(pi) {
                // Non-first parent outside the plan (depth boundary) — nothing to add.
                continue;
            }
            if let Some(&lane) = lane_of.get(pi) {
                infos.push(MergeInfo { source_lane: lane, source_tip_sha: pi.clone() });
                continue;
            }
            // Create a new lane by walking pi's first-parent chain back to an
            // already-assigned commit (or a boundary).
            let mut chain: Vec<String> = Vec::new();
            let mut cur = pi.clone();
            let fork: Option<ForkPoint> = loop {
                if let Some(&lane) = lane_of.get(&cur) {
                    break Some(ForkPoint { lane, commit_sha: cur.clone(), commit_index: 0 });
                }
                chain.push(cur.clone());
                match parent0(&cur) {
                    Some(par) if !is_snapshot(&cur) => cur = par,
                    _ => break None, // boundary / root
                }
            };

            let new_lane = lanes.len();
            for c in &chain {
                lane_of.insert(c.clone(), new_lane);
            }
            let created_at = chain.last().cloned().unwrap_or_else(|| pi.clone());

            lanes.push(PlannedLane {
                id: new_lane,
                // Store the raw derived base name; dedup happens in plan_import.
                name: branch_name_for(&subject, pi),
                fork,
                created_at_commit: created_at,
                merged_at_commit: Some(m.clone()),
                deleted_after_merge: true,
                live: false,
            });
            infos.push(MergeInfo { source_lane: new_lane, source_tip_sha: pi.clone() });
        }

        merges_of.insert(m.clone(), infos);
    }

    AssignResult { lane_of, lanes, merges_of }
}

/// Apply the frozen `-2`/`-3`… collision dedup to a raw base name, mutating the
/// shared `counts` (keyed by raw base). The **first** use of a base keeps it
/// verbatim; the Nth (N≥2) becomes `base-N`. Phase 1 and phase 2 share one
/// `counts` map so an open-branch name colliding with a phase-1 branch name dedups
/// against it (git-open-branches §2), in lane-creation (emission) order.
fn dedup_name(base: &str, counts: &mut HashMap<String, usize>) -> String {
    let n = counts.entry(base.to_string()).or_insert(0);
    *n += 1;
    if *n == 1 {
        base.to_string()
    } else {
        format!("{base}-{n}")
    }
}

/// Build one commit's [`PlannedCommit`] batch: diff its tree against the first parent's
/// tree (empty for a root, a depth-cut snapshot, or an out-of-plan first parent),
/// collecting gitlink/LFS warnings from the changed entries, and reconstruct the
/// message. Shared by phase 1 and phase 2 so their per-commit output is provably
/// identical.
///
/// The diff base is the first parent **iff it is in `included`** (the planned set):
/// this is what makes a phase-2 open-branch commit forking off a phase-1 commit diff
/// against that real parent tree, while a commit whose first parent was cut by
/// `--depth` (or is an unrelated/shallow root) snapshots against the empty tree.
///
/// The diff reads tree objects directly from the db and skips equal-oid subtrees, so it
/// is O(changed entries × depth); no snapshot is materialized or cached.
#[allow(clippy::too_many_arguments)]
fn build_commit_batch(
    db: &GitObjectDb,
    sha: &str,
    meta: &HashMap<String, (Vec<String>, i64)>,
    included: &HashSet<String>,
    depth_snapshot: Option<&str>,
    lane_of: &HashMap<String, LaneId>,
    merges_of: &HashMap<String, Vec<MergeInfo>>,
    gitlink_warns: &mut BTreeMap<String, String>,
    lfs_paths: &mut BTreeSet<String>,
) -> Result<PlannedCommit, GitImportError> {
    let is_snapshot = depth_snapshot == Some(sha);
    let commit = db.commit(sha)?;
    let tree_sha = commit.tree().to_hex().to_string();

    // First-parent tree: None (→ diff vs the empty tree) for a root / depth-cut snapshot
    // / out-of-plan first parent; otherwise the first parent's tree sha.
    let parents = meta.get(sha).map(|(p, _)| p.clone()).unwrap_or_default();
    let parent_tree: Option<String> = if is_snapshot {
        None
    } else if let Some(p0) = parents.first().filter(|p| included.contains(*p)) {
        Some(db.commit(p0)?.tree().to_hex().to_string())
    } else {
        None
    };

    let ops = db.diff_commit_trees(parent_tree.as_deref(), &tree_sha, gitlink_warns, lfs_paths)?;

    let author = commit.author().map_err(|e| GitImportError::Decode(e.to_string()))?;
    let committer = commit.committer().map_err(|e| GitImportError::Decode(e.to_string()))?;
    let msg = commit.message();
    // Reconstruct "subject + body"; git stores a trailing newline, so normalize the
    // tail (deterministic, and what the UI wants to display).
    let title = String::from_utf8_lossy(msg.title).trim_end().to_string();
    let message = match msg.body {
        Some(body) => {
            let body = String::from_utf8_lossy(body);
            format!("{}\n\n{}", title, body.trim_end())
        }
        None => title,
    };

    let merges = merges_of.get(sha).cloned().unwrap_or_default();

    Ok(PlannedCommit {
        sha: sha.to_string(),
        lane: *lane_of.get(sha).expect("every commit is assigned a lane"),
        parents: if is_snapshot { Vec::new() } else { parents },
        merges,
        author_name: String::from_utf8_lossy(author.name).trim().to_string(),
        author_email: String::from_utf8_lossy(author.email).trim().to_string(),
        committer_ts_ms: committer.seconds() * 1000,
        message,
        ops,
        is_depth_cut_snapshot: is_snapshot,
    })
}

/// Plan one open (unmerged) branch into the accumulating plan (git-open-branches §2).
///
/// Appends the branch's unique commits (reachable from `tip`, not in `emitted`) to
/// `order`/`commits`/`meta`/`included`/`emitted`, and its lanes to `lanes`/`lane_of`
/// (one LIVE first-parent lane named `ref_name`, plus tombstoned sub-lanes for any
/// internal merges). Canonical topo order (`committer_seconds, sha`) within the
/// branch, same domain as phase 1.
#[allow(clippy::too_many_arguments)]
fn plan_open_branch(
    db: &GitObjectDb,
    ref_name: &str,
    tip: &str,
    emitted: &mut HashSet<String>,
    meta: &mut HashMap<String, (Vec<String>, i64)>,
    included: &mut HashSet<String>,
    lanes: &mut Vec<PlannedLane>,
    lane_of: &mut HashMap<String, LaneId>,
    merges_of: &mut HashMap<String, Vec<MergeInfo>>,
    name_counts: &mut HashMap<String, usize>,
    order: &mut Vec<String>,
    commits: &mut Vec<PlannedCommit>,
    gitlink_warns: &mut BTreeMap<String, String>,
    lfs_paths: &mut BTreeSet<String>,
) -> Result<(), GitImportError> {
    // 1. Gather this branch's UNIQUE commits: reachable from tip, stopping at any
    //    already-emitted commit (reachable from HEAD or an earlier open branch) and
    //    at shallow/absent boundaries. Those boundary commits are NOT included.
    let mut new_meta: HashMap<String, (Vec<String>, i64)> = HashMap::new();
    {
        let mut stack = vec![tip.to_string()];
        while let Some(sha) = stack.pop() {
            if emitted.contains(&sha) || new_meta.contains_key(&sha) {
                continue;
            }
            let Some((k, _)) = db.get(&sha) else { continue }; // shallow boundary
            if k != GitObjKind::Commit {
                return Err(GitImportError::NotACommit(sha));
            }
            let commit = db.commit(&sha)?;
            let parents: Vec<String> = commit.parents().map(|p| p.to_hex().to_string()).collect();
            let ts = commit.committer().map_err(|e| GitImportError::Decode(e.to_string()))?.seconds();
            for p in &parents {
                if !emitted.contains(p) && db.get(p).is_some() {
                    stack.push(p.clone());
                }
            }
            new_meta.insert(sha, (parents, ts));
        }
    }
    // A tip that peels into nothing new (fully emitted) shouldn't reach here — the
    // caller skips those — but guard anyway so an empty branch is a no-op.
    if new_meta.is_empty() {
        return Ok(());
    }
    let new_set: HashSet<String> = new_meta.keys().cloned().collect();

    // 2. Canonical topo order over the new commits (parents restricted to the new
    //    set; boundary parents are dropped so oldest commits are roots of the sort).
    let mut nodes: BTreeMap<String, (Vec<String>, i64)> = BTreeMap::new();
    for (sha, (parents, ts)) in &new_meta {
        let parents_in: Vec<String> = parents.iter().filter(|p| new_set.contains(*p)).cloned().collect();
        nodes.insert(sha.clone(), (parents_in, *ts));
    }
    let branch_order = canonical_topo_sort_v1(&nodes)?;

    // 3. Merge subjects for internal merges (drive sub-lane naming).
    let mut subjects: HashMap<String, String> = HashMap::new();
    for sha in &branch_order {
        if new_meta.get(sha).map(|(p, _)| p.len() >= 2).unwrap_or(false) {
            subjects.insert(sha.clone(), db.commit(sha)?.message().title.to_string());
        }
    }

    // 4. Lane assignment for this branch (appends to lanes/lane_of/merges_of).
    assign_open_branch_lanes(
        ref_name, tip, &branch_order, &new_meta, &new_set, lane_of, lanes, merges_of,
        name_counts, &subjects,
    );

    // 5. Fold the new commits into the global planned/emitted sets + meta, then build
    //    their batches in canonical order and append to `commits`/`order`. `included`
    //    must contain the new commits BEFORE building so within-branch first parents
    //    resolve as real diff bases (an earlier boundary parent stays out → snapshot).
    for (sha, mp) in new_meta {
        meta.insert(sha, mp);
    }
    for sha in &branch_order {
        included.insert(sha.clone());
        emitted.insert(sha.clone());
    }
    for sha in &branch_order {
        commits.push(build_commit_batch(
            db, sha, meta, included, None, lane_of, merges_of, gitlink_warns, lfs_paths,
        )?);
        order.push(sha.clone());
    }
    Ok(())
}

/// Lane assignment for one open branch (git-open-branches §2), mirroring
/// [`assign_lanes`] but with the branch's own first-parent chain as a **new LIVE
/// lane** (named `ref_name`, no delete) instead of `main`, and its internal merges
/// spawning tombstoned sub-lanes. `lane_of` already holds every phase-1 (and earlier
/// open-branch) commit, so a walk that reaches one of them forks off that lane.
#[allow(clippy::too_many_arguments)]
fn assign_open_branch_lanes(
    ref_name: &str,
    tip: &str,
    order: &[String],
    new_meta: &HashMap<String, (Vec<String>, i64)>,
    new_set: &HashSet<String>,
    lane_of: &mut HashMap<String, LaneId>,
    lanes: &mut Vec<PlannedLane>,
    merges_of: &mut HashMap<String, Vec<MergeInfo>>,
    name_counts: &mut HashMap<String, usize>,
    subjects: &HashMap<String, String>,
) {
    // First parent (unfiltered — boundary handling is explicit at each call site).
    let parent0 = |sha: &str| -> Option<String> {
        new_meta.get(sha).and_then(|(ps, _)| ps.first().cloned())
    };

    // --- the branch's own lane = tip's first-parent chain within the new set ---
    let branch_lane = lanes.len();
    let mut chain: Vec<String> = Vec::new();
    let mut fork: Option<ForkPoint> = None;
    {
        let mut cur = tip.to_string();
        loop {
            chain.push(cur.clone());
            match parent0(&cur) {
                // First parent is another unique commit → keep walking this lane.
                Some(par) if new_set.contains(&par) => cur = par,
                // First already-planned commit → fork off whichever lane owns it.
                Some(par) => {
                    if let Some(&plane) = lane_of.get(&par) {
                        fork = Some(ForkPoint { lane: plane, commit_sha: par, commit_index: 0 });
                    }
                    // Else `par` exists but was cut by `--depth` (not planned): treat
                    // the boundary as a root — fork stays None, oldest commit snapshots.
                    break;
                }
                // Repo root / unrelated (orphan) root → fork nowhere, diff-vs-empty.
                None => break,
            }
        }
    }
    for c in &chain {
        lane_of.insert(c.clone(), branch_lane);
    }
    let created_at = chain.last().cloned().unwrap_or_else(|| tip.to_string());
    lanes.push(PlannedLane {
        id: branch_lane,
        name: dedup_name(ref_name, name_counts),
        fork,
        created_at_commit: created_at,
        merged_at_commit: None,
        deleted_after_merge: false,
        live: true,
    });

    // --- expand internal merges (canonical order) into tombstoned sub-lanes ---
    let is_merge = |sha: &str| new_meta.get(sha).map(|(ps, _)| ps.len() >= 2).unwrap_or(false);
    let mut expanded: HashSet<String> = HashSet::new();
    loop {
        let next = order
            .iter()
            .find(|s| is_merge(s) && lane_of.contains_key(*s) && !expanded.contains(*s))
            .cloned();
        let Some(m) = next else { break };
        expanded.insert(m.clone());

        let subject = subjects.get(&m).cloned().unwrap_or_default();
        let parents = new_meta.get(&m).map(|(p, _)| p.clone()).unwrap_or_default();
        let mut infos: Vec<MergeInfo> = Vec::new();

        for pi in parents.iter().skip(1) {
            if let Some(&lane) = lane_of.get(pi) {
                // Already assigned (this branch's lane, an earlier sub-lane, or a
                // phase-1 / earlier-open-branch lane) → merge edge, no new lane.
                infos.push(MergeInfo { source_lane: lane, source_tip_sha: pi.clone() });
                continue;
            }
            if !new_set.contains(pi) {
                // Boundary (cut ancestor / shallow) that is not planned — unassignable.
                continue;
            }
            // New sub-lane: walk pi's first-parent chain back to an assigned commit.
            let mut chain: Vec<String> = Vec::new();
            let mut cur = pi.clone();
            let fork: Option<ForkPoint> = loop {
                if let Some(&lane) = lane_of.get(&cur) {
                    break Some(ForkPoint { lane, commit_sha: cur.clone(), commit_index: 0 });
                }
                chain.push(cur.clone());
                match parent0(&cur) {
                    Some(par) if new_set.contains(&par) || lane_of.contains_key(&par) => cur = par,
                    _ => break None,
                }
            };

            let new_lane = lanes.len();
            for c in &chain {
                lane_of.insert(c.clone(), new_lane);
            }
            let created = chain.last().cloned().unwrap_or_else(|| pi.clone());
            lanes.push(PlannedLane {
                id: new_lane,
                name: dedup_name(&branch_name_for(&subject, pi), name_counts),
                fork,
                created_at_commit: created,
                merged_at_commit: Some(m.clone()),
                deleted_after_merge: true,
                live: false,
            });
            infos.push(MergeInfo { source_lane: new_lane, source_tip_sha: pi.clone() });
        }

        merges_of.insert(m.clone(), infos);
    }
}

// ===========================================================================
// Tests (in-crate, no system git — objects are synthesized as raw bytes)
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // --- raw-object synthesis helpers ------------------------------------

    fn mk_blob(db: &mut GitObjectDb, content: &str) -> String {
        db.insert_loose(GitObjKind::Blob, content.as_bytes()).unwrap()
    }

    /// Encode + insert a tree. Entries: `(git_mode, name, target_sha_hex)`, e.g.
    /// `("100644", "a.txt", blob)`, `("40000", "dir", subtree)`.
    fn mk_tree(db: &mut GitObjectDb, entries: &[(&str, &str, &str)]) -> String {
        let mut sorted = entries.to_vec();
        sorted.sort_by(|a, b| a.1.as_bytes().cmp(b.1.as_bytes()));
        let mut buf: Vec<u8> = Vec::new();
        for (mode, name, sha_hex) in sorted {
            buf.extend_from_slice(mode.as_bytes());
            buf.push(b' ');
            buf.extend_from_slice(name.as_bytes());
            buf.push(0);
            buf.extend_from_slice(&hex::decode(sha_hex).unwrap());
        }
        db.insert_loose(GitObjKind::Tree, &buf).unwrap()
    }

    fn mk_commit(db: &mut GitObjectDb, tree: &str, parents: &[&str], ts: i64, msg: &str) -> String {
        let mut s = format!("tree {tree}\n");
        for p in parents {
            s.push_str(&format!("parent {p}\n"));
        }
        s.push_str(&format!("author Ada <ada@ex> {ts} +0000\n"));
        s.push_str(&format!("committer Ada <ada@ex> {ts} +0000\n\n"));
        s.push_str(msg);
        db.insert_loose(GitObjKind::Commit, s.as_bytes()).unwrap()
    }

    /// A blob whose content is `<name>-vN`, so distinct commits get distinct trees.
    fn commit_with_file(
        db: &mut GitObjectDb,
        name: &str,
        content: &str,
        parents: &[&str],
        ts: i64,
        msg: &str,
    ) -> String {
        let b = mk_blob(db, content);
        let t = mk_tree(db, &[("100644", name, &b)]);
        mk_commit(db, &t, parents, ts, msg)
    }

    // --- canonical topo order --------------------------------------------

    #[test]
    fn topo_order_is_ts_then_sha_and_parents_first() {
        // root -> {a, b}; m merges a,b. b has an earlier ts than a.
        let mut n = BTreeMap::new();
        n.insert("root".to_string(), (vec![], 1i64));
        n.insert("a".to_string(), (vec!["root".to_string()], 3));
        n.insert("b".to_string(), (vec!["root".to_string()], 2));
        n.insert("m".to_string(), (vec!["a".to_string(), "b".to_string()], 4));
        let order = canonical_topo_sort_v1(&n).unwrap();
        assert_eq!(order, vec!["root", "b", "a", "m"]);
    }

    #[test]
    fn topo_tie_break_is_sha_when_ts_equal() {
        // Two roots with identical ts: smaller sha first (total order via sha).
        let mut n = BTreeMap::new();
        n.insert("zzz".to_string(), (vec![], 5i64));
        n.insert("aaa".to_string(), (vec![], 5i64));
        let order = canonical_topo_sort_v1(&n).unwrap();
        assert_eq!(order, vec!["aaa", "zzz"]);
    }

    #[test]
    fn topo_detects_cycle() {
        let mut n = BTreeMap::new();
        n.insert("x".to_string(), (vec!["y".to_string()], 1i64));
        n.insert("y".to_string(), (vec!["x".to_string()], 1i64));
        assert_eq!(canonical_topo_sort_v1(&n), Err(GitImportError::Cycle));
    }

    // --- branch name parser ----------------------------------------------

    #[test]
    fn branch_name_parser_table() {
        let tip = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2";
        let cases: &[(&str, &str)] = &[
            ("Merge pull request #1 from owner/feature-1", "feature-1"),
            ("Merge pull request #42 from octocat/dev/topic", "dev/topic"),
            ("Merge pull request #7 from a-b_c/fix", "fix"),
            ("Merge branch 'branch-a'", "branch-a"),
            ("Merge branch 'main' into side", "main"),
            ("Merge branch 'feature/x' into develop", "feature/x"),
        ];
        for (subject, expect) in cases {
            assert_eq!(&branch_name_for(subject, tip), expect, "subject: {subject}");
        }
        // Non-matching subjects fall back to git/<short tip>.
        for subject in ["random work", "Merge unrelated history 'graft'", "Octopus merge of a, b"] {
            assert_eq!(branch_name_for(subject, tip), format!("git/{}", &tip[..7]));
        }
    }

    // --- rename pairing ---------------------------------------------------

    /// The recursive first-parent tree diff between two real git trees, discarding
    /// warnings (exercised separately). Mirrors what `build_commit_batch` computes.
    fn tree_diff(db: &GitObjectDb, parent_tree: &str, child_tree: &str) -> Vec<FileOp> {
        let mut g: BTreeMap<String, String> = BTreeMap::new();
        let mut l: BTreeSet<String> = BTreeSet::new();
        db.diff_commit_trees(Some(parent_tree), child_tree, &mut g, &mut l).unwrap()
    }

    #[test]
    fn single_exact_rename() {
        let mut db = GitObjectDb::new();
        let x = mk_blob(&mut db, "content-X");
        let parent = mk_tree(&mut db, &[("100644", "foo.txt", &x)]);
        let child = mk_tree(&mut db, &[("100644", "bar.txt", &x)]);
        let ops = tree_diff(&db, &parent, &child);
        assert_eq!(ops, vec![FileOp::RenameExact {
            from: "foo.txt".into(),
            to: "bar.txt".into(),
            blob_sha: x,
            mode: EntryMode::Normal,
        }]);
    }

    #[test]
    fn rename_pairing_is_bytewise_deterministic() {
        // Same blob at two source paths deleted, appearing at two dest paths.
        // Pair sorted-deletes with sorted-creates: a->c, b->d.
        let mut db = GitObjectDb::new();
        let x = mk_blob(&mut db, "same");
        let parent = mk_tree(&mut db, &[("100644", "a", &x), ("100644", "b", &x)]);
        let child = mk_tree(&mut db, &[("100644", "c", &x), ("100644", "d", &x)]);
        let ops = tree_diff(&db, &parent, &child);
        let renames: Vec<_> = ops
            .iter()
            .filter_map(|o| match o {
                FileOp::RenameExact { from, to, .. } => Some((from.clone(), to.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(renames, vec![("a".into(), "c".into()), ("b".into(), "d".into())]);
    }

    #[test]
    fn mode_flip_same_content_is_edit() {
        let mut db = GitObjectDb::new();
        let x = mk_blob(&mut db, "#!/bin/sh\n");
        let parent = mk_tree(&mut db, &[("100644", "run", &x)]);
        let child = mk_tree(&mut db, &[("100755", "run", &x)]);
        let ops = tree_diff(&db, &parent, &child);
        assert_eq!(ops, vec![FileOp::Edit {
            path: "run".into(),
            old_blob_sha: x.clone(),
            blob_sha: x,
            mode: EntryMode::Executable,
            old_mode: EntryMode::Normal,
        }]);
    }

    /// A renamed LFS pointer must surface its NEW path in the LfsPointers warning —
    /// the old full-tree scan saw the pointer at `to` in the child tree; the recursive
    /// diff pins that via the post-scan over RenameExact ops.
    #[test]
    fn renamed_lfs_pointer_warns_at_both_paths() {
        let mut db = GitObjectDb::new();
        let ptr = mk_blob(&mut db, "version https://git-lfs.github.com/spec/v1\noid sha256:aa\nsize 1\n");
        let t0 = mk_tree(&mut db, &[("100644", "old.bin", &ptr)]);
        let c0 = mk_commit(&mut db, &t0, &[], 1000, "add pointer");
        let t1 = mk_tree(&mut db, &[("100644", "new.bin", &ptr)]);
        let c1 = mk_commit(&mut db, &t1, &[&c0], 1060, "rename pointer");
        let plan = plan_import(&db, &c1, &ImportOptions::default()).unwrap();
        assert!(plan.commits.iter().any(|c| c.ops.iter().any(|o| matches!(o,
            FileOp::RenameExact { from, to, .. } if from == "old.bin" && to == "new.bin"))));
        assert_eq!(
            plan.warnings,
            vec![ImportWarning::LfsPointers {
                paths: vec!["new.bin".to_string(), "old.bin".to_string()],
            }]
        );
    }

    /// Blob↔directory swaps at the same path: replacing a file with a directory (and
    /// back) must emit the same delete/create/DirCreate set the flat diff produced.
    #[test]
    fn blob_dir_swaps_at_same_path() {
        let mut db = GitObjectDb::new();
        let f = mk_blob(&mut db, "file-content");
        let inner = mk_blob(&mut db, "inner-content");
        // parent: "x" is a blob. child: "x" is a dir with x/inner.txt.
        let pt = mk_tree(&mut db, &[("100644", "x", &f)]);
        let sub = mk_tree(&mut db, &[("100644", "inner.txt", &inner)]);
        let ct = mk_tree(&mut db, &[("40000", "x", &sub)]);
        let mut g: BTreeMap<String, String> = BTreeMap::new();
        let mut l: BTreeSet<String> = BTreeSet::new();
        let ops = db.diff_commit_trees(Some(&pt), &ct, &mut g, &mut l).unwrap();
        assert_eq!(ops, vec![
            FileOp::Delete { path: "x".into() },
            FileOp::Create { path: "x/inner.txt".into(), blob_sha: inner.clone(), mode: EntryMode::Normal },
        ]);
        // dir → blob (the reverse) deletes the subtree's blobs and creates the file.
        let ops = db.diff_commit_trees(Some(&ct), &pt, &mut g, &mut l).unwrap();
        assert_eq!(ops, vec![
            FileOp::Create { path: "x".into(), blob_sha: f.clone(), mode: EntryMode::Normal },
            FileOp::Delete { path: "x/inner.txt".into() },
        ]);
        // blob → EMPTY dir: Delete precedes the colliding DirCreate at the same path
        // (frozen op order: the flat diff pushed deletes before dir-creates).
        let empty_tree = db.insert_loose(GitObjKind::Tree, b"").unwrap();
        let ct_empty = mk_tree(&mut db, &[("40000", "x", &empty_tree)]);
        let ops = db.diff_commit_trees(Some(&pt), &ct_empty, &mut g, &mut l).unwrap();
        assert_eq!(ops, vec![
            FileOp::Delete { path: "x".into() },
            FileOp::DirCreate { path: "x".into() },
        ]);
    }

    // --- root commit + empty dir -----------------------------------------

    #[test]
    fn root_commit_diffs_against_empty_tree() {
        let mut db = GitObjectDb::new();
        let tip = commit_with_file(&mut db, "a.txt", "alpha", &[], 1000, "root");
        let plan = plan_import(&db, &tip, &ImportOptions::default()).unwrap();
        assert_eq!(plan.commits.len(), 1);
        let c = &plan.commits[0];
        assert_eq!(c.parents.len(), 0);
        assert_eq!(c.ops.len(), 1);
        assert!(matches!(&c.ops[0], FileOp::Create { path, .. } if path == "a.txt"));
        assert_eq!(plan.root_sha, tip);
        assert_eq!(plan.lanes.len(), 1);
        assert_eq!(plan.lanes[0].name, "main");
    }

    #[test]
    fn new_empty_directory_yields_dircreate() {
        let mut db = GitObjectDb::new();
        let empty_tree = db.insert_loose(GitObjKind::Tree, b"").unwrap();
        // c0: root tree has a single blob.
        let c0 = commit_with_file(&mut db, "keep.txt", "k", &[], 1000, "c0");
        // c1: adds an empty subdirectory (a tree entry pointing at the empty tree).
        let b = mk_blob(&mut db, "k");
        let t1 = mk_tree(&mut db, &[("100644", "keep.txt", &b), ("40000", "emptydir", &empty_tree)]);
        let c1 = mk_commit(&mut db, &t1, &[&c0], 1060, "add empty dir");
        let plan = plan_import(&db, &c1, &ImportOptions::default()).unwrap();
        let c1_plan = plan.commits.iter().find(|c| c.sha == c1).unwrap();
        assert!(
            c1_plan.ops.iter().any(|o| matches!(o, FileOp::DirCreate { path } if path == "emptydir")),
            "expected DirCreate for the empty dir, got {:?}",
            c1_plan.ops
        );
    }

    // --- lane assignment: merged-PR shape --------------------------------

    #[test]
    fn merged_prs_lane_structure() {
        let mut db = GitObjectDb::new();
        // I (root) on main.
        let i = commit_with_file(&mut db, "README", "r", &[], 1000, "initial");
        // feature-1: f1 forks from I.
        let f1 = commit_with_file(&mut db, "f1.txt", "one", &[&i], 1060, "feature 1 work");
        // main merges feature-1 (first parent I, second f1).
        let mtree1 = {
            let b_r = mk_blob(&mut db, "r");
            let b_f1 = mk_blob(&mut db, "one");
            mk_tree(&mut db, &[("100644", "README", &b_r), ("100644", "f1.txt", &b_f1)])
        };
        let m1 = mk_commit(&mut db, &mtree1, &[&i, &f1], 1120, "Merge pull request #1 from owner/feature-1");
        // feature-2: f2 forks from M1.
        let f2 = {
            let b_r = mk_blob(&mut db, "r");
            let b_f1 = mk_blob(&mut db, "one");
            let b_f2 = mk_blob(&mut db, "two");
            let t = mk_tree(&mut db, &[
                ("100644", "README", &b_r),
                ("100644", "f1.txt", &b_f1),
                ("100644", "f2.txt", &b_f2),
            ]);
            mk_commit(&mut db, &t, &[&m1], 1180, "feature 2 work")
        };
        // main merges feature-2.
        let m2 = {
            let b_r = mk_blob(&mut db, "r");
            let b_f1 = mk_blob(&mut db, "one");
            let b_f2 = mk_blob(&mut db, "two");
            let t = mk_tree(&mut db, &[
                ("100644", "README", &b_r),
                ("100644", "f1.txt", &b_f1),
                ("100644", "f2.txt", &b_f2),
            ]);
            mk_commit(&mut db, &t, &[&m1, &f2], 1240, "Merge pull request #2 from owner/feature-2")
        };

        let plan = plan_import(&db, &m2, &ImportOptions::default()).unwrap();

        // 3 lanes: main + two feature lanes.
        assert_eq!(plan.lanes.len(), 3, "lanes: {:?}", plan.lanes);
        let lane_of = |sha: &str| plan.commits.iter().find(|c| c.sha == sha).unwrap().lane;
        assert_eq!(lane_of(&m2), MAIN_LANE);
        assert_eq!(lane_of(&m1), MAIN_LANE);
        assert_eq!(lane_of(&i), MAIN_LANE);
        let l_f1 = lane_of(&f1);
        let l_f2 = lane_of(&f2);
        assert_ne!(l_f1, MAIN_LANE);
        assert_ne!(l_f2, MAIN_LANE);
        assert_ne!(l_f1, l_f2);

        // Names parsed from the PR subjects.
        assert_eq!(plan.lanes[l_f1].name, "feature-1");
        assert_eq!(plan.lanes[l_f2].name, "feature-2");
        // f1 forks at I on main; f2 forks at M1 on main.
        assert_eq!(plan.lanes[l_f1].fork.as_ref().unwrap().commit_sha, i);
        assert_eq!(plan.lanes[l_f2].fork.as_ref().unwrap().commit_sha, m1);
        assert!(plan.lanes[l_f1].deleted_after_merge);
        assert_eq!(plan.lanes[l_f1].merged_at_commit.as_deref(), Some(m1.as_str()));

        // Merge markers: M1 has one MergeInfo to f1's lane; M2 to f2's lane.
        let m1_plan = plan.commits.iter().find(|c| c.sha == m1).unwrap();
        assert_eq!(m1_plan.merges.len(), 1);
        assert_eq!(m1_plan.merges[0].source_tip_sha, f1);
        assert_eq!(m1_plan.merges[0].source_lane, l_f1);
    }

    // --- lane assignment: octopus ----------------------------------------

    #[test]
    fn octopus_gives_one_lane_and_mergeinfo_per_extra_parent() {
        let mut db = GitObjectDb::new();
        let c0 = commit_with_file(&mut db, "base", "b", &[], 1000, "C0");
        let x1 = commit_with_file(&mut db, "x1", "1", &[&c0], 1060, "x1 work");
        let x2 = commit_with_file(&mut db, "x2", "2", &[&c0], 1120, "x2 work");
        let x3 = commit_with_file(&mut db, "x3", "3", &[&c0], 1180, "x3 work");
        let c1 = commit_with_file(&mut db, "main", "m", &[&c0], 1240, "main advances");
        // Octopus: 4 parents (c1 first, then x1,x2,x3).
        let otree = {
            let bb = mk_blob(&mut db, "b");
            let b1 = mk_blob(&mut db, "1");
            let b2 = mk_blob(&mut db, "2");
            let b3 = mk_blob(&mut db, "3");
            let bm = mk_blob(&mut db, "m");
            mk_tree(&mut db, &[
                ("100644", "base", &bb),
                ("100644", "x1", &b1),
                ("100644", "x2", &b2),
                ("100644", "x3", &b3),
                ("100644", "main", &bm),
            ])
        };
        let o = mk_commit(&mut db, &otree, &[&c1, &x1, &x2, &x3], 1300, "Octopus merge");
        let plan = plan_import(&db, &o, &ImportOptions::default()).unwrap();

        // main + three side lanes.
        assert_eq!(plan.lanes.len(), 4);
        let o_plan = plan.commits.iter().find(|c| c.sha == o).unwrap();
        assert_eq!(o_plan.merges.len(), 3, "octopus yields 3 merge edges");
        let tips: Vec<&str> = o_plan.merges.iter().map(|m| m.source_tip_sha.as_str()).collect();
        assert_eq!(tips, vec![x1.as_str(), x2.as_str(), x3.as_str()]);
        // Each side lane distinct.
        let mut lanes: Vec<LaneId> = o_plan.merges.iter().map(|m| m.source_lane).collect();
        lanes.sort();
        lanes.dedup();
        assert_eq!(lanes.len(), 3);
    }

    // --- lane assignment: criss-cross & foxtrot terminate + assign all ----

    fn assert_every_commit_assigned(plan: &ImportPlan) {
        let mut seen: HashSet<&str> = HashSet::new();
        for c in &plan.commits {
            assert!(c.lane < plan.lanes.len(), "lane out of range");
            assert!(seen.insert(&c.sha), "commit appears twice");
        }
        // Parents-before-children in canonical order.
        let idx: HashMap<&str, usize> = plan.commits.iter().enumerate().map(|(i, c)| (c.sha.as_str(), i)).collect();
        for (i, c) in plan.commits.iter().enumerate() {
            for p in &c.parents {
                if let Some(&pi) = idx.get(p.as_str()) {
                    assert!(pi < i, "parent {p} after child {}", c.sha);
                }
            }
        }
    }

    #[test]
    fn criss_cross_terminates_and_assigns_all() {
        // C0; A (branch-a), B (branch-b); M1 on a merges B; M2 on b merges A;
        // then main merges branch-a then branch-b.
        let mut db = GitObjectDb::new();
        let c0 = commit_with_file(&mut db, "base", "base", &[], 1000, "C0");
        let a = commit_with_file(&mut db, "a", "a", &[&c0], 1060, "A");
        let b = commit_with_file(&mut db, "b", "b", &[&c0], 1120, "B");
        // M1 parents [A, B]; M2 parents [B, A].
        let tree_ab = {
            let bb = mk_blob(&mut db, "base");
            let ba = mk_blob(&mut db, "a");
            let bbb = mk_blob(&mut db, "b");
            mk_tree(&mut db, &[("100644", "base", &bb), ("100644", "a", &ba), ("100644", "b", &bbb)])
        };
        let m1 = mk_commit(&mut db, &tree_ab, &[&a, &b], 1180, "M1");
        let m2 = mk_commit(&mut db, &tree_ab, &[&b, &a], 1240, "M2");
        // main (currently at C0) merges branch-a (m1) then branch-b (m2).
        let mm1 = mk_commit(&mut db, &tree_ab, &[&c0, &m1], 1300, "Merge branch 'branch-a'");
        let mm2 = mk_commit(&mut db, &tree_ab, &[&mm1, &m2], 1360, "Merge branch 'branch-b'");
        let plan = plan_import(&db, &mm2, &ImportOptions::default()).unwrap();
        assert_every_commit_assigned(&plan);
        // 7 commits: c0, a, b, m1, m2, mm1, mm2.
        assert_eq!(plan.commits.len(), 7);

        // Exact §3.1 lane structure, derived by hand:
        // main = tip's first-parent chain {mm2, mm1, c0}. Merges expand in
        // canonical order: mm1 first → lane 1 = {m1, a} fork c0 "branch-a";
        // then m1 (now assigned, earlier in canonical order than mm2) →
        // lane 2 = {b} fork c0, subject "M1" matches no pattern → git/<b7>;
        // then mm2 → lane 3 = {m2} forked at b ON LANE 2 (m2's first parent
        // is b), "branch-b"; finally m2's non-first parent a is already on
        // lane 1 → MergeInfo only, no new lane.
        let lane_of = |sha: &str| plan.commits.iter().find(|c| c.sha == sha).unwrap().lane;
        assert_eq!(lane_of(&mm2), MAIN_LANE);
        assert_eq!(lane_of(&mm1), MAIN_LANE);
        assert_eq!(lane_of(&c0), MAIN_LANE);
        assert_eq!(lane_of(&m1), 1);
        assert_eq!(lane_of(&a), 1);
        assert_eq!(lane_of(&b), 2);
        assert_eq!(lane_of(&m2), 3);

        assert_eq!(plan.lanes.len(), 4);
        assert_eq!(plan.lanes[1].name, "branch-a");
        assert_eq!(plan.lanes[2].name, format!("git/{}", &b[..7]));
        assert_eq!(plan.lanes[3].name, "branch-b");
        assert_eq!(plan.lanes[1].fork.as_ref().unwrap().commit_sha, c0);
        assert_eq!(plan.lanes[1].fork.as_ref().unwrap().lane, MAIN_LANE);
        assert_eq!(plan.lanes[2].fork.as_ref().unwrap().commit_sha, c0);
        // The criss-cross signature: branch-b forks off lane 2 at b, not off main.
        assert_eq!(plan.lanes[3].fork.as_ref().unwrap().commit_sha, b);
        assert_eq!(plan.lanes[3].fork.as_ref().unwrap().lane, 2);

        // Merge edges: mm1 consumes lane 1 (tip m1); m1 consumes lane 2 (tip b);
        // mm2 consumes lane 3 (tip m2); m2 points back at lane 1 (tip a).
        let merges_of = |sha: &str| plan.commits.iter().find(|c| c.sha == sha).unwrap().merges.clone();
        assert_eq!(merges_of(&mm1), vec![MergeInfo { source_lane: 1, source_tip_sha: m1.clone() }]);
        assert_eq!(merges_of(&m1), vec![MergeInfo { source_lane: 2, source_tip_sha: b.clone() }]);
        assert_eq!(merges_of(&mm2), vec![MergeInfo { source_lane: 3, source_tip_sha: m2.clone() }]);
        assert_eq!(merges_of(&m2), vec![MergeInfo { source_lane: 1, source_tip_sha: a.clone() }]);
    }

    #[test]
    fn foxtrot_terminates_and_assigns_all() {
        let mut db = GitObjectDb::new();
        let c1 = commit_with_file(&mut db, "base", "c1", &[], 1000, "C1");
        let f1 = commit_with_file(&mut db, "feature", "f1", &[&c1], 1060, "F1");
        let c2 = commit_with_file(&mut db, "base", "c1\nc2", &[&c1], 1120, "C2");
        // Merge main(c2) into feature(f1): first parent f1, second c2.
        let tree = {
            let bf = mk_blob(&mut db, "f1");
            let bb = mk_blob(&mut db, "c1\nc2");
            mk_tree(&mut db, &[("100644", "feature", &bf), ("100644", "base", &bb)])
        };
        let mf = mk_commit(&mut db, &tree, &[&f1, &c2], 1180, "Merge branch 'main' into feature");
        // main fast-forwards to mf → tip = mf, first-parent chain enters feature.
        let plan = plan_import(&db, &mf, &ImportOptions::default()).unwrap();
        assert_every_commit_assigned(&plan);
        // main first-parent chain: mf -> f1 -> c1. c2 is a side lane.
        let lane_of = |sha: &str| plan.commits.iter().find(|c| c.sha == sha).unwrap().lane;
        assert_eq!(lane_of(&mf), MAIN_LANE);
        assert_eq!(lane_of(&f1), MAIN_LANE);
        assert_eq!(lane_of(&c1), MAIN_LANE);
        assert_ne!(lane_of(&c2), MAIN_LANE);
    }

    // --- depth cut --------------------------------------------------------

    #[test]
    fn depth_cut_snapshots_older_history() {
        let mut db = GitObjectDb::new();
        let mut prev: Option<String> = None;
        let mut shas = Vec::new();
        for i in 0..5 {
            let parents: Vec<&str> = prev.iter().map(|s| s.as_str()).collect();
            let sha = commit_with_file(&mut db, "f.txt", &format!("v{i}"), &parents, 1000 + i as i64 * 60, &format!("c{i}"));
            prev = Some(sha.clone());
            shas.push(sha);
        }
        let tip = shas.last().unwrap().clone();
        // main chain newest->oldest = [c4,c3,c2,c1,c0]; depth 2 keeps c4,c3; cut = c2.
        let plan = plan_import(&db, &tip, &ImportOptions { depth: Some(2), keep_imported_branches: false, ..Default::default() }).unwrap();
        // snapshot + c3 + c4 = 3 commits.
        assert_eq!(plan.commits.len(), 3);
        let snap = plan.commits.iter().find(|c| c.is_depth_cut_snapshot).unwrap();
        assert_eq!(snap.sha, shas[2]); // cut point = c2
        assert!(snap.parents.is_empty());
        // Snapshot ops are the full tree (a single Create of f.txt).
        assert_eq!(snap.ops.len(), 1);
        assert!(matches!(&snap.ops[0], FileOp::Create { path, .. } if path == "f.txt"));
        assert_eq!(plan.root_sha, shas[2]);
        // c0, c1 are gone.
        assert!(!plan.commits.iter().any(|c| c.sha == shas[0] || c.sha == shas[1]));
    }

    #[test]
    fn depth_larger_than_history_is_full_import() {
        let mut db = GitObjectDb::new();
        let c0 = commit_with_file(&mut db, "f", "0", &[], 1000, "c0");
        let c1 = commit_with_file(&mut db, "f", "1", &[&c0], 1060, "c1");
        let plan = plan_import(&db, &c1, &ImportOptions { depth: Some(99), keep_imported_branches: false, ..Default::default() }).unwrap();
        assert_eq!(plan.commits.len(), 2);
        assert!(!plan.commits.iter().any(|c| c.is_depth_cut_snapshot));
    }

    // --- determinism ------------------------------------------------------

    #[test]
    fn plan_is_deterministic() {
        let mut db = GitObjectDb::new();
        let i = commit_with_file(&mut db, "README", "r", &[], 1000, "initial");
        let f1 = commit_with_file(&mut db, "f1", "one", &[&i], 1060, "work");
        let mtree = {
            let br = mk_blob(&mut db, "r");
            let bf = mk_blob(&mut db, "one");
            mk_tree(&mut db, &[("100644", "README", &br), ("100644", "f1", &bf)])
        };
        let m = mk_commit(&mut db, &mtree, &[&i, &f1], 1120, "Merge branch 'feat'");
        let p1 = plan_import(&db, &m, &ImportOptions::default()).unwrap();
        let p2 = plan_import(&db, &m, &ImportOptions::default()).unwrap();
        assert_eq!(format!("{p1:?}"), format!("{p2:?}"));
        assert_eq!(serde_json::to_string(&p1).unwrap(), serde_json::to_string(&p2).unwrap());
    }

    // --- keep_imported_branches ------------------------------------------

    #[test]
    fn keep_imported_branches_disables_deletes() {
        let mut db = GitObjectDb::new();
        let i = commit_with_file(&mut db, "README", "r", &[], 1000, "initial");
        let f1 = commit_with_file(&mut db, "f1", "one", &[&i], 1060, "work");
        let mtree = {
            let br = mk_blob(&mut db, "r");
            let bf = mk_blob(&mut db, "one");
            mk_tree(&mut db, &[("100644", "README", &br), ("100644", "f1", &bf)])
        };
        let m = mk_commit(&mut db, &mtree, &[&i, &f1], 1120, "Merge branch 'feat'");
        let plan = plan_import(&db, &m, &ImportOptions { depth: None, keep_imported_branches: true, ..Default::default() }).unwrap();
        assert!(plan.lanes.iter().all(|l| !l.deleted_after_merge));
    }

    // --- LCG fuzz: random small DAGs never panic; invariants hold ---------

    #[test]
    fn fuzz_random_dags_terminate_and_assign() {
        let mut state: u64 = 0xD1B54A32D192ED03;
        let mut next = || {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (state >> 33) as u32
        };

        for trial in 0..200 {
            let mut db = GitObjectDb::new();
            let n = 2 + (next() % 12) as usize;
            let mut shas: Vec<String> = Vec::new();
            for i in 0..n {
                // 0..2 random parents among earlier commits, distinct.
                let mut parents: Vec<String> = Vec::new();
                if i > 0 {
                    let want = (next() % 3) as usize; // 0,1,2 parents
                    for _ in 0..want {
                        let p = shas[(next() as usize) % i].clone();
                        if !parents.contains(&p) {
                            parents.push(p);
                        }
                    }
                }
                let prefs: Vec<&str> = parents.iter().map(|s| s.as_str()).collect();
                let ts = 1000 + (next() % 500) as i64;
                let sha = commit_with_file(&mut db, "f", &format!("t{trial}-c{i}"), &prefs, ts, &format!("c{i}"));
                shas.push(sha);
            }
            let tip = shas.last().unwrap().clone();
            let plan = plan_import(&db, &tip, &ImportOptions::default()).unwrap_or_else(|e| {
                panic!("trial {trial}: plan_import failed: {e}");
            });
            assert_every_commit_assigned(&plan);
            // Random depth cut must also not panic.
            let d = 1 + (next() % 5);
            let _ = plan_import(&db, &tip, &ImportOptions { depth: Some(d), keep_imported_branches: false, ..Default::default() }).unwrap();
        }
    }
}
