//! Hermetic integration test for the desktop git-bridge slice (git-bridge §7.2):
//! `DesktopEngine::clone_git` → `git_status` → `git_pull`, driven against a real
//! git smart-HTTP server (no external network). Reuses the e2e `gitfix` harness,
//! which CGI-execs `git http-backend` over a canned bare repo.
//!
//! These tests mutate process-global env (`HOME`, keyring/relay opt-outs) so the
//! `DesktopEngine` never touches the real `~/.asp` or spawns a relay; they serialize
//! through [`ENV_LOCK`] so their differing `HOME` values never race.

use std::path::Path;
use std::process::Command;
use std::sync::Mutex;

/// Serializes the env-mutating git-bridge tests (each sets its own throwaway `HOME`).
static ENV_LOCK: Mutex<()> = Mutex::new(());

use asp_core::Identity;
use asp_desktop_engine::DesktopEngine;
use asp_e2e::gitfix::{linear_basic, open_branches, GitHttpServer};

/// Skip gracefully unless a usable system git is present (the fixture builds and the
/// server both shell out to `git`).
fn git_available() -> bool {
    match Command::new("git").arg("version").output() {
        Ok(o) if o.status.success() => true,
        _ => {
            eprintln!("SKIP: system `git` not found; git-bridge clone test requires git >= 2.30");
            false
        }
    }
}

#[test]
fn clone_git_then_status_and_pull() {
    if !git_available() {
        return;
    }
    let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    // Isolate all desktop app state under a throwaway HOME, and keep the clone off
    // the OS keyring / any relay.
    let home = tempfile::tempdir().expect("home tmp");
    std::env::set_var("HOME", home.path());
    std::env::set_var("ASP_GIT_DISABLE_KEYRING", "1");
    std::env::set_var("ASP_NO_RELAY", "1");

    // A hermetic smart-HTTP git server over the canned `linear_basic` fixture.
    let repo = linear_basic();
    let server = GitHttpServer::spawn(repo.repo_root());
    let url = server.repo_url(repo.name());

    let engine = DesktopEngine::new(Identity::generate()).expect("engine");

    let dest = home.path().join("cloned-vault");
    let info = engine
        .clone_git(&dest, &url, None, None, false)
        .unwrap_or_else(|e| panic!("clone_git failed: {e}"));

    // The clone produced a real vault, registered + persisted.
    assert!(!info.vault_id.is_empty(), "clone yielded an empty vault_id");
    assert!(dest.join(".asp/asp.db").exists(), "clone did not initialize the vault db");
    assert!(
        engine.list_vaults().iter().any(|v| v.id == info.id),
        "cloned folder was not registered",
    );
    assert_persisted(home.path(), &dest);

    // git_status maps the core (snake_case) status onto the camelCase DTO.
    let status = engine.git_status(&info.id).expect("git_status").expect("has a remote");
    let sv = serde_json::to_value(&status).unwrap();
    assert_eq!(sv["remoteUrl"], serde_json::Value::String(url.clone()));
    assert_eq!(sv["frozen"], false);
    assert_eq!(sv["policy"], "manual");
    assert!(sv["atSha"].is_string(), "atSha should be the ingested tip after a clone");

    // Pulling immediately after a clone finds nothing new (up to date).
    let pull = serde_json::to_value(engine.git_pull(&info.id).expect("git_pull")).unwrap();
    assert_eq!(pull["upToDate"], true, "fresh clone should be up to date");
    assert_eq!(pull["frozen"], false);
    assert_eq!(pull["newCommits"], 0);

    // --- push path (git-bridge §7.2): edit → pending diff → push → verify upstream.
    // The `linear_basic` bare has `http.receivepack=true`, so smart-HTTP push works.
    engine.write_file(&info.id, "pushed.md", "hello from asp\n").expect("write_file");

    // The new file shows up as pending before we push.
    let pending = engine.git_pending_diff(&info.id).expect("pending diff");
    assert!(pending.files_changed >= 1, "the new file should be pending");
    assert!(pending.paths.iter().any(|p| p == "pushed.md"), "pending diff lists the new path");

    let summary = engine.git_push(&info.id, "add pushed.md").expect("git_push");
    let pushed_sha = summary.pushed_sha.expect("push produced a commit");
    assert_eq!(summary.commits, 1, "one plan → one commit");

    // The bare repo now carries our commit (sha + message) on some ref.
    let bare = repo.repo_root().join(format!("{}.git", repo.name()));
    let log_out = Command::new("git")
        .args(["-C", bare.to_str().unwrap(), "log", "--all", "--format=%H %s"])
        .output()
        .expect("git log on bare");
    let log = String::from_utf8_lossy(&log_out.stdout);
    assert!(log.contains(&pushed_sha), "bare log should contain the pushed sha:\n{log}");
    assert!(log.contains("add pushed.md"), "bare log should show our commit message:\n{log}");

    // After the push the tree is clean — nothing left pending.
    let after = engine.git_pending_diff(&info.id).expect("pending diff after push");
    assert_eq!(after.files_changed, 0, "clean tree after push");

    // A vault with no git remote returns None (the web `GitStatus | null` contract).
    let plain = home.path().join("plain-vault");
    let plain_info = engine.add_local_folder(&plain).expect("add local");
    assert!(engine.git_status(&plain_info.id).expect("status").is_none());
}

/// `--all-branches` clone (`specs/git-open-branches.md` §5): the engine imports every
/// unmerged `refs/heads/*` of the `open_branches` fixture as a live ASP branch, so its
/// `list_branches` carries them (minus the skipped `stale-pointer`, minus `main`).
#[test]
fn clone_git_all_branches_imports_open_branches() {
    if !git_available() {
        return;
    }
    let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let home = tempfile::tempdir().expect("home tmp");
    std::env::set_var("HOME", home.path());
    std::env::set_var("ASP_GIT_DISABLE_KEYRING", "1");
    std::env::set_var("ASP_NO_RELAY", "1");

    let repo = open_branches();
    let server = GitHttpServer::spawn(repo.repo_root());
    let url = server.repo_url(repo.name());

    let engine = DesktopEngine::new(Identity::generate()).expect("engine");
    let dest = home.path().join("all-branches-vault");
    let info = engine
        .clone_git(&dest, &url, None, None, true)
        .unwrap_or_else(|e| panic!("clone_git --all-branches failed: {e}"));

    // The live open branches from the fixture (ref-name order), with `feature-1`
    // deduped to `feature-1-2` (it collides with the merged PR#1 branch name) and
    // `stale-pointer` skipped (reachable from HEAD). `main` is always present.
    let mut names: Vec<String> = engine.list_branches(&info.id).expect("list_branches").into_iter().map(|b| b.name).collect();
    names.sort();
    for expected in ["feat/simple", "feature-1-2", "main", "nested/deep", "orphan", "with-merge"] {
        assert!(names.iter().any(|n| n == expected), "expected live branch {expected} in {names:?}");
    }
    assert!(!names.iter().any(|n| n == "stale-pointer"), "stale-pointer is reachable → skipped, not a branch: {names:?}");

    // A plain clone of the SAME repo imports only the merged-PR side branches — no live
    // open branches — proving `all_branches:false` stays base-behavior.
    let plain_dest = home.path().join("plain-open-branches-vault");
    let plain = engine.clone_git(&plain_dest, &url, None, None, false).expect("plain clone");
    let plain_names: Vec<String> = engine.list_branches(&plain.id).expect("list_branches").into_iter().map(|b| b.name).collect();
    assert!(!plain_names.iter().any(|n| n == "feat/simple" || n == "orphan"), "plain clone must not import open branches: {plain_names:?}");
}

/// The clone must be recorded in `~/.asp/desktop_folders.json` with `git:true`, so a
/// restart reopens it and re-arms the pull tick.
fn assert_persisted(home: &Path, dest: &Path) {
    let cfg = std::fs::read_to_string(home.join(".asp/desktop_folders.json")).expect("folders cfg");
    let entries: serde_json::Value = serde_json::from_str(&cfg).unwrap();
    let arr = entries.as_array().expect("array");
    let dest_s = dest.to_string_lossy();
    let found = arr
        .iter()
        .find(|e| e["path"] == serde_json::Value::String(dest_s.to_string()))
        .expect("cloned folder persisted");
    assert_eq!(found["git"], true, "persisted git folder must carry git:true");
}
