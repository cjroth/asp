//! Measure backend op latency at scale. Seeds N files, then times the operations
//! the UI calls per interaction. Reveals O(N)-per-op costs (e.g. materialize).
//!
//!   cargo run --release -p asp-desktop-engine --example bench_ops -- 1000 4000
use asp_core::Identity;
use asp_desktop_engine::DesktopEngine;
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let nfiles: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(1000);
    let nedits: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
    let tmp = std::env::temp_dir().join(format!("aspbench-{}", std::process::id()));
    let vault = tmp.join("vault");
    std::fs::create_dir_all(&vault).unwrap();
    for i in 0..nfiles {
        let sub = vault.join(format!("dir{:02}", i % 20));
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join(format!("note-{:05}.md", i)), format!("# Note {i}\n\nbody {i}\n")).unwrap();
    }
    std::env::set_var("HOME", &tmp);

    let de = DesktopEngine::new(Identity::generate()).unwrap();
    let t = Instant::now();
    let v = de.add_local_folder(&vault).unwrap();
    println!("add_local_folder({nfiles} files, capture):  {:>8.1}ms", t.elapsed().as_secs_f64() * 1000.0);

    for k in 0..nedits {
        let i = k % nfiles.max(1);
        let p = format!("dir{:02}/note-{:05}.md", i % 20, i);
        let _ = de.write_file(&v.id, &p, &format!("# Note {i}\n\nedit {k}\n"));
    }
    let rows = de.status(&v.id).unwrap().rows;

    let time = |label: &str, f: &mut dyn FnMut()| {
        let t = Instant::now();
        f();
        println!("{label:<40} {:>8.1}ms", t.elapsed().as_secs_f64() * 1000.0);
    };

    println!("\n--- vault: {nfiles} files, {rows} db rows ---");
    time("list_files", &mut || { de.list_files(&v.id).unwrap(); });
    time("read_file (1)", &mut || { de.read_file(&v.id, "dir00/note-00000.md").unwrap(); });
    time("history (fold whole log)", &mut || { de.history(&v.id).unwrap(); });
    time("get_status", &mut || { de.status(&v.id).unwrap(); });
    time("write_file edit (materialize)", &mut || { de.write_file(&v.id, "dir00/note-00000.md", "# edited\n\nnew body\n").unwrap(); });
    time("write_file new (create+materialize)", &mut || { de.write_file(&v.id, "zz-new.md", "# new\n").unwrap(); });
    time("delete_file (materialize)", &mut || { de.delete_file(&v.id, "zz-new.md").unwrap(); });

    let _ = std::fs::remove_dir_all(&tmp);
}
