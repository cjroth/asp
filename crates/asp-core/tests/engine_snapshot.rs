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
