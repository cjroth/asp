//! Manual timing harness for the disk-engine capture hot path (run with
//! `cargo test -p asp-core --test perf_capture -- --ignored --nocapture`).
//! Not a CI gate — wall-clock assertions are flaky on shared runners. It exists
//! to put a real before/after number on the capture O(N²) → O(N) fix.

use asp_core::{Engine, Identity};
use std::time::Instant;

#[test]
#[ignore]
fn time_initial_capture_of_a_large_vault() {
    let dir = tempfile::tempdir().unwrap();
    let e = Engine::init(dir.path(), Identity::from_seed(&[1; 32])).unwrap();
    let n = 800;
    for i in 0..n {
        let sub = dir.path().join(format!("d{}", i % 16));
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join(format!("f{i}.md")), format!("# note {i}\n{}\n", "lorem ipsum ".repeat(20))).unwrap();
    }
    let t = Instant::now();
    let rows = e.capture_rescan().unwrap();
    let dt = t.elapsed();
    e.materialize().unwrap();
    eprintln!("INITIAL CAPTURE of {n} files: authored {} rows in {:?}", rows.len(), dt);
    assert!(e.materialize().unwrap().len() >= n as usize);

    // Now a small incremental change against the established vault.
    std::fs::write(dir.path().join("d0").join("f0.md"), "# edited\n").unwrap();
    let t2 = Instant::now();
    let r2 = e.capture_rescan().unwrap();
    eprintln!("INCREMENTAL CAPTURE (1 edit, {n}-file vault): {} rows in {:?}", r2.len(), t2.elapsed());
}

#[test]
#[ignore]
fn time_edit_latency_on_a_large_vault() {
    let dir = tempfile::tempdir().unwrap();
    let e = Engine::init(dir.path(), Identity::from_seed(&[2; 32])).unwrap();
    let n = 8000;
    for i in 0..n {
        let sub = dir.path().join(format!("d{}", i % 32));
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join(format!("f{i}.md")), format!("# note {i}\nbody\n")).unwrap();
    }
    e.capture_rescan().unwrap();
    e.materialize().unwrap();
    e.set_git_export(false); // match the desktop app (no derived git tree)

    // The desktop writeFile path: record_write on an existing file uses the
    // apply_one_edit fast-path and MUST be O(1) — independent of vault size —
    // not an O(N) re-fold/re-materialize of the whole log.
    let mut times = Vec::new();
    for i in 0..30 {
        let t = Instant::now();
        e.record_write("d0/f0.md", format!("# edit {i}\nnew body line\n").as_bytes()).unwrap();
        times.push(t.elapsed());
    }
    times.sort();
    let median = times[times.len() / 2];
    let max = *times.iter().max().unwrap();
    eprintln!("EDIT record_write on {n}-file vault: median {:?}, max {:?}", median, max);
    assert!(max < std::time::Duration::from_millis(50), "edit must be O(1) (was {:?} on {n} files)", max);
}
