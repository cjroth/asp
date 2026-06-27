//! Command-contract tests: every Tauri command's free function, driven against
//! a real `DesktopEngine`, returning the exact shape the frontend `api.ts`
//! depends on (and erroring with a `String` on failure — the Tauri wire form).
//! No display, no Tauri runtime — just the contract the window honors.

use super::*;
use asp_core::Identity;
use std::time::{Duration, Instant};

fn env() {
    std::env::set_var("ASP_NO_RELAY", "1");
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

fn add(eng: &DesktopEngine, dir: &std::path::Path) -> String {
    std::fs::create_dir_all(dir).unwrap();
    eng.add_local_folder(dir).unwrap().id
}

#[test]
fn identity_and_list_shape() {
    env();
    let eng = DesktopEngine::new(Identity::from_seed(&[31; 32])).unwrap();
    let id = get_identity_cmd(&eng);
    assert!(id.starts_with("ssh-ed25519 "), "identity is an ssh pubkey line");
    assert!(list_vaults_cmd(&eng).is_empty(), "no vaults yet");
}

#[test]
fn add_then_tree_read_write_delete_rename_roundtrip() {
    env();
    let eng = DesktopEngine::new(Identity::from_seed(&[32; 32])).unwrap();
    let root = tempfile::tempdir().unwrap();
    let id = add(&eng, &root.path().join("V"));

    // write
    assert!(write_file_cmd(&eng, id.clone(), "a.md".to_string(), "# a\n".into()).is_ok());
    write_file_cmd(&eng, id.clone(), "dir/b.md".to_string(), "b\n".into()).unwrap();
    // read
    assert_eq!(read_file_cmd(&eng, id.clone(), "a.md".into()).unwrap(), Some("# a\n".to_string()));
    assert_eq!(read_file_cmd(&eng, id.clone(), "missing".into()).unwrap(), None);
    // tree
    let tree = files_tree_cmd(&eng, id.clone()).unwrap();
    assert!(tree.iter().any(|n| n.name == "a.md"));
    let dir = tree.iter().find(|n| n.name == "dir").unwrap();
    assert!(dir.is_dir);
    // status
    let st = get_status_cmd(&eng, id.clone()).unwrap();
    assert!(st.files >= 2);
    // rename
    rename_file_cmd(&eng, id.clone(), "a.md".into(), "a2.md".into()).unwrap();
    assert_eq!(read_file_cmd(&eng, id.clone(), "a.md".into()).unwrap(), None);
    assert_eq!(read_file_cmd(&eng, id.clone(), "a2.md".into()).unwrap(), Some("# a\n".to_string()));
    // delete
    delete_file_cmd(&eng, id.clone(), "a2.md".into()).unwrap();
    assert_eq!(read_file_cmd(&eng, id.clone(), "a2.md".into()).unwrap(), None);
    // new_file
    let nf = new_file_cmd(&eng, id.clone(), "untitled.md".into(), "# new\n".into()).unwrap();
    assert_eq!(nf, "untitled.md");
    let nf2 = new_file_cmd(&eng, id.clone(), "untitled.md".into(), "# new2\n".into()).unwrap();
    assert_ne!(nf, nf2);
    // history
    let h = history_cmd(&eng, id.clone()).unwrap();
    assert!(!h.is_empty());
    // file_at_time on a far-future ts reads live content
    let far = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64 + 100_000;
    let fut = file_at_time_cmd(&eng, id.clone(), nf.clone(), far).unwrap();
    assert!(fut.exists);
    assert_eq!(fut.content.as_deref(), Some("# new\n"));
    // remove (non-trash: disk kept)
    let removed = remove_vault_cmd(&eng, id.clone(), false).unwrap();
    assert!(removed.ends_with("V"));
    assert!(list_vaults_cmd(&eng).is_empty());
    // operating on a removed id now returns an Err String (the Tauri wire form)
    assert!(read_file_cmd(&eng, id.clone(), "x".into()).is_err());
}

#[test]
fn errors_are_strings() {
    env();
    let eng = DesktopEngine::new(Identity::from_seed(&[33; 32])).unwrap();
    let err = read_file_cmd(&eng, "nope".into(), "x".into()).unwrap_err();
    assert!(!err.is_empty(), "command errors surface as a non-empty String");
}

#[test]
fn snapshot_restore_flow_through_commands() {
    env();
    let eng = DesktopEngine::new(Identity::from_seed(&[34; 32])).unwrap();
    let root = tempfile::tempdir().unwrap();
    let id = add(&eng, &root.path().join("V"));
    write_file_cmd(&eng, id.clone(), "a.md".into(), "a1\n".into()).unwrap();
    let snap = create_snapshot_cmd(&eng, id.clone(), "c1".into()).unwrap();
    assert!(!snap.is_empty());
    write_file_cmd(&eng, id.clone(), "a.md".into(), "a2\n".into()).unwrap();
    restore_cmd(&eng, id.clone(), "c1".into()).unwrap();
    assert_eq!(read_file_cmd(&eng, id.clone(), "a.md".into()).unwrap(), Some("a1\n".to_string()));
}

#[test]
fn share_and_sync_through_commands_converge() {
    env();
    let root = tempfile::tempdir().unwrap();
    let eng_a = DesktopEngine::new(Identity::from_seed(&[41; 32])).unwrap();
    let eng_b = DesktopEngine::new(Identity::from_seed(&[42; 32])).unwrap();
    let dir_a = root.path().join("A");
    let a = add(&eng_a, &dir_a);
    write_file_cmd(&eng_a, a.clone(), "shared.md".into(), "hi\n".into()).unwrap();
    let ticket = set_allow_connections_cmd(&eng_a, a.clone(), true, Some("K".to_string())).unwrap().unwrap();

    let dir_b = root.path().join("B");
    std::fs::create_dir_all(&dir_b).unwrap();
    let b = clone_remote_cmd(&eng_b, dir_b.to_string_lossy().to_string(), ticket.clone(), Some("K".to_string())).unwrap().id;
    assert_eq!(read_file_cmd(&eng_b, b.clone(), "shared.md".into()).unwrap(), Some("hi\n".to_string()));

    write_file_cmd(&eng_b, b.clone(), "shared.md".into(), "hi\nedited\n".into()).unwrap();
    // Retry the sync — iroh's loopback direct-dial can be timing-sensitive.
    let got = wait_until(Duration::from_secs(30), || {
        if read_file_cmd(&eng_a, a.clone(), "shared.md".into()).unwrap().as_deref() == Some("hi\nedited\n") {
            return true;
        }
        let _ = sync_now_cmd(&eng_b, b.clone(), ticket.clone(), Some("K".to_string()));
        false
    });
    assert!(got, "A's listener got B's edit via the command surface");
}
