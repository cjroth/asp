//! Integration tests for the native git transport + object store layer
//! (`asp_core::gitbridge`, spec §2/§2.1/§6.3/§8) against the hermetic smart-HTTP
//! fixture server (`asp_e2e::gitfix`).
//!
//! Everything here drives **real git wire bytes**: the [`GitHttpServer`] CGI-execs
//! `git http-backend`, so `ls_remote`/`fetch_pack`/`push_pack` speak protocol v2 (and
//! v0 receive-pack for push) exactly as they would to GitHub. The SSH path is
//! exercised via a fake `ssh` shim on `$ASP_GIT_SSH` that execs a local
//! `git upload-pack`, giving a genuine v2-over-pipe exchange without an sshd.
//!
//! All tests skip gracefully (printing a reason) when system `git` is absent.

use std::path::Path;
use std::process::{Command, Output};

use asp_core::gitbridge::{
    self, fetch_pack, git_oid, git_oid_bytes, ls_remote, push_pack, write_pack, GitAuth,
    GitBridgeError, GitObjectKind, GitRemoteSpec, RemoteStore,
};
use asp_core::gitwire::GitUrl;
use asp_e2e::gitfix::{
    all_fixtures, force_rewrite_tip, linear_basic, FixtureRepo, GitHttpServer,
};

// ── test harness helpers ───────────────────────────────────────────────────

/// Skip unless `git >= 2.30` is available.
fn git_available() -> bool {
    let out = match Command::new("git").arg("version").output() {
        Ok(o) if o.status.success() => o,
        _ => {
            eprintln!("SKIP: system `git` not found; git transport tests require git >= 2.30");
            return false;
        }
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let ver = text.split_whitespace().nth(2).unwrap_or("");
    let mut parts = ver.split('.');
    let major: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let minor: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    if (major, minor) < (2, 30) {
        eprintln!("SKIP: git {ver} < 2.30");
        return false;
    }
    true
}

/// Run an async future to completion on a fresh current-thread runtime.
fn block<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(f)
}

/// An HTTPS-shaped spec pointing at the (plain-HTTP) fixture server — the transport
/// uses the base string verbatim, so `http://` loopback stands in for a real host.
fn http_spec(url: &str, auth: GitAuth) -> GitRemoteSpec {
    GitRemoteSpec { url: GitUrl::Https { base: url.to_string() }, auth }
}

/// A hermetic git invocation against a bare repo (`--git-dir`).
fn bare_git(bare: &Path, args: &[&str]) -> Output {
    let mut full = vec!["--git-dir".to_string(), bare.to_string_lossy().to_string()];
    full.extend(args.iter().map(|s| s.to_string()));
    Command::new("git")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .args(&full)
        .output()
        .expect("spawn git")
}

fn bare_git_ok(bare: &Path, args: &[&str]) -> String {
    let out = bare_git(bare, args);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Count the objects reachable from every ref (`git rev-list --objects --all`).
fn rev_list_object_count(repo: &FixtureRepo) -> usize {
    let out = repo.git_ok(&["rev-list", "--objects", "--all"]);
    out.lines().filter(|l| !l.trim().is_empty()).count()
}

// ── ls_remote ───────────────────────────────────────────────────────────────

#[test]
fn ls_remote_reports_head_symref_and_refs_for_every_fixture() {
    if !git_available() {
        return;
    }
    for (name, build) in all_fixtures() {
        let repo = build();
        let head = repo.head();
        let server = GitHttpServer::spawn(repo.repo_root());
        let spec = http_spec(&server.repo_url(name), GitAuth::Anonymous);

        let refs = block(ls_remote(&spec)).unwrap_or_else(|e| panic!("{name}: ls_remote: {e}"));

        assert_eq!(
            refs.default_branch.as_deref(),
            Some("main"),
            "{name}: default branch from HEAD symref"
        );
        let main = refs
            .refs
            .iter()
            .find(|r| r.name == "refs/heads/main")
            .unwrap_or_else(|| panic!("{name}: no refs/heads/main"));
        assert_eq!(main.oid, head, "{name}: main tip oid");
        assert_eq!(
            refs.default_branch_oid(),
            Some(head.as_str()),
            "{name}: default_branch_oid resolves"
        );
        assert!(
            refs.refs.iter().any(|r| r.name == "HEAD"),
            "{name}: HEAD advertised"
        );
    }
}

// ── fetch_pack ───────────────────────────────────────────────────────────────

#[test]
fn fetch_pack_full_clone_decodes_all_objects() {
    if !git_available() {
        return;
    }
    for (name, build) in all_fixtures() {
        let repo = build();
        let head = repo.head();
        let expected = rev_list_object_count(&repo);
        let server = GitHttpServer::spawn(repo.repo_root());
        let spec = http_spec(&server.repo_url(name), GitAuth::Anonymous);

        let outcome = block(fetch_pack(&spec, std::slice::from_ref(&head), &[], None, |_| {}))
            .unwrap_or_else(|e| panic!("{name}: fetch_pack: {e}"));

        let tmp = tempfile::tempdir().unwrap();
        let mut store = RemoteStore::open(tmp.path(), &gitbridge::remote_id(name)).unwrap();
        store
            .record_fetch(&outcome.pack, &[("refs/heads/main".into(), head.clone())])
            .unwrap_or_else(|e| panic!("{name}: record_fetch: {e}"));

        assert_eq!(
            store.object_count() as usize,
            expected,
            "{name}: decoded object count == rev-list --objects --all"
        );
        // The tip decodes to a commit and is present.
        assert!(store.has(&head), "{name}: store has tip");
        let (kind, _) = store.get_object(&head).expect("tip object");
        assert_eq!(kind, GitObjectKind::Commit, "{name}: tip is a commit");
    }
}

#[test]
fn fetch_pack_with_haves_at_tip_yields_no_new_objects() {
    if !git_available() {
        return;
    }
    let repo = linear_basic();
    let head = repo.head();
    let server = GitHttpServer::spawn(repo.repo_root());
    let spec = http_spec(&server.repo_url("linear_basic"), GitAuth::Anonymous);

    // Fetch again advertising the tip as a have → server has nothing new to send.
    let outcome = block(fetch_pack(&spec, std::slice::from_ref(&head), std::slice::from_ref(&head), None, |_| {}))
        .expect("incremental fetch");

    let tmp = tempfile::tempdir().unwrap();
    let mut store = RemoteStore::open(tmp.path(), "incremental").unwrap();
    store.record_fetch(&outcome.pack, &[]).unwrap();
    assert_eq!(store.object_count(), 0, "have=tip → empty pack, nothing to fetch");
}

// ── token auth ───────────────────────────────────────────────────────────────

#[test]
fn token_server_rejects_anonymous_and_accepts_token() {
    if !git_available() {
        return;
    }
    let repo = linear_basic();
    let server = GitHttpServer::spawn_with_token(repo.repo_root(), "s3cr3t-pat");
    let url = server.repo_url("linear_basic");

    let anon = http_spec(&url, GitAuth::Anonymous);
    let err = block(ls_remote(&anon)).expect_err("anonymous must be rejected");
    assert!(
        matches!(err, GitBridgeError::Auth),
        "expected typed Auth error, got {err:?}"
    );

    let authed = http_spec(&url, GitAuth::Token("s3cr3t-pat".into()));
    let refs = block(ls_remote(&authed)).expect("token clone should succeed");
    assert_eq!(refs.default_branch.as_deref(), Some("main"));
}

// ── pack writer + push ───────────────────────────────────────────────────────

/// Build a `{blob, tree, commit}` triple on top of `parent`, returning
/// `(commit_oid, pack)`. The commit's tree holds a single file `file.txt`.
fn build_commit_on(parent: &str, file_contents: &str, message: &str, ts: i64) -> (String, Vec<u8>) {
    let blob = file_contents.as_bytes().to_vec();
    let blob_oid = git_oid_bytes(GitObjectKind::Blob, &blob);

    let mut tree = Vec::new();
    tree.extend_from_slice(b"100644 file.txt\0");
    tree.extend_from_slice(&blob_oid);
    let tree_oid = git_oid(GitObjectKind::Tree, &tree);

    let parent_line = if parent.is_empty() {
        String::new()
    } else {
        format!("parent {parent}\n")
    };
    let commit = format!(
        "tree {tree_oid}\n{parent_line}author ASP <asp@asp.test> {ts} +0000\ncommitter ASP <asp@asp.test> {ts} +0000\n\n{message}\n"
    )
    .into_bytes();
    let commit_oid = git_oid(GitObjectKind::Commit, &commit);

    let pack = write_pack(&[
        (GitObjectKind::Commit, commit),
        (GitObjectKind::Tree, tree),
        (GitObjectKind::Blob, blob),
    ]);
    (commit_oid, pack)
}

#[test]
fn push_pack_lands_a_locally_built_commit() {
    if !git_available() {
        return;
    }
    let repo = linear_basic();
    let old_tip = repo.head();
    let server = GitHttpServer::spawn(repo.repo_root());
    let spec = http_spec(&server.repo_url("linear_basic"), GitAuth::Anonymous);

    let (commit_oid, pack) = build_commit_on(&old_tip, "hello from asp\n", "asp: push test", 1_700_100_000);

    let outcome = block(push_pack(&spec, "refs/heads/main", &old_tip, &commit_oid, pack))
        .expect("push should succeed as a fast-forward");
    assert!(outcome.updated);
    assert_eq!(outcome.new_oid, commit_oid);

    // The bare fixture now has our commit as the main tip.
    assert_eq!(bare_git_ok(&repo.bare, &["rev-parse", "refs/heads/main"]), commit_oid);
    let subject = bare_git_ok(&repo.bare, &["log", "-1", "--pretty=format:%s", "refs/heads/main"]);
    assert_eq!(subject, "asp: push test");
}

#[test]
fn push_pack_non_fast_forward_is_typed() {
    if !git_available() {
        return;
    }
    let repo = linear_basic();
    let old_tip = repo.head();
    let server = GitHttpServer::spawn(repo.repo_root());
    let spec = http_spec(&server.repo_url("linear_basic"), GitAuth::Anonymous);

    // First push advances main to commit A (a clean fast-forward).
    let (a_oid, a_pack) = build_commit_on(&old_tip, "branch A\n", "commit A", 1_700_100_100);
    block(push_pack(&spec, "refs/heads/main", &old_tip, &a_oid, a_pack)).expect("push A");
    assert_eq!(bare_git_ok(&repo.bare, &["rev-parse", "refs/heads/main"]), a_oid);

    // Enforce fast-forward-only on the bare (git's default is *false*), so a genuine
    // non-FF update is rejected rather than clobbering history.
    bare_git_ok(&repo.bare, &["config", "receive.denyNonFastForwards", "true"]);

    // Commit B is a *sibling* of A (also parented on old_tip). Pushing it with the
    // correct current old (A) is a genuine non-fast-forward — B is not a descendant
    // of A — so the server (denyNonFastForwards default) rejects it.
    let (b_oid, b_pack) = build_commit_on(&old_tip, "branch B\n", "commit B", 1_700_100_200);
    let err = block(push_pack(&spec, "refs/heads/main", &a_oid, &b_oid, b_pack))
        .expect_err("non-ff must be rejected");
    assert!(
        matches!(err, GitBridgeError::NonFastForward),
        "expected NonFastForward, got {err:?}"
    );
    // The ref did not move.
    assert_eq!(bare_git_ok(&repo.bare, &["rev-parse", "refs/heads/main"]), a_oid);
}

// ── RemoteStore ancestry ─────────────────────────────────────────────────────

#[test]
fn remote_store_records_and_answers_ancestry() {
    if !git_available() {
        return;
    }
    let repo = linear_basic();
    let head = repo.head();
    let root_commit = repo.commits.first().expect("at least one commit").1.clone();
    let server = GitHttpServer::spawn(repo.repo_root());
    let spec = http_spec(&server.repo_url("linear_basic"), GitAuth::Anonymous);

    let outcome = block(fetch_pack(&spec, std::slice::from_ref(&head), &[], None, |_| {})).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let mut store = RemoteStore::open(tmp.path(), "ancestry").unwrap();
    store
        .record_fetch(&outcome.pack, &[("refs/heads/main".into(), head.clone())])
        .unwrap();

    assert_eq!(store.refs().get("refs/heads/main"), Some(&head));
    let (kind, _) = store.get_object(&head).unwrap();
    assert_eq!(kind, GitObjectKind::Commit);

    assert!(store.is_ancestor(&root_commit, &head).unwrap(), "root is ancestor of tip");
    assert!(!store.is_ancestor(&head, &root_commit).unwrap(), "tip is not ancestor of root");
    assert!(store.is_ancestor(&head, &head).unwrap(), "reflexive");
}

#[test]
fn force_rewrite_breaks_ancestry() {
    if !git_available() {
        return;
    }
    let repo = linear_basic();
    let old_tip = repo.head();
    let server = GitHttpServer::spawn(repo.repo_root());
    let spec = http_spec(&server.repo_url("linear_basic"), GitAuth::Anonymous);

    // Clone the current history into the store.
    let out1 = block(fetch_pack(&spec, std::slice::from_ref(&old_tip), &[], None, |_| {})).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let mut store = RemoteStore::open(tmp.path(), "forcepush").unwrap();
    store
        .record_fetch(&out1.pack, &[("refs/heads/main".into(), old_tip.clone())])
        .unwrap();

    // Upstream rewrites its tip to a divergent (non-descendant) sha.
    let new_tip = force_rewrite_tip(&repo.bare);
    assert_ne!(new_tip, old_tip);

    // Fetch the rewritten history and record it into the same store.
    let out2 = block(fetch_pack(&spec, std::slice::from_ref(&new_tip), &[], None, |_| {})).unwrap();
    store
        .record_fetch(&out2.pack, &[("refs/heads/main".into(), new_tip.clone())])
        .unwrap();

    // Force-push detection (§4.4): the new tip is NOT a descendant of the last-ingested
    // tip, and vice-versa.
    assert!(!store.is_ancestor(&old_tip, &new_tip).unwrap());
    assert!(!store.is_ancestor(&new_tip, &old_tip).unwrap());
}

// ── SSH via a fake shim ──────────────────────────────────────────────────────

/// Write an executable POSIX-sh `ssh` shim that execs a local `git <service> <path>`
/// (inheriting `GIT_PROTOCOL=version=2` from the spawned child, so upload-pack speaks
/// protocol v2). Returns its path.
fn write_ssh_shim(dir: &Path) -> std::path::PathBuf {
    let shim = dir.join("fake-ssh");
    let script = r#"#!/bin/sh
# Fake ssh: args are `-o BatchMode=yes -o SendEnv=GIT_PROTOCOL <host> "<git-cmd> '<path>'"`.
# The remote command is the last positional argument.
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null
cmd=""
for a in "$@"; do cmd="$a"; done
case "$cmd" in
  "git-upload-pack "*)  svc="upload-pack";  rest=${cmd#git-upload-pack } ;;
  "git-receive-pack "*) svc="receive-pack"; rest=${cmd#git-receive-pack } ;;
  *) echo "fake-ssh: unrecognized remote command: $cmd" >&2; exit 1 ;;
esac
# Strip surrounding single quotes from the path.
path=$(printf '%s' "$rest" | sed "s/^'//; s/'$//")
exec git "$svc" "$path"
"#;
    std::fs::write(&shim, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    shim
}

#[test]
fn ssh_ls_remote_and_fetch_via_shim() {
    if !git_available() {
        return;
    }
    let repo = linear_basic();
    let head = repo.head();
    let expected = rev_list_object_count(&repo);

    let shim_dir = tempfile::tempdir().unwrap();
    let shim = write_ssh_shim(shim_dir.path());
    // Point the bridge at our shim for this test (process-global; only ssh tests read it).
    std::env::set_var("ASP_GIT_SSH", &shim);

    let url = GitUrl::Ssh {
        user: None,
        host: "localhost".into(),
        port: None,
        path: repo.bare.to_string_lossy().to_string(),
    };
    let spec = GitRemoteSpec { url, auth: GitAuth::SshAgent };

    // ls_remote over the pipe: real v2 advertisement + ls-refs response.
    let refs = block(ls_remote(&spec)).expect("ssh ls_remote");
    assert_eq!(refs.default_branch.as_deref(), Some("main"));
    assert_eq!(
        refs.refs.iter().find(|r| r.name == "refs/heads/main").map(|r| r.oid.as_str()),
        Some(head.as_str())
    );

    // fetch over the pipe decodes to the full object set.
    let outcome = block(fetch_pack(&spec, std::slice::from_ref(&head), &[], None, |_| {})).expect("ssh fetch");
    let tmp = tempfile::tempdir().unwrap();
    let mut store = RemoteStore::open(tmp.path(), "ssh").unwrap();
    store
        .record_fetch(&outcome.pack, &[("refs/heads/main".into(), head.clone())])
        .unwrap();
    assert_eq!(store.object_count() as usize, expected);
    assert!(store.has(&head));

    std::env::remove_var("ASP_GIT_SSH");
}

/// Real GitHub-over-SSH — opt-in (needs network + a configured key), so ignored by
/// default. Run with `cargo test -p asp-e2e --test git_transport -- --ignored`.
#[test]
#[ignore]
fn ssh_real_github_ls_remote() {
    let url = GitUrl::Ssh {
        user: Some("git".into()),
        host: "github.com".into(),
        port: None,
        path: "git/git.git".into(),
    };
    let spec = GitRemoteSpec { url, auth: GitAuth::SshAgent };
    let refs = block(ls_remote(&spec)).expect("github ssh ls_remote");
    assert!(refs.refs.iter().any(|r| r.name.starts_with("refs/heads/")));
}
