#![allow(dead_code)] // ported library surface + engine API kept for wiring/tests
//! Pure-logic modules ported 1:1 from the desktop app's `src/vault/*.ts`, with
//! parity tests mirroring the original vitest suites. These hold the app's
//! deterministic core (naming, tree, tabs, history geometry, markdown, prefs).
pub mod format;
pub mod history;
pub mod log;
pub mod markdown;
pub mod prefs;
pub mod pretty_names;
pub mod tabs;
pub mod textbuffer;
pub mod tree;
pub mod vault_meta;
