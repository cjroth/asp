//! *PITR:* named snapshot is exact and skew-free; restore rolls the working tree
//! back; "state as of T" wall-clock restore is best-effort by `ts`.

use asp_e2e::{temp_root, Node};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[test]
fn named_snapshot_restore_is_exact() {
    let root = temp_root();
    let a = Node::new(root.path(), "A");
    a.init();
    a.write("doc.md", b"one\n");
    a.commit();
    a.snapshot("v1");

    a.write("doc.md", b"two\n");
    a.write("extra.md", b"new file\n");
    a.commit();
    assert_eq!(a.read_str("doc.md").as_deref(), Some("two\n"));

    a.restore("v1");
    assert_eq!(a.read_str("doc.md").as_deref(), Some("one\n"), "snapshot restore is exact");
    assert!(!a.exists("extra.md"), "files added after the snapshot are removed on restore");
}

#[test]
fn restore_as_of_wall_clock_time() {
    let root = temp_root();
    let a = Node::new(root.path(), "A");
    a.init();
    a.write("doc.md", b"early\n");
    a.commit();

    let t_mid = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    std::thread::sleep(Duration::from_millis(2100)); // cross a wall-clock second

    a.write("doc.md", b"late\n");
    a.write("later.md", b"added late\n");
    a.commit();
    assert_eq!(a.read_str("doc.md").as_deref(), Some("late\n"));

    // Restore to the moment between the two writes.
    a.restore(&t_mid.to_string());
    assert_eq!(a.read_str("doc.md").as_deref(), Some("early\n"), "as-of-T folds rows with ts ≤ T");
    assert!(!a.exists("later.md"));
}
