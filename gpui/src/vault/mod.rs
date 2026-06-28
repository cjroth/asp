//! Pure-logic modules ported 1:1 from the desktop app's `src/vault/*.ts`, with
//! parity tests mirroring the original vitest suites. These hold the app's
//! deterministic core (naming, tree, tabs, history geometry, markdown, prefs).
pub mod format;
pub mod tree;
