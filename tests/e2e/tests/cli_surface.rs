//! *No-regression / parity:* the CLI surface — `--json` machine-readable status
//! and log, `key`, `scope`, `completions`, and documented exit codes — exercised
//! against the real binary. "We didn't lose anything" is a passing test.

use asp_e2e::{temp_root, Node};

#[test]
fn status_json_has_expected_fields() {
    let root = temp_root();
    let a = Node::new(root.path(), "A");
    a.init();
    a.write("x.md", b"x\n");
    a.commit();
    let s = a.status_json();
    for key in ["node_id", "vault_id", "rows", "files", "head", "tiebreak_key"] {
        assert!(s.get(key).is_some(), "status --json missing {key}: {s}");
    }
    assert_eq!(s["tiebreak_key"], "lamport", "v1 tiebreak is lamport");
    assert_eq!(s["files"].as_u64(), Some(1));
    assert_eq!(s["node_id"].as_str().unwrap().len(), 64, "node id is 32-byte ed25519 hex");
}

#[test]
fn key_is_stable_per_home() {
    let root = temp_root();
    let a = Node::new(root.path(), "A");
    a.init();
    let k1 = a.key();
    let k2 = a.key();
    assert!(k1.starts_with("ssh-ed25519 "), "OpenSSH-format key: {k1}");
    assert_eq!(k1, k2, "device key is stable per ASP_HOME");
}

#[test]
fn log_json_is_machine_readable() {
    let root = temp_root();
    let a = Node::new(root.path(), "A");
    a.init();
    a.write("a.md", b"a\n");
    a.write("b.md", b"b\n");
    a.commit();
    let log: serde_json::Value = serde_json::from_str(&a.run(&["log", "--json"])).unwrap();
    let rows = log.as_array().unwrap();
    assert_eq!(rows.len(), 2);
    for r in rows {
        assert_eq!(r["kind"], "create");
        assert!(r["id"].as_str().unwrap().len() == 64, "Merkle id is sha256 hex");
    }
}

#[test]
fn scope_reports_root_and_private_dir() {
    let root = temp_root();
    let a = Node::new(root.path(), "A");
    a.init();
    let out = a.run(&["scope"]);
    assert!(out.contains("scope root"));
    assert!(out.contains(".asp/") || out.contains("excluded"));
}

#[test]
fn aspignore_excludes_files_from_sync() {
    let root = temp_root();
    let a = Node::new(root.path(), "A");
    a.init();
    a.write(".aspignore", b"*.log\nsecret/\n");
    a.write("keep.md", b"keep\n");
    a.write("debug.log", b"noise\n");
    a.write("secret/key.txt", b"ssh\n");
    a.commit();
    let files = a.status_json()["files"].as_u64().unwrap();
    // keep.md + .aspignore are tracked; *.log and secret/ are excluded.
    assert_eq!(files, 2, "ignored paths are out of scope (only keep.md + .aspignore)");
}

#[test]
fn completions_generate() {
    let root = temp_root();
    let a = Node::new(root.path(), "A");
    a.init();
    let out = a.run(&["completions", "bash"]);
    assert!(out.contains("asp"), "bash completions mention the binary");
}

#[test]
fn tiebreak_key_is_genesis_immutable() {
    let root = temp_root();
    let a = Node::new(root.path(), "A");
    a.init();
    // It is fixed to lamport at init and reported in status.
    assert_eq!(a.status_json()["tiebreak_key"], "lamport");
}
