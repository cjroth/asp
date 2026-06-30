//! Disk-engine capture seam (§Capture). `engine.rs` — the sqlite-backed engine
//! the hub and `asp watch` run — was the coldest meaningful spot in coverage
//! (~56%), and its capture path is the NATIVE analogue of the Obsidian bridge
//! that produced this session's worst bugs: it diffs the working tree against the
//! materialized state, authors create/edit/delete/rename rows, and must respect
//! scope. The pure-engine property tests never touch it (they author rows
//! directly). These drive the real engine against a real working tree on disk.
//!
//! Invariants guarded:
//!   - capture is IDEMPOTENT (re-scanning an unchanged tree authors nothing — the
//!     disk-side form of the duplicate-explosion guard);
//!   - create/edit/delete are classified and materialize correctly;
//!   - SCOPE is enforced on capture, at any depth — the hub must never version
//!     `.git/`, `.obsidian/`, or a nested `proj/.git/**` (the leak we shipped);
//!   - rename inference moves content under a new path instead of delete+create.

use asp_core::{Engine, Identity, MergeClass};
use std::fs;
use tempfile::TempDir;

fn open_vault() -> (TempDir, Engine) {
    let dir = tempfile::tempdir().expect("tmpdir");
    let e = Engine::init(dir.path(), Identity::from_seed(&[3; 32])).expect("init");
    (dir, e)
}

#[test]
fn capture_is_idempotent_and_classifies_changes() {
    let (dir, e) = open_vault();
    let p = dir.path().join("a.md");

    fs::write(&p, b"hello world\n").unwrap();
    assert_eq!(e.capture_rescan().unwrap().len(), 1, "create → one row");
    e.materialize().unwrap();

    // The disk-side dup guard: nothing changed on disk → capture authors NOTHING.
    // (If capture re-authored its own materialized output, this is where the
    // runaway would start — the native mirror of the Obsidian loop.)
    assert!(e.capture_rescan().unwrap().is_empty(), "re-scan of unchanged tree must be a no-op");
    assert!(e.capture_rescan().unwrap().is_empty(), "and still a no-op on a third pass");

    fs::write(&p, b"hello there\n").unwrap();
    assert_eq!(e.capture_rescan().unwrap().len(), 1, "edit → one row");
    e.materialize().unwrap();
    assert!(e.capture_rescan().unwrap().is_empty(), "edit then no-change → no-op");

    fs::remove_file(&p).unwrap();
    assert_eq!(e.capture_rescan().unwrap().len(), 1, "delete → one row");
    let files = e.materialize().unwrap();
    assert!(!files.contains_key("a.md"), "deleted file must leave the tree");
}

#[test]
fn capture_never_versions_ignored_dirs_at_any_depth() {
    let (dir, e) = open_vault();
    // Editor/VCS/private junk, including a NESTED repo — the exact shapes that
    // leaked to the hub this session.
    for (rel, body) in [
        (".git/config", &b"[core] root repo"[..]),
        (".obsidian/workspace.json", b"{\"x\":1}"),
        ("proj/.git/objects/pack", b"binary-ish pack bytes"),
        (".context/id_ed25519", b"PRIVATE KEY MATERIAL"),
    ] {
        let full = dir.path().join(rel);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(full, body).unwrap();
    }
    fs::write(dir.path().join("note.md"), b"a real note\n").unwrap();
    fs::create_dir_all(dir.path().join("proj/src")).unwrap();
    fs::write(dir.path().join("proj/src/main.rs"), b"fn main() {}\n").unwrap();

    e.capture_rescan().unwrap();
    let files = e.materialize().unwrap();

    assert!(files.contains_key("note.md"), "real note must be versioned");
    assert!(files.contains_key("proj/src/main.rs"), "nested non-.git source must be versioned");
    for junk in [".git/config", ".obsidian/workspace.json", "proj/.git/objects/pack", ".context/id_ed25519"] {
        assert!(!files.contains_key(junk), "must NEVER version {junk}");
    }
}

#[test]
fn capture_infers_rename_instead_of_delete_plus_create() {
    let (dir, e) = open_vault();
    let old = dir.path().join("old.md");
    let new = dir.path().join("notes").join("new.md");
    let body = b"substantial unique content that survives the move\n";

    fs::write(&old, body).unwrap();
    e.capture_rescan().unwrap();
    e.materialize().unwrap();

    fs::create_dir_all(new.parent().unwrap()).unwrap();
    fs::rename(&old, &new).unwrap();
    let rows = e.capture_rescan().unwrap();
    let files = e.materialize().unwrap();

    assert!(files.contains_key("notes/new.md"), "renamed-to path present");
    assert!(!files.contains_key("old.md"), "renamed-from path gone");
    assert_eq!(fs::read(&new).unwrap(), body, "content preserved across the rename");
    // A rename is ONE authored row, not a delete + a create (which would lose the
    // file's identity/history). Capture also may emit a dir entity for the new
    // folder, so bound it rather than pin it exactly.
    assert!(rows.len() <= 2, "rename should not explode into many rows, got {}", rows.len());
}

#[test]
fn empty_directories_are_tracked_as_entities_then_pruned() {
    // A physically-empty in-scope folder is a first-class entity so it
    // replicates without a marker file; once it holds a real file (or is
    // removed) the entity is pruned. Exercises record_dir_create/delete +
    // empty-dir discovery, which the file-only capture tests never reach.
    let dir = tempfile::tempdir().unwrap();
    let e = Engine::init(dir.path(), Identity::from_seed(&[14; 32])).unwrap();
    let live_dirs = |e: &Engine| -> Vec<String> {
        e.store
            .live_files()
            .unwrap()
            .into_iter()
            .filter(|f| f.merge_class == MergeClass::Dir && !f.deleted)
            .map(|f| f.path)
            .collect()
    };

    fs::create_dir_all(dir.path().join("emptydir")).unwrap();
    e.capture_rescan().unwrap();
    assert!(live_dirs(&e).contains(&"emptydir".to_string()), "empty dir tracked as an entity");

    // Put a file in it → no longer empty → the dir entity is pruned.
    fs::write(dir.path().join("emptydir").join("f.md"), b"content\n").unwrap();
    e.capture_rescan().unwrap();
    assert!(!live_dirs(&e).contains(&"emptydir".to_string()), "non-empty dir is not an entity");
    assert!(e.materialize().unwrap().contains_key("emptydir/f.md"));
}

#[test]
fn record_write_is_a_no_op_on_unchanged_content_and_ignored_paths() {
    let dir = tempfile::tempdir().unwrap();
    let e = Engine::init(dir.path(), Identity::from_seed(&[15; 32])).unwrap();
    assert!(e.record_write("a.md", b"hello\n").unwrap().is_some(), "first write authors a row");
    assert!(e.record_write("a.md", b"hello\n").unwrap().is_none(), "identical re-write authors nothing");
    assert!(e.record_write(".git/config", b"x").unwrap().is_none(), "ignored path authors nothing");
    assert!(e.record_remove("never-existed.md").unwrap().is_none(), "removing an absent file is a no-op");
}

#[test]
fn integrate_rejects_tampered_rows() {
    // A peer's rows are content-addressed + signed; a flipped id or blob hash
    // must be refused, never folded. (The disk engine's integrate validation.)
    let (sd, dd) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let src = Engine::init(sd.path(), Identity::from_seed(&[16; 32])).unwrap();
    let dst = Engine::init(dd.path(), Identity::from_seed(&[17; 32])).unwrap();
    let wr = src.record_write("a.md", b"genuine content\n").unwrap().unwrap();

    let mut bad_id = wr.clone();
    bad_id.row.id = "deadbeefdeadbeef".into();
    assert!(dst.integrate(&bad_id).is_err(), "a row whose id doesn't match its contents is rejected");

    let mut bad_blob = wr.clone();
    if let Some(b) = bad_blob.blobs.first_mut() {
        b.hash = "0000000000000000".into();
    }
    assert!(dst.integrate(&bad_blob).is_err(), "a blob whose hash doesn't match its bytes is rejected");

    // The genuine row integrates fine.
    assert!(dst.integrate(&wr).is_ok());
}

#[test]
fn reopening_an_unchanged_vault_reconciles_to_a_no_op() {
    // The startup-reconciliation path: re-open an existing on-disk vault whose
    // working tree already matches its log. It must author nothing — a spurious
    // row here would resurface as a phantom edit pushed to every peer on launch.
    let dir = tempfile::tempdir().unwrap();
    {
        let e = Engine::init(dir.path(), Identity::from_seed(&[5; 32])).unwrap();
        fs::write(dir.path().join("keep.md"), b"durable content\n").unwrap();
        e.capture_rescan().unwrap();
        e.materialize().unwrap();
    }
    // Fresh process: same identity seed, same dir → open and reconcile.
    let e2 = Engine::open(dir.path(), Identity::from_seed(&[5; 32])).unwrap();
    assert!(e2.reconcile_startup().unwrap().is_empty(), "reopening an unchanged vault must author nothing");
    assert!(e2.materialize().unwrap().contains_key("keep.md"), "and the file is still there");
}

#[test]
fn aspignore_added_after_open_takes_effect_without_reopen() {
    // The scope must NOT freeze at the value loaded when the engine opened.
    // A long-running engine (`asp watch` / the desktop engine) that gains an
    // `.aspignore` mid-session — authored locally or materialized from a peer
    // push — must start honoring it. Regression guard for the dead `reload_scope`
    // (it had zero callers; scope only ever loaded once at `Engine::open`).
    let (dir, e) = open_vault();

    // Before any ignore rules: a *.log file is in scope and captures normally.
    fs::write(dir.path().join("early.log"), b"captured while in scope\n").unwrap();
    assert_eq!(e.capture_rescan().unwrap().len(), 1, "early.log captured (no ignore yet)");

    // Author an `.aspignore` mid-session, then materialize (the chokepoint that
    // refreshes the live scope when `.aspignore` changes on disk).
    fs::write(dir.path().join(".aspignore"), b"*.log\n").unwrap();
    e.capture_rescan().unwrap(); // captures .aspignore; materialize reloads scope
    e.materialize().unwrap();

    // A NEW *.log must now be ignored on every authoring path.
    assert!(
        e.record_write("late.log", b"should be ignored\n").unwrap().is_none(),
        "record_write honors the mid-session .aspignore (no row authored)"
    );
    fs::write(dir.path().join("disk.log"), b"external, should be ignored\n").unwrap();
    fs::write(dir.path().join("kept.md"), b"in scope\n").unwrap();
    let rows = e.capture_rescan().unwrap();
    let files = e.materialize().unwrap();
    assert!(files.contains_key("kept.md"), "in-scope file still captured");
    assert!(!files.contains_key("disk.log"), "external *.log filtered by the live scope");
    assert!(!files.contains_key("late.log"), "API-authored *.log never materialized");
    assert_eq!(rows.len(), 1, "only kept.md authored from the rescan, not the *.log files");
}

#[test]
fn reconcile_cache_skips_unchanged_yet_catches_external_edits() {
    let (dir, e) = open_vault();
    let p = dir.path().join("n.md");

    fs::write(&p, b"v1\n").unwrap();
    assert_eq!(e.capture_rescan().unwrap().len(), 1, "create");
    e.materialize().unwrap();

    // The cache-skip path must be behavior-identical to a full re-hash: an
    // unchanged tree re-scans to a no-op, every time.
    for _ in 0..3 {
        assert!(e.capture_rescan().unwrap().is_empty(), "unchanged tree → no-op (cache skip)");
    }

    // A normal external edit bumps mtime → caught despite the cache.
    std::thread::sleep(std::time::Duration::from_millis(10));
    fs::write(&p, b"v2 edited\n").unwrap();
    assert_eq!(e.capture_rescan().unwrap().len(), 1, "external edit (new mtime) caught");
    e.materialize().unwrap();
    assert!(e.capture_rescan().unwrap().is_empty(), "settles back to no-op");

    // Size-keyed detection: rewrite to a DIFFERENT size but RESTORE the prior
    // mtime, so mtime alone would call it unchanged — the size half of the key
    // still catches it.
    let prior_mtime = fs::metadata(&p).unwrap().modified().unwrap();
    fs::write(&p, b"v3 a noticeably longer body so the byte size differs\n").unwrap();
    fs::OpenOptions::new().write(true).open(&p).unwrap().set_modified(prior_mtime).unwrap();
    assert_eq!(e.capture_rescan().unwrap().len(), 1, "size change caught even with a restored mtime");
}
