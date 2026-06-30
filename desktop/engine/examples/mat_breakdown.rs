//! Measure per-edit materialize cost on a real vault, desktop-style (git export
//! off). Run: cargo run --release -p asp-desktop-engine --example mat_breakdown -- <vault> <existing_file>
use asp_core::{Engine, Identity};
use std::time::Instant;

fn main() {
    let vault = std::path::PathBuf::from(std::env::args().nth(1).expect("usage: <vault> <existing_file>"));
    let existing = std::env::args().nth(2).expect("need an existing file path");
    let id = Identity::generate();

    let t = Instant::now();
    let eng = Engine::open(&vault, id).expect("open");
    eng.set_git_export(false); // desktop mode
    println!("open: {:?}", t.elapsed());

    // EDIT an existing file -> linear fast-path (no fold, no full I/O).
    let t = Instant::now();
    eng.record_write(&existing, format!("# edited {}\n", now()).as_bytes()).expect("edit");
    println!("EDIT existing file (fast-path):   {:?}  <<< per-keystroke-save", t.elapsed());

    // Edit it again (still fast-path).
    let t = Instant::now();
    eng.record_write(&existing, format!("# edited again {}\n", now()).as_bytes()).expect("edit2");
    println!("EDIT again (fast-path):           {:?}", t.elapsed());

    // CREATE a new file -> diff-based full materialize (fold once, write 1 file).
    let t = Instant::now();
    eng.record_write("__bench_new.md", b"# new file\n").expect("create");
    println!("CREATE new file (diff materialize): {:?}", t.elapsed());

    // CORRECTNESS: the incrementally-persisted files table must be byte-identical
    // to what a full fold of the whole log produces. Any mismatch = divergence.
    let rows = eng.store.all_rows().expect("all_rows");
    let full = asp_core::fold::compute_files(&eng.store, &rows).expect("fold");
    let stored: std::collections::HashMap<_, _> =
        eng.store.all_files().expect("all_files").into_iter().map(|f| (f.file_id.clone(), f)).collect();
    let mut mismatches = 0;
    for f in &full {
        if stored.get(&f.file_id) != Some(f) {
            if mismatches < 5 {
                println!("  MISMATCH {}: full={:?} stored={:?}", f.file_id, f, stored.get(&f.file_id));
            }
            mismatches += 1;
        }
    }
    if full.len() != stored.len() {
        println!("  COUNT MISMATCH: full={} stored={}", full.len(), stored.len());
        mismatches += 1;
    }
    println!("\nCORRECTNESS: {} ({} files, {} mismatches)", if mismatches == 0 { "OK — incremental == full fold" } else { "DIVERGED!" }, full.len(), mismatches);

    fn now() -> u64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64
    }
}
