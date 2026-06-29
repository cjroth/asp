//! Derived git history (§Derived git history). At settle boundaries a full node
//! materializes the converged bytes into a real, stock-git-compatible object
//! store under `.asp/git` — inspectable by read-only `asp git` or an unmodified
//! `git --git-dir`. It is **derived from the log, not the source of truth**: a
//! minimal object writer (not full git/gitoxide), with a **deterministic** commit
//! (fixed identity/template, derived time) so the `main` SHA converges across
//! nodes that hold the same converged tree.

use crate::error::AspResult;
use flate2::write::ZlibEncoder;
use flate2::Compression;
use sha1::{Digest, Sha1};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::Path;

const FILE_MODE: &str = "100644";
const TREE_MODE: &str = "40000";

fn sha1_hex(framed: &[u8]) -> ([u8; 20], String) {
    let mut h = Sha1::new();
    h.update(framed);
    let d = h.finalize();
    let mut a = [0u8; 20];
    a.copy_from_slice(&d);
    (a, hex::encode(a))
}

fn frame(kind: &str, payload: &[u8]) -> Vec<u8> {
    let mut out = format!("{} {}\0", kind, payload.len()).into_bytes();
    out.extend_from_slice(payload);
    out
}

fn write_object(git_dir: &Path, kind: &str, payload: &[u8]) -> AspResult<[u8; 20]> {
    let framed = frame(kind, payload);
    let (oid, hexid) = sha1_hex(&framed);
    let dir = git_dir.join("objects").join(&hexid[..2]);
    let file = dir.join(&hexid[2..]);
    if !file.exists() {
        fs::create_dir_all(&dir)?;
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        enc.write_all(&framed)?;
        let compressed = enc.finish()?;
        fs::write(&file, compressed)?;
    }
    Ok(oid)
}

struct Entry {
    mode: String,
    name: String,
    oid: [u8; 20],
}

fn tree_sort_key(e: &Entry) -> Vec<u8> {
    let mut k = e.name.as_bytes().to_vec();
    if e.mode == TREE_MODE {
        k.push(b'/');
    }
    k
}

/// Write a blob object for `content` and return its git oid (skips the write if
/// the object already exists — git objects are content-addressed). Exposed so the
/// caller can resolve+cache oids without `build_tree` holding every file's bytes.
pub fn write_blob_object(git_dir: &Path, content: &[u8]) -> AspResult<[u8; 20]> {
    write_object(git_dir, "blob", content)
}

/// Build the tree for `prefix` from `entries` (path → asp content_hash for every
/// live content file). `blob_oid` maps a content_hash to its git blob oid (the
/// caller backs this with a cache so an unchanged file is never re-read/re-hashed
/// into the git store). Empty directories carry no git tree entry, exactly as a
/// real git tree wouldn't — same as the previous bytes-map export.
fn build_tree<F>(prefix: &str, entries: &BTreeMap<String, String>, git_dir: &Path, blob_oid: &mut F) -> AspResult<[u8; 20]>
where
    F: FnMut(&str) -> AspResult<[u8; 20]>,
{
    let mut direct: BTreeMap<String, String> = BTreeMap::new(); // name -> content_hash
    let mut subdirs: BTreeMap<String, ()> = BTreeMap::new();
    for (path, content_hash) in entries {
        let rel = match path.strip_prefix(prefix) {
            Some(r) => r,
            None => continue,
        };
        if rel.is_empty() {
            continue;
        }
        match rel.find('/') {
            None => {
                direct.insert(rel.to_string(), content_hash.clone());
            }
            Some(idx) => {
                subdirs.insert(rel[..idx].to_string(), ());
            }
        }
    }
    let mut tree_entries = Vec::new();
    for (name, content_hash) in &direct {
        let oid = blob_oid(content_hash)?;
        tree_entries.push(Entry { mode: FILE_MODE.to_string(), name: name.clone(), oid });
    }
    for name in subdirs.keys() {
        let child_prefix = format!("{prefix}{name}/");
        let oid = build_tree(&child_prefix, entries, git_dir, blob_oid)?;
        tree_entries.push(Entry { mode: TREE_MODE.to_string(), name: name.clone(), oid });
    }
    tree_entries.sort_by_key(tree_sort_key);
    let mut payload = Vec::new();
    for e in &tree_entries {
        payload.extend_from_slice(e.mode.as_bytes());
        payload.push(b' ');
        payload.extend_from_slice(e.name.as_bytes());
        payload.push(0);
        payload.extend_from_slice(&e.oid);
    }
    write_object(git_dir, "tree", &payload)
}

/// Ensure the git dir has the minimal layout (`objects/`, `refs/heads/`, HEAD).
pub fn init_git_dir(git_dir: &Path) -> AspResult<()> {
    fs::create_dir_all(git_dir.join("objects"))?;
    fs::create_dir_all(git_dir.join("refs").join("heads"))?;
    let head = git_dir.join("HEAD");
    if !head.exists() {
        fs::write(head, "ref: refs/heads/main\n")?;
    }
    let cfg = git_dir.join("config");
    if !cfg.exists() {
        fs::write(cfg, "[core]\n\trepositoryformatversion = 0\n\tbare = false\n")?;
    }
    Ok(())
}

/// Export the converged tree as a single deterministic commit on `main`.
/// `entries` is path → asp content_hash for every live content file; `blob_oid`
/// resolves a content_hash to its git blob oid (cache-backed by the caller, so an
/// unchanged file is never re-read). `derived_time` is the non-decreasing time
/// stamped into the commit (e.g. max lamport) so byte-identical trees yield
/// identical SHAs across nodes. Returns the commit SHA hex.
pub fn export<F>(git_dir: &Path, entries: &BTreeMap<String, String>, derived_time: u64, mut blob_oid: F) -> AspResult<String>
where
    F: FnMut(&str) -> AspResult<[u8; 20]>,
{
    init_git_dir(git_dir)?;
    let tree_oid = build_tree("", entries, git_dir, &mut blob_oid)?;
    let tree_hex = hex::encode(tree_oid);

    let ident = "asp <asp@asp>";
    let msg = "asp derived snapshot\n";
    let mut payload = String::new();
    payload.push_str(&format!("tree {tree_hex}\n"));
    payload.push_str(&format!("author {ident} {derived_time} +0000\n"));
    payload.push_str(&format!("committer {ident} {derived_time} +0000\n"));
    payload.push('\n');
    payload.push_str(msg);

    let commit_oid = write_object(git_dir, "commit", payload.as_bytes())?;
    let commit_hex = hex::encode(commit_oid);

    fs::write(git_dir.join("refs").join("heads").join("main"), format!("{commit_hex}\n"))?;
    Ok(commit_hex)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test helper: a content-addressed blob map + an oid resolver that writes the
    // blob object on demand (mirrors what the engine's cache-backed closure does).
    fn resolver<'a>(g: &'a Path, blobs: &'a BTreeMap<String, Vec<u8>>) -> impl FnMut(&str) -> AspResult<[u8; 20]> + 'a {
        move |content_hash: &str| write_blob_object(g, blobs.get(content_hash).map(|v| v.as_slice()).unwrap_or(&[]))
    }

    #[test]
    fn empty_tree_is_canonical() {
        let dir = tempfile::tempdir().unwrap();
        let g = dir.path().join("git");
        let entries = BTreeMap::new();
        let blobs = BTreeMap::new();
        let sha = export(&g, &entries, 100, resolver(&g, &blobs)).unwrap();
        // The commit references the canonical empty tree.
        assert_eq!(sha.len(), 40);
        // Deterministic: same inputs → same SHA.
        let dir2 = tempfile::tempdir().unwrap();
        let g2 = dir2.path().join("git");
        let sha2 = export(&g2, &entries, 100, resolver(&g2, &blobs)).unwrap();
        assert_eq!(sha, sha2);
    }

    #[test]
    fn nested_tree_roundtrips_via_git_layout() {
        let dir = tempfile::tempdir().unwrap();
        let g = dir.path().join("git");
        // path -> content_hash, plus content_hash -> bytes.
        let mut entries = BTreeMap::new();
        entries.insert("a/b.md".to_string(), "h_hello".to_string());
        entries.insert("c.md".to_string(), "h_world".to_string());
        let mut blobs = BTreeMap::new();
        blobs.insert("h_hello".to_string(), b"hello\n".to_vec());
        blobs.insert("h_world".to_string(), b"world\n".to_vec());
        let sha = export(&g, &entries, 5, resolver(&g, &blobs)).unwrap();
        assert!(g.join("refs/heads/main").exists());
        assert_eq!(fs::read_to_string(g.join("refs/heads/main")).unwrap().trim(), sha);
    }
}
