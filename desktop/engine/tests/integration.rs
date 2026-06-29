//! The desktop engine drives real in-process convergence — two managed folders,
//! one listening, sync through the same `asp-core` net driver + Session as the
//! CLI. No subprocess, no wasm: the backend links the native engine directly.

use asp_core::Identity;
use asp_desktop_engine::DesktopEngine;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

// These tests mutate process-global env (HOME, ASP_NO_RELAY) and do real iroh
// networking, so they must not run concurrently. Serialize them with a guard so
// the suite is correct under a plain `cargo test` (no --test-threads=1 needed).
static SERIAL: Mutex<()> = Mutex::new(());
fn serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|p| p.into_inner())
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

#[test]
fn two_managed_folders_converge_in_process() {
    let _serial = serial();
    // Hermetic: direct/LAN dialing only (no public relays), like the CLI e2e.
    std::env::set_var("ASP_NO_RELAY", "1");
    // Two devices (distinct identities → distinct iroh NodeIds), one folder each —
    // the realistic desktop topology. iroh dials by key, so a single device
    // syncing two of its own folders to each other would be a self-dial.
    let root = tempfile::tempdir().unwrap();
    // Isolate the per-user config ($HOME/.asp/desktop_folders.json) from the real home.
    std::env::set_var("HOME", root.path());
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
    std::fs::write(dir_b.join("reply.md"), b"hi back\n").unwrap();
    de_b.sync(&b.id, &ticket, Some("S")).unwrap();
    let got = wait_until(Duration::from_secs(8), || dir_a.join("reply.md").exists());
    assert!(got, "A's listener received and materialized B's file");
    assert_eq!(std::fs::read(dir_a.join("reply.md")).unwrap(), b"hi back\n");

    // Status surfaces real engine state.
    let st = de_a.status(&a.id).unwrap();
    assert!(st.rows >= 2);
    assert_eq!(st.listening_ticket, Some(ticket));
}

#[test]
fn list_and_authorize() {
    let _serial = serial();
    let root = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", root.path());
    let de = DesktopEngine::new(Identity::from_seed(&[2; 32])).unwrap();
    let dir = root.path().join("V");
    std::fs::create_dir_all(&dir).unwrap();
    let v = de.add_local_folder(&dir).unwrap();
    assert_eq!(de.list_vaults().len(), 1);
    let peer = Identity::from_seed(&[9; 32]).to_ssh_string();
    de.authorize(&v.id, &peer).unwrap();
    assert_eq!(de.list_authorized(&v.id).unwrap().len(), 1);
    assert!(!de.identity_ssh().is_empty());
}

/// End-to-end exercise of the file surface the Vault Editor UI drives:
/// list/read/write/new/rename/delete, history projection, and read-only
/// time travel + per-file restore — all through the engine forwarders.
#[test]
fn file_surface_crud_history_and_time_travel() {
    let _serial = serial();
    let root = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", root.path());
    let de = DesktopEngine::new(Identity::from_seed(&[3; 32])).unwrap();
    let dir = root.path().join("vault");
    std::fs::create_dir_all(&dir).unwrap();
    // Seed a file on disk so add_local_folder captures it.
    std::fs::write(dir.join("README.md"), b"# Hello\n").unwrap();
    let v = de.add_local_folder(&dir).unwrap();

    // list_files sees the captured file.
    let files = de.list_files(&v.id).unwrap();
    assert!(files.iter().any(|f| f.path == "README.md" && !f.is_dir));

    // read_file round-trips the live content.
    assert_eq!(de.read_file(&v.id, "README.md").unwrap(), "# Hello\n");

    // Wall-clock history is second-granular; space the create from the edits so
    // time travel can address the moment before the edit (real edits are seconds+
    // apart — this just makes the test deterministic).
    std::thread::sleep(Duration::from_millis(1100));

    // write_file edits an existing file (persists to log + disk).
    de.write_file(&v.id, "README.md", "# Hello\n\nedited\n").unwrap();
    assert_eq!(de.read_file(&v.id, "README.md").unwrap(), "# Hello\n\nedited\n");
    assert_eq!(std::fs::read(dir.join("README.md")).unwrap(), b"# Hello\n\nedited\n");

    // write_file on a new path creates it (in a subdir).
    de.write_file(&v.id, "notes/idea.md", "# Idea\n").unwrap();
    assert!(de.list_files(&v.id).unwrap().iter().any(|f| f.path == "notes/idea.md"));
    assert_eq!(de.read_file(&v.id, "notes/idea.md").unwrap(), "# Idea\n");

    // rename preserves content (stable file_id) and moves it on disk.
    de.rename_file(&v.id, "notes/idea.md", "notes/plan.md").unwrap();
    let after_rename = de.list_files(&v.id).unwrap();
    assert!(after_rename.iter().any(|f| f.path == "notes/plan.md"));
    assert!(!after_rename.iter().any(|f| f.path == "notes/idea.md"));
    assert_eq!(de.read_file(&v.id, "notes/plan.md").unwrap(), "# Idea\n");

    // history projects the log with wall-clock ts, a kind string, and a path
    // resolved even for rows that don't carry one (the edit above).
    let hist = de.history(&v.id).unwrap();
    assert!(hist.iter().all(|e| e.ts > 0 && !e.path.is_empty()));
    assert!(hist.iter().any(|e| e.kind == "create"));
    assert!(hist.iter().any(|e| e.kind == "edit" && e.path == "README.md"));
    assert!(hist.iter().any(|e| e.kind == "rename"));

    // delete authors a tombstone and removes it from the live set + disk.
    de.delete_file(&v.id, "notes/plan.md").unwrap();
    assert!(!de.list_files(&v.id).unwrap().iter().any(|f| f.path == "notes/plan.md"));
    assert!(!dir.join("notes/plan.md").exists());

    // Time travel: as of just before the edit, the file existed but without the
    // "edited" line. Use the create row's ts as the lookback point.
    let create_ts = hist.iter().find(|e| e.kind == "create" && e.path == "README.md").unwrap().ts;
    let at = de.read_file_at(&v.id, "README.md", create_ts).unwrap();
    assert!(at.exists);
    assert!(!at.content.contains("edited"), "historical content predates the edit: {:?}", at.content);

    // A file that did not exist yet reads back as not-existing.
    let gone = de.read_file_at(&v.id, "notes/plan.md", create_ts).unwrap();
    assert!(!gone.exists);

    // restore_file_at brings README.md back to its original content as a NEW edit.
    de.restore_file_at(&v.id, "README.md", create_ts).unwrap();
    assert_eq!(de.read_file(&v.id, "README.md").unwrap(), "# Hello\n");

    // status surfaces last_ts now that there are rows.
    let st = de.status(&v.id).unwrap();
    assert!(st.last_ts.is_some());
    assert!(st.rows >= 4);
}

/// Live push: an edit on one node propagates to a persistently-connected peer
/// WITHOUT any explicit `sync` call — the desktop equivalent of two `asp watch`
/// processes converging in real time. Exercises the broadcast-on-`record_*` path
/// and the auto-started persistent connector that `clone_remote` opens.
#[test]
fn live_push_propagates_edits_without_explicit_sync() {
    let _serial = serial();
    std::env::set_var("ASP_NO_RELAY", "1");
    let root = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", root.path());

    // Two devices (distinct identities). A shares; B clones and stays connected.
    let de_a = DesktopEngine::new(Identity::from_seed(&[7; 32])).unwrap();
    let de_b = DesktopEngine::new(Identity::from_seed(&[8; 32])).unwrap();

    let dir_a = root.path().join("A");
    std::fs::create_dir_all(&dir_a).unwrap();
    std::fs::write(dir_a.join("README.md"), b"# Shared\n").unwrap();
    let a = de_a.add_local_folder(&dir_a).unwrap();
    let ticket = de_a.set_allow_connections(&a.id, true, Some("S")).unwrap().unwrap();

    let dir_b = root.path().join("B");
    std::fs::create_dir_all(&dir_b).unwrap();
    // clone_remote auto-opens a persistent connector back to A (live both ways).
    let b = de_b.clone_remote(&dir_b, &ticket, Some("S")).unwrap();
    assert!(wait_until(Duration::from_secs(8), || dir_b.join("README.md").exists()), "B cloned the seed file");

    // B is now simultaneously a connector (to A) AND a listener — both must share
    // B's single endpoint (the formerly-unhandled dual-role case). Minting a
    // ticket proves the listener bound on the same endpoint the connector uses.
    let b_ticket = de_b.set_allow_connections(&b.id, true, Some("S")).unwrap();
    assert!(b_ticket.is_some(), "B serves and connects on one shared endpoint");

    // A edits live → B must receive it via push, with NO sync call anywhere.
    de_a.write_file(&a.id, "README.md", "# Shared\n\nlive edit from A\n").unwrap();
    let got_a_to_b = wait_until(Duration::from_secs(10), || {
        de_b.read_file(&b.id, "README.md").map(|c| c.contains("live edit from A")).unwrap_or(false)
    });
    assert!(got_a_to_b, "B received A's live edit over the standing connection (no sync)");

    // B edits live → A must receive it the same way (bidirectional).
    de_b.write_file(&b.id, "reply.md", "# Reply\n\nhi back, live\n").unwrap();
    let got_b_to_a = wait_until(Duration::from_secs(10), || {
        de_a.read_file(&a.id, "reply.md").map(|c| c.contains("hi back, live")).unwrap_or(false)
    });
    assert!(got_b_to_a, "A received B's live edit over the standing connection (no sync)");
}

/// Folders managed in one session reopen in the next (persisted folder list).
#[test]
fn reopen_saved_persists_folders() {
    let _serial = serial();
    let root = tempfile::tempdir().unwrap();
    // Isolate the per-user config file ($HOME/.asp/desktop_folders.json).
    let home = root.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::env::set_var("HOME", &home);

    let dir = root.path().join("V");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.md"), b"hi\n").unwrap();

    {
        let de = DesktopEngine::new(Identity::from_seed(&[4; 32])).unwrap();
        let _ = de.add_local_folder(&dir).unwrap();
        assert_eq!(de.list_vaults().len(), 1);
    }

    // A fresh engine (new session) reopens the remembered folder.
    let de2 = DesktopEngine::new(Identity::from_seed(&[4; 32])).unwrap();
    assert_eq!(de2.list_vaults().len(), 0, "nothing loaded until reopen_saved");
    let reopened = de2.reopen_saved().unwrap();
    assert_eq!(reopened.len(), 1);
    assert_eq!(de2.list_vaults().len(), 1);
    assert_eq!(de2.read_file(&reopened[0].id, "a.md").unwrap(), "hi\n");

    // remove_vault forgets it, so the next session starts clean.
    de2.remove_vault(&reopened[0].id, false).unwrap();
    let de3 = DesktopEngine::new(Identity::from_seed(&[4; 32])).unwrap();
    assert_eq!(de3.reopen_saved().unwrap().len(), 0);
}

/// The "faster local syncing" toggle: co-host a relay and converge a real clone
/// through it (no public n0 relay involved), then toggle it back off.
#[test]
fn cohosted_local_relay_routes_a_real_clone() {
    let _serial = serial();
    std::env::remove_var("ASP_NO_RELAY");
    std::env::remove_var("ASP_RELAY_URL");
    let root = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", root.path());

    let de_a = DesktopEngine::new(Identity::from_seed(&[21; 32])).unwrap();
    assert!(!de_a.local_relay_on());
    assert!(de_a.set_local_relay(true).unwrap(), "toggle returns the new state");
    assert!(de_a.local_relay_on());
    let relay = de_a.local_relay_url().expect("co-hosted relay url");
    assert!(relay.starts_with("http://127.0.0.1:"), "binds a localhost relay, got {relay}");
    // Idempotent: enabling again is a no-op and keeps the same relay.
    assert!(de_a.set_local_relay(true).unwrap());
    assert_eq!(de_a.local_relay_url().as_deref(), Some(relay.as_str()));

    let dir_a = root.path().join("a");
    std::fs::create_dir_all(&dir_a).unwrap();
    std::fs::write(dir_a.join("note.md"), b"hello via the local relay\n").unwrap();
    let a = de_a.add_local_folder(&dir_a).unwrap();
    let ticket = de_a.set_allow_connections(&a.id, true, None).unwrap().unwrap();

    // Pin the cloner to A's co-hosted relay so the whole exchange is hermetic
    // (no n0). A itself keeps using its relay_override, which takes precedence.
    std::env::set_var("ASP_RELAY_URL", &relay);
    let de_b = DesktopEngine::new(Identity::from_seed(&[22; 32])).unwrap();
    let dir_b = root.path().join("b");
    let b = de_b.clone_remote(&dir_b, &ticket, None).unwrap();
    assert!(
        wait_until(Duration::from_secs(25), || {
            de_b.read_file(&b.id, "note.md").map(|c| c.contains("hello via the local relay")).unwrap_or(false)
        }),
        "the clone converges through the co-hosted relay"
    );

    // Toggling off stops the relay and clears the override.
    assert!(!de_a.set_local_relay(false).unwrap());
    assert!(!de_a.local_relay_on());
    assert!(de_a.local_relay_url().is_none());
    std::env::remove_var("ASP_RELAY_URL");
}
