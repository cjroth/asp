//! Probes for sync-surface BLIND SPOTS the cross-surface fuzzer never exercises:
//! empty-directory propagation (`create_dir`), external edits captured by
//! `rescan`, and snapshot `restore` — all under a live peer connection. Each test
//! asserts the CORRECT (converged) behavior, so a failure pins a real bug.
//!
//! Same in-process two-device topology as integration.rs: A shares (listener),
//! B clones and stays connected, so a `record_*` broadcast reaches B with NO
//! explicit sync. These tests probe whether the OTHER engine entry points
//! (create_dir / rescan / restore) push live the same way write_file does.

use asp_core::Identity;
use asp_desktop_engine::DesktopEngine;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

static SERIAL: Mutex<()> = Mutex::new(());
fn serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|p| p.into_inner())
}

fn wait_until(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    cond()
}

/// A + B live-connected. Returns (engine_a, id_a, dir_a, engine_b, id_b, dir_b).
struct Pair {
    a: DesktopEngine,
    ida: String,
    dira: std::path::PathBuf,
    b: DesktopEngine,
    idb: String,
    dirb: std::path::PathBuf,
    _root: tempfile::TempDir,
}

fn connected_pair(sa: u8, sb: u8) -> Pair {
    std::env::set_var("ASP_NO_RELAY", "1");
    let root = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", root.path());
    let de_a = DesktopEngine::new(Identity::from_seed(&[sa; 32])).unwrap();
    let de_b = DesktopEngine::new(Identity::from_seed(&[sb; 32])).unwrap();

    let dir_a = root.path().join("A");
    std::fs::create_dir_all(&dir_a).unwrap();
    std::fs::write(dir_a.join("README.md"), b"# Shared\n").unwrap();
    let a = de_a.add_local_folder(&dir_a).unwrap();
    let ticket = de_a.set_allow_connections(&a.id, true, Some("S")).unwrap().unwrap();

    let dir_b = root.path().join("B");
    std::fs::create_dir_all(&dir_b).unwrap();
    let b = de_b.clone_remote(&dir_b, &ticket, Some("S")).unwrap();
    assert!(
        wait_until(Duration::from_secs(8), || dir_b.join("README.md").exists()),
        "B cloned the seed file"
    );
    // Sanity: the standing connection delivers a normal write_file edit live.
    de_a.write_file(&a.id, "README.md", "# Shared\n\nwarmup\n").unwrap();
    assert!(
        wait_until(Duration::from_secs(10), || de_b
            .read_file(&b.id, "README.md")
            .map(|c| c.contains("warmup"))
            .unwrap_or(false)),
        "live connection is warm (write_file pushes A->B)"
    );

    Pair { a: de_a, ida: a.id, dira: dir_a, b: de_b, idb: b.id, dirb: dir_b, _root: root }
}

/// PROBE 1: an empty directory created via `create_dir` on A should appear on the
/// live-connected peer B (asp-core treats empty in-scope dirs as first-class).
#[test]
fn empty_dir_propagates_to_live_peer() {
    let _g = serial();
    let p = connected_pair(11, 12);

    p.a.create_dir(&p.ida, "empty_folder").unwrap();
    assert!(p.dira.join("empty_folder").is_dir(), "A materialized the empty dir locally");

    let got = wait_until(Duration::from_secs(10), || p.dirb.join("empty_folder").is_dir());
    assert!(got, "B received the empty dir over the live connection");
    // And the engine API view (what the UI renders) lists it as a dir entity.
    let listed = p
        .b
        .list_files(&p.idb)
        .unwrap()
        .iter()
        .any(|f| f.path == "empty_folder" && f.is_dir);
    assert!(listed, "B's API view lists the empty dir");
}

/// PROBE 2: an external edit (a file written directly to disk, behind the engine)
/// captured by `rescan` should propagate to the live-connected peer B — the same
/// way write_file does. (`rescan` calls capture_rescan; the question is whether it
/// broadcasts the resulting rows like create_dir does.)
#[test]
fn external_edit_via_rescan_propagates_to_live_peer() {
    let _g = serial();
    let p = connected_pair(13, 14);

    // Write a brand-new file straight to A's disk, bypassing the engine API.
    std::fs::write(p.dira.join("external.md"), b"# External\n\nwritten behind the engine\n").unwrap();
    // User clicks "refresh": rescan captures the on-disk change into the log.
    p.a.rescan(&p.ida).unwrap();
    // A's own API view must see it (capture worked).
    assert!(
        p.a.list_files(&p.ida).unwrap().iter().any(|f| f.path == "external.md"),
        "A captured the external file via rescan"
    );

    // The live peer must converge to it.
    let got = wait_until(Duration::from_secs(10), || {
        p.b.read_file(&p.idb, "external.md").map(|c| c.contains("written behind")).unwrap_or(false)
    });
    assert!(got, "B received the rescan-captured external edit over the live connection");
}

/// PROBE 3: an external EDIT to an existing tracked file, captured by rescan,
/// should propagate live to B too.
#[test]
fn external_modify_via_rescan_propagates_to_live_peer() {
    let _g = serial();
    let p = connected_pair(15, 16);

    // Modify an already-tracked file directly on disk.
    std::fs::write(p.dira.join("README.md"), b"# Shared\n\nEXTERNALLY MODIFIED\n").unwrap();
    p.a.rescan(&p.ida).unwrap();
    assert!(
        p.a.read_file(&p.ida, "README.md").unwrap().contains("EXTERNALLY MODIFIED"),
        "A captured the external modification"
    );

    let got = wait_until(Duration::from_secs(10), || {
        p.b.read_file(&p.idb, "README.md").map(|c| c.contains("EXTERNALLY MODIFIED")).unwrap_or(false)
    });
    assert!(got, "B received the rescan-captured external modification over the live connection");
}

/// PROBE 4 (control): restore_file_at records the historical bytes as a new edit
/// and DOES broadcast — so the live peer should converge. Confirms the live path
/// itself is healthy (isolating the rescan/restore bug to the missing broadcast).
#[test]
fn restore_file_at_propagates_to_live_peer() {
    let _g = serial();
    let p = connected_pair(17, 18);

    // Establish history: original -> edited. Space by >1s for second-granular ts.
    p.a.write_file(&p.ida, "doc.md", "VERSION ONE\n").unwrap();
    assert!(wait_until(Duration::from_secs(10), || p
        .b
        .read_file(&p.idb, "doc.md")
        .map(|c| c.contains("VERSION ONE"))
        .unwrap_or(false)));
    std::thread::sleep(Duration::from_millis(1100));
    let mid = {
        let h = p.a.history(&p.ida).unwrap();
        h.iter().filter(|e| e.path == "doc.md").map(|e| e.ts).max().unwrap()
    };
    std::thread::sleep(Duration::from_millis(1100));
    p.a.write_file(&p.ida, "doc.md", "VERSION TWO\n").unwrap();

    // Restore A's doc.md to its VERSION ONE content as of `mid`.
    p.a.restore_file_at(&p.ida, "doc.md", mid).unwrap();
    assert_eq!(p.a.read_file(&p.ida, "doc.md").unwrap(), "VERSION ONE\n");

    let got = wait_until(Duration::from_secs(10), || {
        p.b.read_file(&p.idb, "doc.md").map(|c| c == "VERSION ONE\n").unwrap_or(false)
    });
    assert!(got, "B converged to the restore_file_at result over the live connection");
}

/// PROBE 5: a named snapshot taken on A, then `restore`d after a divergent edit,
/// should bring the live peer B back to the snapshot content too. (`restore`
/// authors the rows that revert the vault; the question is whether they broadcast.)
#[test]
fn snapshot_restore_propagates_to_live_peer() {
    let _g = serial();
    let p = connected_pair(19, 20);

    p.a.write_file(&p.ida, "snap.md", "SNAPSHOT STATE\n").unwrap();
    assert!(wait_until(Duration::from_secs(10), || p
        .b
        .read_file(&p.idb, "snap.md")
        .map(|c| c.contains("SNAPSHOT STATE"))
        .unwrap_or(false)));

    // Take a named snapshot of the current state.
    p.a.snapshot(&p.ida, "checkpoint").unwrap();

    // Diverge: edit the file away from the snapshot, and confirm B follows.
    p.a.write_file(&p.ida, "snap.md", "DIVERGED STATE\n").unwrap();
    assert!(wait_until(Duration::from_secs(10), || p
        .b
        .read_file(&p.idb, "snap.md")
        .map(|c| c.contains("DIVERGED STATE"))
        .unwrap_or(false)));

    // Restore the snapshot on A.
    p.a.restore(&p.ida, "checkpoint").unwrap();
    assert_eq!(p.a.read_file(&p.ida, "snap.md").unwrap(), "SNAPSHOT STATE\n");

    // B must converge to the restored snapshot content over the live connection.
    let got = wait_until(Duration::from_secs(10), || {
        p.b.read_file(&p.idb, "snap.md").map(|c| c == "SNAPSHOT STATE\n").unwrap_or(false)
    });
    assert!(got, "B converged to the snapshot restore over the live connection");
}
