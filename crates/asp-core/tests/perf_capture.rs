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
