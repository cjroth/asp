---
name: rust-patch-vendored-dep
description: Fork/patch a Rust dependency you don't control (a crates.io or git crate) when you need to add or change something inside it — vendor a local copy, point your deps at it, keep everything else from upstream, and make it target-conditional so the patch only applies where needed. Use when a dependency is missing an API, has a bug, needs a platform-specific impl, or you must add a feature behind its public surface, and a fork/PR upstream isn't practical right now.
---

# Patch a dependency you don't control (Rust)

When you need code *inside* a dependency changed (add a method, implement a trait
for a new platform, fix a bug) and can't wait on upstream, vendor + patch it.

## Fastest: reuse the cargo checkout you already have
For a git dependency, cargo already downloaded the source. Copy it to a writable
dir (don't edit in `~/.cargo` — cargo treats it as immutable and keys on the rev,
so edits there are ignored and may be clobbered):
```bash
cp -a ~/.cargo/git/checkouts/<repo>-<hash>/<shortrev> /path/to/vendor/<repo>
```
For a crates.io crate, get the source via `cargo vendor` or by cloning the repo at
the matching tag.

## Wire your project at the local copy
**Option A — path deps (simplest when the dep is a multi-crate workspace).**
Point the crate(s) you depend on directly at the vendored path; transitive
crates from the same workspace resolve automatically from the vendored workspace
root. Only name the ones you actually depend on:
```toml
foo          = { path = "/path/to/vendor/repo/crates/foo" }
foo_platform = { path = "/path/to/vendor/repo/crates/foo_platform", features = ["x"] }
```
Then `rm Cargo.lock` so it re-resolves. First build recompiles the vendored tree
(it's a new source fingerprint) — slow once, then incremental.

**Option B — `[patch]` (surgical; keeps most of the dep from upstream).**
```toml
[patch."https://github.com/org/repo"]   # or [patch.crates-io]
foo = { path = "/path/to/vendor/repo/crates/foo" }
```
Patched crate must keep the same name/version. Caveat: if the patched crate's
*siblings* are referenced by `workspace = true`/`path = "../sib"`, the standalone
copy can't resolve them — Option A (whole-workspace path) avoids that surgery.

## Make it target/condition-specific
If the patch is only needed on some platforms (e.g. a Linux-only impl), gate it so
other targets use upstream and stay clean:
```toml
[target.'cfg(not(target_os = "macos"))'.dependencies]
foo = { path = "/path/to/vendor/repo/crates/foo", features = ["..."] }

[target.'cfg(target_os = "macos")'.dependencies]
foo = { git = "https://github.com/org/repo", rev = "<rev>", features = ["..."] }
```
Note: vendored paths are absolute/machine-specific — they won't build on another
machine. Prefer `[patch]` with a relative in-repo path, or a git fork, when others
must build it. Document the vendor location (e.g. in a STATUS.md).

## Make the change
Read the crate source first (the checkout IS the ground truth — don't guess the
API from memory/docs of a different version). Add the minimal public surface:
implement the trait, add the `pub fn`, re-export the new type from the crate's lib.
Match the crate's existing feature gating (e.g. `#[cfg(feature = "test-support")]`)
and enable that feature in your dep declaration if the new code is behind it.

## Build gotchas
- Switching a dep git→path (or editing the vendor) re-fingerprints the source →
  full recompile of that crate + everything downstream. Expect a long first build.
- Large native deps OOM at the final link with high parallelism. Use
  `cargo build -j3` / `-j4` if you see `failed to map object file: memory map must
  have a non-zero length` (that's an OOM/disk symptom, not a code error).
- To iterate on just the patched crate, edit the vendor in place and rebuild — only
  it + its dependents recompile (the big upstream crates stay cached).
- Resume long builds: cargo is incremental, just re-run `cargo build`.
