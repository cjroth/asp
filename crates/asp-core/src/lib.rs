//! asp-core — the Agent Sync Protocol engine. One core, thin bindings: the
//! object/oid model, the deterministic two-layer fold + 3-way merge, identity &
//! auth, the `authorized_keys` admission logic, the sans-IO sync `Session`, and
//! wire framing all live here and nowhere else (§Implementation). Storage is the
//! SQLite event log (§Data model). I/O (sockets, fs, TLS) is injected by the
//! native driver; this crate is pure protocol/merge/convergence.

pub mod authkeys;
pub mod config;
pub mod engine;
pub mod error;
pub mod fold;
pub mod gitexport;
pub mod identity;
pub mod log;
pub mod merge;
pub mod oid;
pub mod order;
pub mod scope;
pub mod session;
pub mod store;
pub mod wire;

pub use authkeys::AuthKey;
pub use config::VaultConfig;
pub use engine::Engine;
pub use error::{AspError, AspResult};
pub use fold::{compute_files, fold_order};
pub use identity::Identity;
pub use log::{Kind, LogRow, MergeClass};
pub use order::{NodeId, OrderKey};
pub use session::{Role, Session, Step};
pub use store::{FileRow, Store};
pub use wire::Msg;
