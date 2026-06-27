//! Comprehensive coverage for the editor-facing engine surface: file CRUD,
//! rename with stable file_id, delete, the file tree, the history timeline,
//! point-in-time read/restore, snapshot/restore, and remove-vault (with and
//! without trash). All real `asp-core` — no mocks.

use asp_core::Identity;
use asp_desktop_engine::{DesktopEngine, FileAtTime, HistoryEvent};
use std::time::{Duration, Instant};

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

/// Two distinct devices, each running the real in-process engine + iroh driver.
fn env() {
    std::env::set_var("ASP_NO_RELAY", "1"); // hermetic
}

fn add_vault(de: &DesktopEngine, dir: &std::path::Path) -> String {
    std::fs::create_dir_all(dir).unwrap();
    de.add_local_folder(dir).unwrap().id
}

// (The canonical two-managed-folders convergence test lives in integration.rs;
// the editor-specific sync path is covered by `share_ticket_then_peer_sync_converges_editor_files` below.)

#[test]
fn list_and_authorize() {
    env();
    let de = DesktopEngine::new(Identity::from_seed(&[2; 32])).unwrap();
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("V");
    std::fs::create_dir_all(&dir).unwrap();
    let v = de.add_local_folder(&dir).unwrap();
    assert_eq!(de.list_vaults().len(), 1);
    let peer = Identity::from_seed(&[9; 32]).to_ssh_string();
    de.authorize(&v.id, &peer).unwrap();
    assert_eq!(de.list_authorized(&v.id).unwrap().len(), 1);
    assert!(!de.identity_ssh().is_empty());
}

// ---------------- editor file surface ----------------

#[test]
fn write_read_roundtrip_and_tree() {
    env();
    let de = DesktopEngine::new(Identity::from_seed(&[3; 32])).unwrap();
    let root = tempfile::tempdir().unwrap();
    let id = add_vault(&de, &root.path().join("V"));

    de.write_file(&id, "README.md", "# hi\n").unwrap();
    de.write_file(&id, "inbox/quick.md", "- thought\n").unwrap();
    de.write_file(&id, "inbox/nested/deep.md", "deep\n").unwrap();

    assert_eq!(de.read_file(&id, "README.md").unwrap(), Some("# hi\n".to_string()));
    assert_eq!(de.read_file(&id, "inbox/quick.md").unwrap(), Some("- thought\n".to_string()));
    assert_eq!(de.read_file(&id, "missing.md").unwrap(), None);

    let tree = de.files_tree(&id).unwrap();
    // top-level: README.md + inbox dir
    let names: Vec<&str> = tree.iter().map(|n| n.name.as_str()).collect();
    assert!(names.contains(&"README.md"));
    let inbox = tree.iter().find(|n| n.name == "inbox").expect("inbox dir");
    assert!(inbox.is_dir);
    let inbox_kids = inbox.children.as_ref().unwrap();
    assert!(inbox_kids.iter().any(|n| n.name == "quick.md" && !n.is_dir));
    let nested = inbox_kids.iter().find(|n| n.name == "nested").expect("nested dir");
    assert!(nested.is_dir);
    let nested_kids = nested.children.as_ref().unwrap();
    assert!(nested_kids.iter().any(|n| n.name == "deep.md"));

    // on-disk materialization is byte-exact
    assert_eq!(std::fs::read_to_string(root.path().join("V/README.md")).unwrap(), "# hi\n");
}

#[test]
fn edit_overwrites_then_read_reflects_latest() {
    env();
    let de = DesktopEngine::new(Identity::from_seed(&[4; 32])).unwrap();
    let root = tempfile::tempdir().unwrap();
    let id = add_vault(&de, &root.path().join("V"));
    de.write_file(&id, "a.md", "v1\n").unwrap();
    de.write_file(&id, "a.md", "v2\n").unwrap();
    assert_eq!(de.read_file(&id, "a.md").unwrap(), Some("v2\n".to_string()));
}

#[test]
fn rename_moves_content_and_stable_path() {
    env();
    let de = DesktopEngine::new(Identity::from_seed(&[5; 32])).unwrap();
    let root = tempfile::tempdir().unwrap();
    let id = add_vault(&de, &root.path().join("V"));
    de.write_file(&id, "old.md", "content\n").unwrap();
    de.rename_file(&id, "old.md", "new.md").unwrap();
    assert_eq!(de.read_file(&id, "old.md").unwrap(), None, "old path gone");
    assert_eq!(de.read_file(&id, "new.md").unwrap(), Some("content\n".to_string()), "content moved");
    let tree = de.files_tree(&id).unwrap();
    assert!(!tree.iter().any(|n| n.name == "old.md"));
    assert!(tree.iter().any(|n| n.name == "new.md"));
}

#[test]
fn rename_into_subdir_creates_intermediate_dirs() {
    env();
    let de = DesktopEngine::new(Identity::from_seed(&[6; 32])).unwrap();
    let root = tempfile::tempdir().unwrap();
    let id = add_vault(&de, &root.path().join("V"));
    de.write_file(&id, "note.md", "x\n").unwrap();
    de.rename_file(&id, "note.md", "archive/2026/note.md").unwrap();
    assert_eq!(de.read_file(&id, "archive/2026/note.md").unwrap(), Some("x\n".to_string()));
}

#[test]
fn delete_removes_file_and_disk() {
    env();
    let de = DesktopEngine::new(Identity::from_seed(&[7; 32])).unwrap();
    let root = tempfile::tempdir().unwrap();
    let id = add_vault(&de, &root.path().join("V"));
    de.write_file(&id, "gone.md", "bye\n").unwrap();
    assert!(root.path().join("V/gone.md").exists());
    de.delete_file(&id, "gone.md").unwrap();
    assert_eq!(de.read_file(&id, "gone.md").unwrap(), None);
    assert!(!root.path().join("V/gone.md").exists(), "file removed from disk");
    let tree = de.files_tree(&id).unwrap();
    assert!(tree.is_empty(), "tree empty after the only file deleted");
}

#[test]
fn new_file_avoids_name_clash() {
    env();
    let de = DesktopEngine::new(Identity::from_seed(&[8; 32])).unwrap();
    let root = tempfile::tempdir().unwrap();
    let id = add_vault(&de, &root.path().join("V"));
    let p1 = de.new_file(&id, "untitled.md", "# one\n").unwrap();
    let p2 = de.new_file(&id, "untitled.md", "# two\n").unwrap();
    assert_eq!(p1, "untitled.md");
    assert_ne!(p1, p2, "second untitled gets a suffix");
    assert_eq!(de.read_file(&id, &p2).unwrap(), Some("# two\n".to_string()));
}

#[test]
fn history_records_create_edit_rename_delete() {
    env();
    let de = DesktopEngine::new(Identity::from_seed(&[10; 32])).unwrap();
    let root = tempfile::tempdir().unwrap();
    let id = add_vault(&de, &root.path().join("V"));
    de.write_file(&id, "a.md", "v1\n").unwrap();
    de.write_file(&id, "a.md", "v2\n").unwrap();
    de.rename_file(&id, "a.md", "b.md").unwrap();
    de.delete_file(&id, "b.md").unwrap();

    let h: Vec<HistoryEvent> = de.history(&id).unwrap();
    let kinds: Vec<&str> = h.iter().map(|e| e.kind.as_str()).collect();
    assert!(kinds.contains(&"create"), "create event present");
    assert!(kinds.contains(&"edit"), "edit event present");
    assert!(kinds.contains(&"rename"), "rename event present");
    assert!(kinds.contains(&"delete"), "delete event present");
    // monotonic by (ts, lamport)
    let mut sorted = h.clone();
    sorted.sort_by(|a, b| a.ts.cmp(&b.ts).then(a.lamport.cmp(&b.lamport)));
    assert_eq!(h.iter().map(|e| e.id.clone()).collect::<Vec<_>>(),
               sorted.iter().map(|e| e.id.clone()).collect::<Vec<_>>(),
               "history returned in (ts, lamport) order");
    // rename carries the new path
    let rename = h.iter().find(|e| e.kind == "rename").unwrap();
    assert_eq!(rename.path.as_deref(), Some("b.md"));
}

#[test]
fn file_at_time_and_restore_file_at() {
    env();
    let de = DesktopEngine::new(Identity::from_seed(&[11; 32])).unwrap();
    let root = tempfile::tempdir().unwrap();
    let id = add_vault(&de, &root.path().join("V"));
    de.write_file(&id, "evolve.md", "v1\n").unwrap();
    let t_after_first = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64 + 1;
    // sleep so the second edit's ts strictly exceeds the first.
    std::thread::sleep(Duration::from_secs(2));
    de.write_file(&id, "evolve.md", "v2\n").unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;

    // at t_after_first, the file was "v1"
    let past = de.file_at_time(&id, "evolve.md", t_after_first).unwrap();
    let FileAtTime { exists, content, key } = past;
    assert!(exists, "file existed at the earlier time");
    assert_eq!(content.as_deref(), Some("v1\n"));
    assert!(key.contains("evolve.md"), "key tags the path");

    // far-future / now reads live content
    let live = de.file_at_time(&id, "evolve.md", now).unwrap();
    assert!(live.exists);
    assert_eq!(live.content.as_deref(), Some("v2\n"));

    // a file that never existed at that ts is "gone"
    let gone = de.file_at_time(&id, "nope.md", now).unwrap();
    assert!(!gone.exists);
    assert_eq!(gone.key, "gone");

    // restore-to-past re-authors the earlier content as a new edit
    de.restore_file_at(&id, "evolve.md", t_after_first).unwrap();
    assert_eq!(de.read_file(&id, "evolve.md").unwrap(), Some("v1\n".to_string()),
               "restore_file_at brought back the earlier version");

    // restoring a path that didn't exist at that ts is a no-op (false)
    let ok = de.restore_file_at(&id, "nope.md", t_after_first).unwrap();
    assert!(!ok);
}

#[test]
fn snapshot_and_restore_named() {
    env();
    let de = DesktopEngine::new(Identity::from_seed(&[12; 32])).unwrap();
    let root = tempfile::tempdir().unwrap();
    let id = add_vault(&de, &root.path().join("V"));
    de.write_file(&id, "a.md", "a1\n").unwrap();
    de.write_file(&id, "b.md", "b1\n").unwrap();
    let snap = de.snapshot(&id, "checkpoint").unwrap();
    assert!(!snap.is_empty(), "snapshot id returned");
    // mutate further
    de.write_file(&id, "a.md", "a2\n").unwrap();
    de.delete_file(&id, "b.md").unwrap();
    assert_eq!(de.read_file(&id, "a.md").unwrap(), Some("a2\n".to_string()));
    assert_eq!(de.read_file(&id, "b.md").unwrap(), None);
    // restore to the snapshot
    de.restore(&id, "checkpoint").unwrap();
    assert_eq!(de.read_file(&id, "a.md").unwrap(), Some("a1\n".to_string()), "a restored");
    assert_eq!(de.read_file(&id, "b.md").unwrap(), Some("b1\n".to_string()), "b restored");
}

#[test]
fn remove_vault_keeps_disk_when_not_trash() {
    env();
    let de = DesktopEngine::new(Identity::from_seed(&[13; 32])).unwrap();
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("V");
    let id = add_vault(&de, &dir);
    de.write_file(&id, "keep.md", "stay\n").unwrap();
    let removed_path = de.remove_vault(&id, false).unwrap();
    assert_eq!(removed_path, dir.to_string_lossy().to_string());
    // disk untouched
    assert!(dir.join("keep.md").exists(), "files remain on disk after non-trash remove");
    // the vault is forgotten by the engine
    assert!(de.list_vaults().is_empty());
    // operating on a removed id now errors
    assert!(de.read_file(&id, "keep.md").is_err());
}

#[test]
fn remove_vault_trash_moves_dir() {
    env();
    let de = DesktopEngine::new(Identity::from_seed(&[14; 32])).unwrap();
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("V");
    let id = add_vault(&de, &dir);
    de.write_file(&id, "bye.md", "gone\n").unwrap();
    let _ = de.remove_vault(&id, true).unwrap();
    assert!(!dir.exists(), "original dir gone after trash");
    let trash = root.path().join(".asp-trash");
    assert!(trash.exists(), "trash dir created");
    let moved: Vec<_> = std::fs::read_dir(&trash).unwrap().collect();
    assert_eq!(moved.len(), 1, "one vault moved into trash");
    // the moved dir still carries the file
    let entry = moved[0].as_ref().unwrap().path();
    assert!(entry.join("bye.md").exists());
}

#[test]
fn missing_folder_errors_cleanly() {
    env();
    let de = DesktopEngine::new(Identity::from_seed(&[15; 32])).unwrap();
    let bad = "does-not-exist";
    assert!(de.read_file(bad, "x.md").is_err());
    assert!(de.write_file(bad, "x.md", "y").is_err());
    assert!(de.delete_file(bad, "x.md").is_err());
    assert!(de.rename_file(bad, "a", "b").is_err());
    assert!(de.files_tree(bad).is_err());
    assert!(de.history(bad).is_err());
    assert!(de.status(bad).is_err());
    assert!(de.remove_vault(bad, false).is_err());
}

#[test]
fn share_ticket_then_peer_sync_converges_editor_files() {
    // The editor's "share" returns a real iroh ticket; a second device "sync"
    // pulls the file tree. Exercises the real listen/connect path the editor UI
    // drives end-to-end (engine-level).
    env();
    let root = tempfile::tempdir().unwrap();
    let de_a = DesktopEngine::new(Identity::from_seed(&[21; 32])).unwrap();
    let de_b = DesktopEngine::new(Identity::from_seed(&[22; 32])).unwrap();
    let dir_a = root.path().join("A");
    let a = de_a.add_local_folder(&dir_a).unwrap();
    de_a.write_file(&a.id, "shared.md", "from A\n").unwrap();
    de_a.write_file(&a.id, "dir/nested.md", "nested\n").unwrap();
    let ticket = de_a.set_allow_connections(&a.id, true, Some("K")).unwrap().unwrap();

    let dir_b = root.path().join("B");
    let b = de_b.clone_remote(&dir_b, &ticket, Some("K")).unwrap();
    // B now has A's tree (clone_bootstrap materialized it).
    assert_eq!(de_b.read_file(&b.id, "shared.md").unwrap(), Some("from A\n".to_string()));
    assert_eq!(de_b.read_file(&b.id, "dir/nested.md").unwrap(), Some("nested\n".to_string()));

    // B authors an edit; push to A's listener; A materializes it. Retry the
    // sync a few times — iroh's loopback direct-dial can be timing-sensitive
    // under load, and a second sync re-pushes any pending rows.
    de_b.write_file(&b.id, "shared.md", "from A\nedit by B\n").unwrap();
    let got = wait_until(Duration::from_secs(30), || {
        if de_a.read_file(&a.id, "shared.md").unwrap().as_deref() == Some("from A\nedit by B\n") {
            return true;
        }
        let _ = de_b.sync(&b.id, &ticket, Some("K"));
        false
    });
    assert!(got, "A's listener received B's edit through the real net path");

    // A's history now includes both writers' events.
    let h = de_a.history(&a.id).unwrap();
    assert!(h.iter().any(|e| e.kind == "edit"), "edit event in A's history");
}
