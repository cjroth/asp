//! Round 2 of sync-surface probes for the blind spots the cross-surface fuzzer
//! never exercised: live `.aspignore` scope changes, auth wrong-key rejection,
//! offline→reconnect bidirectional catch-up, and clone-at-scale. Same in-process
//! two-device topology as integration.rs / sync_surface_probes.rs.

use asp_core::Identity;
use asp_desktop_engine::DesktopEngine;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

static SERIAL: Mutex<()> = Mutex::new(());
fn serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|p| p.into_inner())
}

/// Removes an env var on drop so a panic mid-test can't leak it into the next
/// serialized test (e.g. a stale ASP_RELAY_URL would wedge later sync tests).
struct EnvGuard(&'static str);
impl Drop for EnvGuard {
    fn drop(&mut self) {
        std::env::remove_var(self.0);
    }
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

struct Pair {
    a: DesktopEngine,
    ida: String,
    dira: std::path::PathBuf,
    b: DesktopEngine,
    idb: String,
    dirb: std::path::PathBuf,
    ticket: String,
    _root: tempfile::TempDir,
}

fn connected_pair(sa: u8, sb: u8) -> Pair {
    std::env::set_var("ASP_NO_RELAY", "1");
    let root = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", root.path());
    let de_a = DesktopEngine::new(Identity::from_seed(&[sa; 32])).unwrap();
    let de_b = DesktopEngine::new(Identity::from_seed(&[sb; 32])).unwrap();

    let dir_a = root.path().join("A");
    std::fs::create_dir_all(&dir_a).unwrap();
    std::fs::write(dir_a.join("README.md"), b"# Shared\n").unwrap();
    let a = de_a.add_local_folder(&dir_a).unwrap();
    let ticket = de_a.set_allow_connections(&a.id, true, Some("S")).unwrap().unwrap();

    let dir_b = root.path().join("B");
    std::fs::create_dir_all(&dir_b).unwrap();
    let b = de_b.clone_remote(&dir_b, &ticket, Some("S")).unwrap();
    assert!(
        wait_until(Duration::from_secs(8), || dir_b.join("README.md").exists()),
        "B cloned the seed file"
    );
    de_a.write_file(&a.id, "README.md", "# Shared\n\nwarmup\n").unwrap();
    assert!(
        wait_until(Duration::from_secs(10), || de_b
            .read_file(&b.id, "README.md")
            .map(|c| c.contains("warmup"))
            .unwrap_or(false)),
        "live connection is warm (write_file pushes A->B)"
    );

    Pair { a: de_a, ida: a.id, dira: dir_a, b: de_b, idb: b.id, dirb: dir_b, ticket, _root: root }
}

/// PROBE 6: a `.aspignore` added/changed AFTER the engine opened must take effect
/// — for the node that authored it AND for a peer it's pushed to. The ignore
/// rules must not freeze at the value loaded when the vault opened. Covers all
/// three authoring paths (local API, peer push, external+rescan).
#[test]
fn aspignore_added_mid_session_takes_effect_both_sides() {
    let _g = serial();
    let p = connected_pair(31, 32);

    // A authors a `.aspignore` AFTER open. It is itself in-scope, so it syncs to B;
    // receiving it must refresh B's scope too (peer-push reload path).
    p.a.write_file(&p.ida, ".aspignore", "*.log\n").unwrap();
    assert!(
        wait_until(Duration::from_secs(10), || p.dirb.join(".aspignore").exists()),
        "the .aspignore file itself syncs to B (it is in scope)"
    );

    // --- A side: a newly-ignored file authored via the API must NOT sync. ---
    p.a.write_file(&p.ida, "a.log", "should be ignored\n").unwrap();
    // Positive control authored right after, so we can wait on it deterministically.
    p.a.write_file(&p.ida, "a.md", "should sync\n").unwrap();
    assert!(
        wait_until(Duration::from_secs(10), || p.dirb.join("a.md").exists()),
        "the in-scope control file synced (connection live)"
    );
    assert!(!p.dirb.join("a.log").exists(), "A's newly-ignored *.log did not sync to B");
    assert!(
        !p.a.list_files(&p.ida).unwrap().iter().any(|f| f.path == "a.log"),
        "A did not even author the ignored *.log (record_write honored the live scope)"
    );

    // --- A external edit + rescan: a newly-ignored file on disk must NOT sync. ---
    std::fs::write(p.dira.join("c.log"), b"external ignored\n").unwrap();
    std::fs::write(p.dira.join("c.md"), b"external kept\n").unwrap();
    p.a.rescan(&p.ida).unwrap();
    assert!(
        wait_until(Duration::from_secs(10), || p.dirb.join("c.md").exists()),
        "the in-scope external file synced after rescan"
    );
    assert!(!p.dirb.join("c.log").exists(), "A's external *.log was filtered by the live scope");

    // --- B side (peer-push reload): B authoring a newly-ignored file must NOT sync. ---
    p.b.write_file(&p.idb, "b.log", "should be ignored\n").unwrap();
    p.b.write_file(&p.idb, "b.md", "should sync\n").unwrap();
    assert!(
        wait_until(Duration::from_secs(10), || p.dira.join("b.md").exists()),
        "B's in-scope control file synced to A"
    );
    assert!(!p.dira.join("b.log").exists(), "B honored the .aspignore it received from A");
    assert!(
        !p.b.list_files(&p.idb).unwrap().iter().any(|f| f.path == "b.log"),
        "B did not author the ignored *.log (peer-pushed .aspignore refreshed B's scope)"
    );
}

/// PROBE 7: cloning with the WRONG auth key must be rejected and must leak no
/// vault content to the rejected peer.
#[test]
fn wrong_auth_key_is_rejected_and_leaks_no_data() {
    let _g = serial();
    std::env::set_var("ASP_NO_RELAY", "1");
    let root = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", root.path());
    let de_a = DesktopEngine::new(Identity::from_seed(&[33; 32])).unwrap();
    let de_b = DesktopEngine::new(Identity::from_seed(&[34; 32])).unwrap();

    let dir_a = root.path().join("A");
    std::fs::create_dir_all(&dir_a).unwrap();
    std::fs::write(dir_a.join("SECRET.md"), b"# top secret\n").unwrap();
    let a = de_a.add_local_folder(&dir_a).unwrap();
    let ticket = de_a.set_allow_connections(&a.id, true, Some("RIGHTKEY")).unwrap().unwrap();

    // Wrong key: must error, must not materialize the vault content.
    let dir_bad = root.path().join("B-bad");
    std::fs::create_dir_all(&dir_bad).unwrap();
    let bad = de_b.clone_remote(&dir_bad, &ticket, Some("WRONGKEY"));
    assert!(bad.is_ok() == false, "clone with the wrong auth key must be rejected (it succeeded)");
    // Give any (incorrect) background materialize a moment, then assert no leak.
    std::thread::sleep(Duration::from_millis(500));
    assert!(!dir_bad.join("SECRET.md").exists(), "rejected peer must not receive any vault content");

    // Right key on a fresh engine: succeeds and converges (proves the listener is
    // healthy and it was the key, not the path, that was rejected).
    let de_c = DesktopEngine::new(Identity::from_seed(&[35; 32])).unwrap();
    let dir_c = root.path().join("C-good");
    std::fs::create_dir_all(&dir_c).unwrap();
    let good = de_c.clone_remote(&dir_c, &ticket, Some("RIGHTKEY"));
    assert!(good.is_ok(), "clone with the right auth key succeeds, got err {:?}", good.err());
    assert!(
        wait_until(Duration::from_secs(8), || dir_c.join("SECRET.md").exists()),
        "correctly-keyed peer converges"
    );
}

/// PROBE 8: disconnect → peer accumulates edits → reconnect → catch-up. The
/// desktop-engine analogue of the CLI's clone_catchup e2e, in the form the
/// desktop API actually supports: B drops its standing connection (`remove_vault`,
/// like closing the app — there is no "pause but keep writable" primitive), A
/// accumulates a batch of edits while B is gone, and B reconnects by re-cloning
/// the same on-disk vault, which must catch up the whole accumulated batch via
/// version-vector anti-entropy while A retains B's pre-disconnect edit (no loss).
#[test]
fn reconnect_after_disconnect_catches_up_accumulated_edits() {
    let _g = serial();
    let p = connected_pair(36, 37);

    // B authors an edit that lands on A over the live connection.
    p.b.write_file(&p.idb, "from_b.md", "B's edit before disconnect\n").unwrap();
    assert!(
        wait_until(Duration::from_secs(10), || p.dira.join("from_b.md").exists()),
        "A received B's edit while connected"
    );

    // B disconnects: tear down its connector (its `.asp` history stays on disk).
    p.b.remove_vault(&p.idb, false).unwrap();
    std::thread::sleep(Duration::from_millis(400));

    // A accumulates a batch of edits while B is gone.
    for i in 0..5 {
        p.a.write_file(&p.ida, &format!("acc/offline{i}.md"), &format!("accumulated {i}\n")).unwrap();
    }
    p.a.write_file(&p.ida, "README.md", "# Shared\n\nv2 authored while B was gone\n").unwrap();

    // B reconnects by re-cloning the same directory (its adopted vault persists on
    // disk); clone_bootstrap runs a version-vector catch-up against A's listener.
    let b2 = p.b.clone_remote(&p.dirb, &p.ticket, Some("S")).unwrap();

    // B must catch up the ENTIRE accumulated batch.
    for i in 0..5 {
        assert!(
            wait_until(Duration::from_secs(12), || p.dirb.join(format!("acc/offline{i}.md")).exists()),
            "B caught up accumulated offline{i}.md on reconnect"
        );
    }
    assert!(
        wait_until(Duration::from_secs(12), || p
            .b
            .read_file(&b2.id, "README.md")
            .map(|c| c.contains("v2 authored while B was gone"))
            .unwrap_or(false)),
        "B caught up the README edit made while it was gone"
    );
    // No loss across the reconnect: A still has B's pre-disconnect edit.
    assert!(p.dira.join("from_b.md").exists(), "A retained B's pre-disconnect edit through the catch-up");
}

/// PROBE 9 (scale + perf): seed A with 5000 files, clone to B, assert the whole
/// set converges and report clone latency. Pushes well past the fuzzer's ~1000.
#[test]
fn clone_at_scale_5000_files_converges() {
    let _g = serial();
    std::env::set_var("ASP_NO_RELAY", "1");
    let root = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", root.path());
    let de_a = DesktopEngine::new(Identity::from_seed(&[38; 32])).unwrap();
    let de_b = DesktopEngine::new(Identity::from_seed(&[39; 32])).unwrap();

    let dir_a = root.path().join("A");
    std::fs::create_dir_all(&dir_a).unwrap();
    const N: usize = 5000;
    for i in 0..N {
        let sub = dir_a.join(format!("notes/{:03}", i / 100));
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join(format!("f{i:04}.md")), format!("# file {i}\n\nbody {i}\n")).unwrap();
    }
    let t_capture = Instant::now();
    let a = de_a.add_local_folder(&dir_a).unwrap();
    let captured = de_a.list_files(&a.id).unwrap().iter().filter(|f| !f.is_dir).count();
    eprintln!("[scale] captured {captured} files in {:?}", t_capture.elapsed());
    assert!(captured >= N, "A captured all {N} files (got {captured})");

    let ticket = de_a.set_allow_connections(&a.id, true, Some("S")).unwrap().unwrap();
    let dir_b = root.path().join("B");
    std::fs::create_dir_all(&dir_b).unwrap();
    let t_clone = Instant::now();
    let b = de_b.clone_remote(&dir_b, &ticket, Some("S")).unwrap();
    let converged = wait_until(Duration::from_secs(90), || {
        de_b.list_files(&b.id).map(|f| f.iter().filter(|e| !e.is_dir).count() >= N).unwrap_or(false)
    });
    let elapsed = t_clone.elapsed();
    let got = de_b.list_files(&b.id).unwrap().iter().filter(|f| !f.is_dir).count();
    eprintln!("[scale] cloned {got}/{N} files in {elapsed:?}");
    assert!(converged, "B converged to all {N} files (got {got}) within 90s; clone took {elapsed:?}");
}

/// PROBE 10: the desktop engine must honor `ASP_RELAY_URL` (a self-hosted relay),
/// mirroring the CLI's `--relay-url`, instead of silently ignoring it and falling
/// back to the public n0 relays. We pin a distinctive relay and assert the minted
/// listening ticket actually advertises it (so a NAT'd desktop peer can dial it).
/// `ASP_NO_RELAY=1` is set too, to prove an explicit relay URL takes precedence.
#[test]
fn ticket_advertises_configured_relay_url() {
    let _g = serial();
    std::env::set_var("ASP_NO_RELAY", "1");
    let _eg = EnvGuard("ASP_RELAY_URL");
    std::env::set_var("ASP_RELAY_URL", "http://relay-test.invalid:9999/");

    let root = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", root.path());
    let de = DesktopEngine::new(Identity::from_seed(&[40; 32])).unwrap();
    let dir = root.path().join("V");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("README.md"), b"# v\n").unwrap();
    let v = de.add_local_folder(&dir).unwrap();
    let ticket = de.set_allow_connections(&v.id, true, Some("S")).unwrap().unwrap();
    // Clear the relay env before asserting so nothing leaks even if the assert fires.
    std::env::remove_var("ASP_RELAY_URL");

    let addr = asp_core::iroh_net::parse_peer(&ticket).unwrap();
    let dbg = format!("{addr:?}");
    assert!(
        dbg.contains("relay-test.invalid"),
        "minted ticket must advertise the configured ASP_RELAY_URL relay; got {dbg}"
    );
}

/// PROBE 11: a real OFFLINE MERGE CONFLICT. Both sides edit the SAME file while
/// partitioned, then reconnect — the core's deterministic fold must converge
/// BOTH surfaces to byte-identical content (the CRDT guarantee), keeping
/// non-overlapping edits and resolving an overlapping one to a single agreed
/// result. This is the "make changes offline on two devices, go back online,
/// they converge" scenario, which the one-directional catch-up probe (PROBE 8)
/// does not cover.
#[test]
fn offline_conflicting_edits_to_same_file_converge() {
    let _g = serial();
    let p = connected_pair(43, 44);

    // Establish a shared multi-line base for a clean (non-overlapping) merge and a
    // single-line base for an overlapping (same-region) conflict; let both land on
    // B over the live link so the two sides share the same base_hash.
    let clean_base = "header\nalpha\nbeta\ngamma\ndelta\nfooter\n";
    p.a.write_file(&p.ida, "doc.md", clean_base).unwrap();
    p.a.write_file(&p.ida, "conflict.md", "shared base line\n").unwrap();
    assert!(
        wait_until(Duration::from_secs(10), || p
            .b
            .read_file(&p.idb, "doc.md")
            .map(|c| c == clean_base)
            .unwrap_or(false)
            && p.b.read_file(&p.idb, "conflict.md").map(|c| c.contains("shared base")).unwrap_or(false)),
        "B received the shared base for both files while connected"
    );

    // GO OFFLINE for real. set_allow_connections(false) only stops NEW dials — it
    // does not sever B's already-established live connection, so edits would just
    // serialize over it. To model two genuinely disconnected devices we drop B's
    // standing connector entirely (remove_vault), then re-open B's folder
    // STANDALONE (add_local_folder → no peer, no connector). B now has the synced
    // base on disk but no link to A.
    p.b.remove_vault(&p.idb, false).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let b_off = p.b.add_local_folder(&p.dirb).unwrap();
    let idb = b_off.id;

    // While disconnected, BOTH sides edit the SAME files via their engine APIs,
    // each branching from the shared base (captured in that side's own log).
    //  - doc.md: A edits the FIRST region, B edits the LAST region (non-overlapping
    //    → a clean 3-way merge must keep both).
    //  - conflict.md: both rewrite the single line differently (overlapping → must
    //    still converge to ONE agreed result on both sides).
    p.a.write_file(&p.ida, "doc.md", "header\nalpha-EDITED-BY-A\nbeta\ngamma\ndelta\nfooter\n").unwrap();
    p.a.write_file(&p.ida, "conflict.md", "A's offline version\n").unwrap();
    p.b.write_file(&idb, "doc.md", "header\nalpha\nbeta\ngamma\ndelta\nfooter-EDITED-BY-B\n").unwrap();
    p.b.write_file(&idb, "conflict.md", "B's offline version\n").unwrap();

    // GO BACK ONLINE: B runs an explicit oneshot sync against A's (still-listening)
    // ticket — bidirectional anti-entropy that exchanges both sides' accumulated
    // offline rows. The deterministic fold then converges both surfaces. Sync twice
    // so each side both pushes and pulls the other's post-merge state.
    p.b.sync(&idb, &p.ticket, Some("S")).unwrap();

    // Both surfaces must converge to byte-identical content for each file.
    let converged = |path: &str| -> bool {
        match (p.a.read_file(&p.ida, path), p.b.read_file(&idb, path)) {
            (Ok(ca), Ok(cb)) => ca == cb && !ca.is_empty(),
            _ => false,
        }
    };
    assert!(
        wait_until(Duration::from_secs(25), || {
            // Re-sync each poll until both sides have exchanged and re-folded.
            let _ = p.b.sync(&idb, &p.ticket, Some("S"));
            converged("doc.md") && converged("conflict.md")
        }),
        "both files converged to identical bytes on A and B after reconnect\n  doc.md A={:?} B={:?}\n  conflict.md A={:?} B={:?}",
        p.a.read_file(&p.ida, "doc.md"),
        p.b.read_file(&idb, "doc.md"),
        p.a.read_file(&p.ida, "conflict.md"),
        p.b.read_file(&idb, "conflict.md"),
    );

    // The clean (non-overlapping) merge kept BOTH offline edits.
    let doc = p.a.read_file(&p.ida, "doc.md").unwrap();
    assert!(doc.contains("alpha-EDITED-BY-A"), "A's offline edit survived the merge: {doc:?}");
    assert!(doc.contains("footer-EDITED-BY-B"), "B's offline edit survived the merge: {doc:?}");

    // The overlapping conflict converged to ONE agreed value on both surfaces.
    let conflict = p.a.read_file(&p.ida, "conflict.md").unwrap();
    assert!(
        conflict.contains("offline version"),
        "conflicting file converged to a concrete merged value: {conflict:?}"
    );
}
