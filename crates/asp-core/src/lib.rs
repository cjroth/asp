//! asp-core — the Agent Sync Protocol engine. **One engine everywhere:** the
//! deterministic two-layer fold + 3-way merge, the row/object model, identity &
//! auth, the `authorized_keys` admission logic, the sans-IO sync `Session`, and
//! wire framing are **always-on** and compile to `wasm32` unchanged, so a wasm
//! thin node computes byte-identical state to the native daemon (§Implementation).
//!
//! Only genuinely platform-bound pieces are `cfg`-gated to native: the on-disk
//! `SqliteStore`, the fs-backed `Engine` + derived-git export, and TLS. The fold
//! runs over the small [`store::BlobStore`] seam; the `Session` runs over the
//! [`session::SessionVault`] seam — both implemented by the native `Engine` and
//! the wasm-safe [`memengine::MemEngine`].

// Always-on / wasm-safe surface.
pub mod authkeys;
pub mod branch;
pub mod error;
pub mod fold;
pub mod gitgenesis;
pub mod gitimport;
pub mod gitrecord;
pub mod gitwire;
pub mod identity;
pub mod log;
pub mod memengine;
pub mod merge;
pub mod oid;
pub mod order;
pub mod rbsr;
pub mod scope;
pub mod security;
pub mod session;
pub mod store;
pub mod tag;
pub mod wire;

// Native-only: on-disk SQLite, fs-backed engine + git export, TLS.
#[cfg(not(target_arch = "wasm32"))]
pub mod config;
#[cfg(not(target_arch = "wasm32"))]
pub mod engine;
#[cfg(not(target_arch = "wasm32"))]
pub mod gitbridge;
#[cfg(not(target_arch = "wasm32"))]
pub mod gitexport;
#[cfg(not(target_arch = "wasm32"))]
pub mod gitpolicy;
#[cfg(not(target_arch = "wasm32"))]
pub mod gitpush;
#[cfg(not(target_arch = "wasm32"))]
pub mod gitremote;
#[cfg(not(target_arch = "wasm32"))]
pub mod gitproxy;
#[cfg(not(target_arch = "wasm32"))]
pub mod net;
#[cfg(not(target_arch = "wasm32"))]
pub mod iroh_net;
#[cfg(target_arch = "wasm32")]
pub mod iroh_wasm;
#[cfg(not(target_arch = "wasm32"))]
pub mod sqlite;

pub use authkeys::{AdmitCtx, AuthKey};
pub use branch::{
    build_graph, encode_branch_record, reconcile_branches, version_vector_of, visible_rows, Branch,
    BranchSet, Graph, GraphBranch, GraphNode, GraphTag, VersionVector, Visibility,
};
pub use error::{AspError, AspResult};
pub use fold::{compute_files, fold_order, FoldState};
pub use gitrecord::{
    build_commit_marker_row, build_ingest_row, build_plan_row, GitCommitMarker, GitIngestRecord,
    GitPlanRecord, GitRowIdentity,
};
pub use identity::Identity;
pub use log::{Kind, LogRow, MergeClass, MAIN_BRANCH_ID};
pub use memengine::MemEngine;
pub use order::{NodeId, OrderKey};
pub use session::{Role, Session, SessionVault, Step};
pub use store::{BlobStore, FileRow, MemBlobStore};
pub use tag::{encode_tag_record, reconcile_tags, Tag};
pub use wire::{Msg, WireBlob, WireRow};

#[cfg(not(target_arch = "wasm32"))]
pub use config::VaultConfig;
#[cfg(not(target_arch = "wasm32"))]
pub use engine::Engine;
#[cfg(not(target_arch = "wasm32"))]
pub use sqlite::{GitRemoteRow, SqliteStore as Store};
