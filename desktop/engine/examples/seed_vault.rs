//! Seed a large vault for performance testing the desktop app end-to-end.
//!
//! Writes `<nfiles>` markdown files (spread across subdirs) into `<dir>`, captures
//! them into an asp vault, then applies `<nedits>` edits for history variety. Run
//! it with `HOME` pointed at a throwaway dir so it also writes that home's
//! `~/.asp/desktop_folders.json` — the app then auto-loads the vault on launch.
//!
//!   HOME=/tmp/h cargo run -p asp-desktop-engine --example seed_vault -- /tmp/h/vault 1000 200
use asp_core::Identity;
use asp_desktop_engine::DesktopEngine;
use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dir = PathBuf::from(args.get(1).expect("usage: seed_vault <dir> [nfiles] [nedits]"));
    let nfiles: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(1000);
    let nedits: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);

    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("README.md"), "# Massive vault\n\nSeeded for performance testing.\n").unwrap();
    for i in 0..nfiles {
        let sub = dir.join(format!("dir{:02}", i % 20));
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(
            sub.join(format!("note-{:05}.md", i)),
            format!("# Note {i}\n\n- item one\n- item two\n\nSome **body** text for note {i}.\n"),
        )
        .unwrap();
    }

    let de = DesktopEngine::new(Identity::generate()).unwrap();
    let info = de.add_local_folder(&dir).unwrap();

    for k in 0..nedits {
        let i = k % nfiles.min(200).max(1);
        let path = format!("dir{:02}/note-{:05}.md", i % 20, i);
        let _ = de.write_file(&info.id, &path, &format!("# Note {i}\n\nedit pass {k}\n\n- a\n- b\n"));
    }

    let st = de.status(&info.id).unwrap();
    eprintln!("seeded: {nfiles} files, {} db rows, vault={} id={}", st.rows, dir.display(), info.id);
}
