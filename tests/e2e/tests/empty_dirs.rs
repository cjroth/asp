//! *Capture:* an empty in-scope directory replicates as a first-class,
//! content-free **directory entity** — materialized as a real `mkdir` on every
//! node, with **no marker file** in the vault. The entity's lifecycle is
//! convergent (dropped when the folder gains a real file or is removed) and
//! same-path directories converge (no ` (n)` split) since dirs are identity-by-path.

use asp_e2e::{temp_root, Hub, Node};

const SECRET: &str = "k";

fn ls(dir: &std::path::Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect())
        .unwrap_or_default()
}

#[test]
fn empty_directory_replicates_with_no_marker_file() {
    let root = temp_root();
    let hub = Hub::start(root.path(), "hub", Some(SECRET), &[]);
    let url = hub.url();

    let a = Node::new(root.path(), "A");
    a.init();
    std::fs::create_dir_all(a.dir.join("notes/empty")).unwrap();
    a.commit();
    a.sync(&url, Some(SECRET));

    let b = Node::new(root.path(), "B");
    b.clone_from(&url, Some(SECRET));

    // The folder exists on B as a real directory — and is genuinely empty.
    assert!(b.dir.join("notes/empty").is_dir(), "empty dir replicated to B");
    assert!(ls(&b.dir.join("notes/empty")).is_empty(), "no marker file in the folder");
    // Symmetric: A's own folder has no marker either.
    assert!(ls(&a.dir.join("notes/empty")).is_empty());
}

#[test]
fn dir_entity_drops_when_folder_gets_a_real_file() {
    let root = temp_root();
    let hub = Hub::start(root.path(), "hub", Some(SECRET), &[]);
    let url = hub.url();

    let a = Node::new(root.path(), "A");
    a.init();
    std::fs::create_dir_all(a.dir.join("d")).unwrap();
    a.commit();
    a.sync(&url, Some(SECRET));
    let b = Node::new(root.path(), "B");
    b.clone_from(&url, Some(SECRET));
    assert!(b.dir.join("d").is_dir());

    // Real file added → the folder is no longer empty; it stays via the file.
    a.write("d/real.md", b"hello\n");
    a.commit();
    a.sync(&url, Some(SECRET));
    b.sync(&url, Some(SECRET));
    assert_eq!(b.read_str("d/real.md").as_deref(), Some("hello\n"));
    assert!(b.dir.join("d").is_dir());
    assert_eq!(ls(&b.dir.join("d")), vec!["real.md".to_string()], "only the real file, no marker");
}

#[test]
fn concurrent_same_dir_creation_converges_without_suffix() {
    let root = temp_root();
    let hub = Hub::start(root.path(), "hub", Some(SECRET), &[]);
    let url = hub.url();

    // Shared vault: A inits + syncs, B clones.
    let a = Node::new(root.path(), "A");
    a.init();
    a.write("seed.md", b"seed\n");
    a.sync(&url, Some(SECRET));
    let b = Node::new(root.path(), "B");
    b.clone_from(&url, Some(SECRET));

    // Both independently create the SAME empty folder.
    std::fs::create_dir_all(a.dir.join("shared")).unwrap();
    std::fs::create_dir_all(b.dir.join("shared")).unwrap();
    a.commit();
    b.commit();
    for _ in 0..2 {
        a.sync(&url, Some(SECRET));
        b.sync(&url, Some(SECRET));
    }

    // Converges to ONE folder — directories are identity-by-path, so no `shared (1)`.
    assert!(a.dir.join("shared").is_dir() && b.dir.join("shared").is_dir());
    assert!(!a.dir.join("shared (1)").exists(), "no split — same dir path converges");
    assert!(!b.dir.join("shared (1)").exists());
}
