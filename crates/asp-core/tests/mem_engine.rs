//! MemEngine surface not hit by the convergence/property suites: the whole-set
//! `commit_files` reconcile and the `files_detail` metadata view.

use asp_core::{Identity, MemEngine};
use std::collections::BTreeMap;

fn vault(files: &[(&str, &[u8])]) -> BTreeMap<String, Vec<u8>> {
    files.iter().map(|(p, b)| (p.to_string(), b.to_vec())).collect()
}

#[test]
fn commit_files_reconciles_to_the_desired_set() {
    let e = MemEngine::create(Identity::from_seed(&[20; 32]), "v");

    let rows = e.commit_files(&vault(&[("a.md", b"alpha\n"), ("b.md", b"beta\n")])).unwrap();
    assert_eq!(rows.len(), 2, "two creates");
    assert_eq!(e.files_map().unwrap().len(), 2);

    // Re-commit with `a` unchanged, `b` dropped, `c` added → exactly a delete +
    // a create (the unchanged `a` authors no row).
    let rows2 = e.commit_files(&vault(&[("a.md", b"alpha\n"), ("c.md", b"gamma\n")])).unwrap();
    assert_eq!(rows2.len(), 2);
    let files = e.files_map().unwrap();
    assert!(files.contains_key("a.md") && files.contains_key("c.md"));
    assert!(!files.contains_key("b.md"), "dropped file is gone");

    // Idempotent: committing the same set again authors nothing.
    assert!(e.commit_files(&vault(&[("a.md", b"alpha\n"), ("c.md", b"gamma\n")])).unwrap().is_empty());

    // files_detail exposes per-file metadata (path + not-deleted live rows).
    let detail = e.files_detail();
    assert!(detail.iter().any(|f| f.path == "a.md" && !f.deleted));
    assert!(detail.iter().any(|f| f.path == "c.md" && !f.deleted));
}

#[test]
fn integrate_rejects_tampered_rows() {
    let a = MemEngine::create(Identity::from_seed(&[21; 32]), "v");
    let b = MemEngine::create(Identity::from_seed(&[22; 32]), "v");
    let wr = a.record_write("x.md", b"genuine\n").unwrap().unwrap();

    let mut bad = wr.clone();
    bad.row.id = "deadbeefdeadbeef".into();
    assert!(b.integrate(&bad).is_err(), "a row id that doesn't match its contents is refused");
    assert!(b.integrate(&wr).unwrap(), "the genuine row integrates as new");
}
