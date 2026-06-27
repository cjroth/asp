//! The desktop engine drives real in-process convergence — two managed folders,
//! one listening, sync through the same `asp-core` net driver + Session as the
//! CLI. No subprocess, no wasm: the backend links the native engine directly.

use asp_core::Identity;
use asp_desktop_engine::DesktopEngine;
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

#[test]
fn two_managed_folders_converge_in_process() {
    // Hermetic: direct/LAN dialing only (no public relays), like the CLI e2e.
    std::env::set_var("ASP_NO_RELAY", "1");
    // Two devices (distinct identities → distinct iroh NodeIds), one folder each —
    // the realistic desktop topology. iroh dials by key, so a single device
    // syncing two of its own folders to each other would be a self-dial.
    let root = tempfile::tempdir().unwrap();
    let de_a = DesktopEngine::new(Identity::from_seed(&[1; 32])).unwrap();
    let de_b = DesktopEngine::new(Identity::from_seed(&[2; 32])).unwrap();

    // Device A: a note + "allow connections" (a per-folder iroh listener).
    let dir_a = root.path().join("A");
    std::fs::create_dir_all(&dir_a).unwrap();
    std::fs::write(dir_a.join("note.md"), b"hello desktop\n").unwrap();
    let a = de_a.add_local_folder(&dir_a).unwrap();
    let ticket = de_a.set_allow_connections(&a.id, true, Some("S")).unwrap().unwrap();

    // Device B: clone from A through the per-folder listener, by ticket.
    let dir_b = root.path().join("B");
    std::fs::create_dir_all(&dir_b).unwrap();
    let b = de_b.clone_remote(&dir_b, &ticket, Some("S")).unwrap();
    assert_eq!(std::fs::read(dir_b.join("note.md")).unwrap(), b"hello desktop\n");
    assert!(!b.vault_id.is_empty(), "B adopted A's vault id");

    // B authors a reply; sync pushes it to A, whose listener materializes it.
    // Retry the sync — iroh's loopback direct-dial can be timing-sensitive.
    std::fs::write(dir_b.join("reply.md"), b"hi back\n").unwrap();
    let got = wait_until(Duration::from_secs(30), || {
        if dir_a.join("reply.md").exists() { return true; }
        let _ = de_b.sync(&b.id, &ticket, Some("S"));
        false
    });
    assert!(got, "A's listener received and materialized B's file");
    assert_eq!(std::fs::read(dir_a.join("reply.md")).unwrap(), b"hi back\n");

    // Status surfaces real engine state.
    let st = de_a.status(&a.id).unwrap();
    assert!(st.rows >= 2);
    assert_eq!(st.listening_ticket, Some(ticket));
}

#[test]
fn list_and_authorize() {
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
