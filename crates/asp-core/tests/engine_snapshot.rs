//! Snapshot / point-in-time restore + admission, on the disk engine. These are
//! user-facing `asp snapshot`/`restore`/`authorize`/`revoke` paths that the
//! convergence tests never touch.

use asp_core::{Engine, Identity};

const SSH: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIN1SPB1Au9ASedCsH0QN6iz5G+cop6tuxYD8CKoRvwt2 asp";

#[test]
fn snapshot_then_restore_reverts_the_working_tree() {
    let dir = tempfile::tempdir().unwrap();
    let e = Engine::init(dir.path(), Identity::from_seed(&[11; 32])).unwrap();
    let f = dir.path().join("n.md");

    std::fs::write(&f, b"version one\n").unwrap();
    e.capture_rescan().unwrap();
    let snap = e.snapshot("v1").unwrap();
    assert!(!snap.is_empty());

    std::fs::write(&f, b"version two\n").unwrap();
    e.capture_rescan().unwrap();
    assert_eq!(std::fs::read(&f).unwrap(), b"version two\n");

    // Restore by label reverts disk AND records the revert as new rows (the log
    // stays the append-only source of truth).
    let rows = e.restore("v1").unwrap();
    assert!(!rows.is_empty());
    assert_eq!(std::fs::read(&f).unwrap(), b"version one\n", "restore reverts disk content");

    // state_as_of reflects the (restored) current content.
    let st = e.state_as_of(i64::MAX).unwrap();
    assert_eq!(st.get("n.md").map(|b| b.as_slice()), Some(&b"version one\n"[..]));

    // An unknown target (not a label, not a parseable time) errors.
    assert!(e.restore("no-such-target-xyz").is_err());

    // Restore "as of" a far-future unix timestamp resolves the time path and
    // yields the current state (a no-op revert) — exercises parse_time_arg.
    let rows2 = e.restore("9999999999").unwrap();
    assert!(rows2.is_empty(), "restoring to 'now-ish' authors nothing");
    assert_eq!(std::fs::read(&f).unwrap(), b"version one\n");
}

#[test]
fn restore_removes_files_absent_from_the_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let e = Engine::init(dir.path(), Identity::from_seed(&[13; 32])).unwrap();
    std::fs::write(dir.path().join("keep.md"), b"keep\n").unwrap();
    e.capture_rescan().unwrap();
    let snap = e.snapshot("base").unwrap();
    assert!(!snap.is_empty());

    // Add a file after the snapshot, then restore — the new file must be removed.
    std::fs::write(dir.path().join("added.md"), b"added later\n").unwrap();
    e.capture_rescan().unwrap();
    assert!(dir.path().join("added.md").exists());

    e.restore("base").unwrap();
    assert!(!dir.path().join("added.md").exists(), "restore drops files not in the snapshot");
    assert!(dir.path().join("keep.md").exists());
}

// `file_at(path, t)` is the snappy single-blob history read used by the desktop
// history slider. It must return EXACTLY what `state_as_of(t).get(path)` would —
// same path resolution (renames), same content — while reading only one blob.
// This also exercises the lazy-content fold: a 3-way merge must still produce the
// same merged bytes whether materialized eagerly or folded for a point-in-time.
#[test]
fn file_at_matches_state_as_of_across_edits_renames_and_merges() {
    let dir = tempfile::tempdir().unwrap();
    let e = Engine::init(dir.path(), Identity::from_seed(&[21; 32])).unwrap();

    // A short timeline of distinct wall-clock instants.
    e.record_write("a.md", b"one\n").unwrap();
    let t1 = e.store.max_ts().unwrap().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    e.record_write("a.md", b"one\ntwo\n").unwrap();
    e.record_write("b.md", b"bee\n").unwrap();
    let t2 = e.store.max_ts().unwrap().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    e.record_rename("a.md", "renamed.md").unwrap();
    e.record_remove("b.md").unwrap();
    let t3 = e.store.max_ts().unwrap().unwrap();

    // At each instant and for several paths, file_at == state_as_of.get(path).
    for t in [t1, t2, t3, i64::MAX] {
        let snap = e.state_as_of(t).unwrap();
        for path in ["a.md", "b.md", "renamed.md", "missing.md"] {
            let via_file_at = e.file_at(path, t).unwrap();
            let via_snapshot = snap.get(path).cloned();
            assert_eq!(via_file_at, via_snapshot, "file_at != state_as_of for {path} @ {t}");
        }
    }
    // Spot-check the semantics the equivalence rides on.
    assert_eq!(e.file_at("a.md", t1).unwrap().as_deref(), Some(&b"one\n"[..]));
    assert_eq!(e.file_at("renamed.md", t1).unwrap(), None, "renamed name didn't exist yet at t1");
    assert_eq!(e.file_at("a.md", t3).unwrap(), None, "old name gone after rename");
    assert_eq!(e.file_at("renamed.md", t3).unwrap().as_deref(), Some(&b"one\ntwo\n"[..]));
    assert_eq!(e.file_at("b.md", t3).unwrap(), None, "deleted file is absent");
}

// materialize is O(changed): it skips re-writing a file whose hash is unchanged.
// But it must still RE-HEAL a live file that vanished from disk behind the engine
// (the cheap `exists()` check), or the on-disk tree would silently drift from the
// log on the next unrelated edit.
#[test]
fn materialize_reheals_a_live_file_deleted_from_disk() {
    let dir = tempfile::tempdir().unwrap();
    let e = Engine::init(dir.path(), Identity::from_seed(&[31; 32])).unwrap();
    e.record_write("keep.md", b"content\n").unwrap();
    let p = dir.path().join("keep.md");
    assert!(p.exists());

    // Delete it from disk behind the engine (no rescan). The log still says it's live.
    std::fs::remove_file(&p).unwrap();
    assert!(!p.exists());

    // An unrelated edit triggers materialize; it must re-create keep.md (the heal),
    // not skip it as "unchanged".
    e.record_write("other.md", b"x\n").unwrap();
    assert!(p.exists(), "materialize re-healed the externally-deleted live file");
    assert_eq!(std::fs::read(&p).unwrap(), b"content\n");
}

// The derived-git head is a deterministic function of the tree + derived time, so
// the content_hash→git_oid cache must reproduce the exact same SHA on a warm-cache
// re-export as a cold one — otherwise nodes holding the same log could disagree on
// the git head.
#[test]
fn git_head_is_stable_across_rematerialize_via_the_blob_cache() {
    let dir = tempfile::tempdir().unwrap();
    let e = Engine::init(dir.path(), Identity::from_seed(&[33; 32])).unwrap();
    e.record_write("a/x.md", b"hello\n").unwrap();
    e.record_write("b.md", b"world\n").unwrap();
    let head1 = std::fs::read_to_string(e.git_dir.join("refs/heads/main")).unwrap();
    assert!(!head1.trim().is_empty());

    // capture_rescan with no on-disk changes authors no rows → identical tree and
    // identical max-lamport → the re-export (now resolving blob oids from the cache)
    // must yield the SAME commit SHA.
    e.capture_rescan().unwrap();
    let head2 = std::fs::read_to_string(e.git_dir.join("refs/heads/main")).unwrap();
    assert_eq!(head1, head2, "warm-cache re-export reproduces the identical deterministic SHA");
}

// record_write takes an incremental fast path for a local linear edit (skipping
// the whole-log re-fold). Its result MUST be identical to what a full re-fold
// produces — same files table, same derived git head, same disk — or the two
// would drift the first time a peer push forces a full materialize.
#[test]
fn fast_path_edit_matches_a_full_refold() {
    let dir = tempfile::tempdir().unwrap();
    let e = Engine::init(dir.path(), Identity::from_seed(&[34; 32])).unwrap();
    e.record_write("a.md", b"one\n").unwrap(); // create — full path
    e.record_write("a.md", b"one\ntwo\n").unwrap(); // edit — FAST path
    e.record_write("b/c.md", b"hi\n").unwrap(); // create in subdir — full path
    e.record_write("b/c.md", b"hi there\n").unwrap(); // edit — FAST path

    let snap = |e: &Engine| -> Vec<(String, Option<String>, bool, String)> {
        let mut v: Vec<_> = e
            .store
            .live_files()
            .unwrap()
            .into_iter()
            .map(|f| (f.path, f.result_hash, f.deleted, f.merge_class.as_str().to_string()))
            .collect();
        v.sort();
        v
    };
    let files_before = snap(&e);
    let head_before = std::fs::read_to_string(e.git_dir.join("refs/heads/main")).unwrap();
    assert_eq!(std::fs::read(dir.path().join("a.md")).unwrap(), b"one\ntwo\n");
    assert_eq!(std::fs::read(dir.path().join("b/c.md")).unwrap(), b"hi there\n");

    // Force a full re-fold from the log; nothing must change.
    e.materialize().unwrap();
    assert_eq!(snap(&e), files_before, "fast-path files table matches a full re-fold");
    assert_eq!(
        std::fs::read_to_string(e.git_dir.join("refs/heads/main")).unwrap(),
        head_before,
        "fast-path git head matches a full re-fold"
    );
}

#[test]
fn authorize_and_revoke_admission_keys() {
    let dir = tempfile::tempdir().unwrap();
    let e = Engine::init(dir.path(), Identity::from_seed(&[12; 32])).unwrap();

    e.authorize(SSH, None, false, "manual").unwrap();
    let node_hex = e.store.authkeys().unwrap()[0].node_id.clone();
    assert!(e.store.authkey_by_node(&node_hex).unwrap().is_some());

    assert!(e.revoke(&node_hex).unwrap(), "revoke removes the key");
    assert!(!e.revoke(&node_hex).unwrap(), "revoking again is a no-op");

    // A non-ssh line is rejected.
    assert!(e.authorize("not a key line", None, false, "x").is_err());
}
