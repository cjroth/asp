//! Storage abstraction (§Implementation: I/O injected via traits). The
//! convergence-critical fold needs only **blob access**, so that is the one
//! storage seam shared across surfaces: the native [`crate::sqlite::SqliteStore`]
//! (SQLite, on-disk) and the wasm-safe [`MemBlobStore`] (in-memory / OPFS-backed
//! by the host) both implement [`BlobStore`], and the *same* `fold::compute_files`
//! runs over either. Everything else native (the full SQLite schema, on-disk
//! materialize) lives in `sqlite.rs`; the wasm engine keeps its log/config/auth in
//! memory ([`crate::memengine`]).

use crate::error::AspResult;
use crate::log::MergeClass;
use std::cell::RefCell;
use std::collections::HashMap;

/// Content-addressed blob storage — the one seam the deterministic fold needs.
pub trait BlobStore {
    fn put_blob(&self, bytes: &[u8]) -> AspResult<String>;
    fn get_blob(&self, hash: &str) -> AspResult<Option<Vec<u8>>>;
    fn has_blob(&self, hash: &str) -> AspResult<bool>;
}

/// A materialized file row (§files) — the fold's output, surface-independent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileRow {
    pub file_id: String,
    pub path: String,
    pub result_hash: Option<String>,
    pub merge_class: MergeClass,
    pub deleted: bool,
    pub lamport: u64,
    pub site_id: String,
    pub conflict: bool,
}

/// In-memory content-addressed blob store (wasm / tests). Interior-mutable so its
/// `put_blob` matches the `&self` `BlobStore` contract.
#[derive(Default)]
pub struct MemBlobStore {
    blobs: RefCell<HashMap<String, Vec<u8>>>,
}

impl MemBlobStore {
    pub fn new() -> MemBlobStore {
        MemBlobStore::default()
    }
}

impl BlobStore for MemBlobStore {
    fn put_blob(&self, bytes: &[u8]) -> AspResult<String> {
        let h = crate::oid::content_hash(bytes);
        self.blobs.borrow_mut().entry(h.clone()).or_insert_with(|| bytes.to_vec());
        Ok(h)
    }
    fn get_blob(&self, hash: &str) -> AspResult<Option<Vec<u8>>> {
        Ok(self.blobs.borrow().get(hash).cloned())
    }
    fn has_blob(&self, hash: &str) -> AspResult<bool> {
        Ok(self.blobs.borrow().contains_key(hash))
    }
}
