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
