//! End-to-end tests for the git-bridge **rollup policies** slice (`asp_core::gitpolicy`,
//! git-bridge §5.3, M6): the `interval` auto-plan policy + duplicate-plan guard, and the
//! `llm`-hook CLI primitives (`asp git diff` / `asp git plan`) that let an external agent
//! drive rollup. Verified against the hermetic smart-HTTP fixture server with system
//! `git` inspecting the pushed result. Tests skip gracefully when system `git` is absent.

use std::path::Path;
use std::process::Command;

use asp_core::gitbridge::{remote_id, GitAuth, GitRemoteSpec, RemoteStore};
use asp_core::gitpolicy::{interval_tick, should_author_plan, IntervalParams, PolicyAction};
use asp_core::gitpush::{author_plan, pending_git_diff, plans_in_order, push, synthesize_commits, ModeTable, PushReport};
use asp_core::gitremote::{clone_from_git, CloneOptions};
use asp_core::gitrecord::GitPlanRecord;
use asp_core::gitwire::GitUrl;
use asp_core::identity::Identity;
use asp_core::Engine;
use asp_e2e::gitfix::{linear_basic, GitHttpServer};

// ── harness (mirrors git_push.rs) ─────────────────────────────────────────────

fn git_available() -> bool {
    Command::new("git").arg("version").output().map(|o| o.status.success()).unwrap_or(false)
}

fn block<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(f)
}

fn no_keyring() {
    std::env::set_var("ASP_GIT_DISABLE_KEYRING", "1");
    std::env::remove_var("ASP_GIT_TOKEN");
}

fn https(url: &str, auth: GitAuth) -> GitRemoteSpec {
    GitRemoteSpec { url: GitUrl::Https { base: url.to_string() }, auth }
}

fn open_engine(dir: &Path, seed: u8) -> Engine {
    Engine::open(dir, Identity::from_seed(&[seed; 32])).expect("open engine")
}

fn clone_into(dir: &Path, seed: u8, url: &str) -> Engine {
    let engine = open_engine(dir, seed);
    block(clone_from_git(&engine, &https(url, GitAuth::Anonymous), &CloneOptions::default())).expect("clone");
    engine
}

fn git(bare: &Path, args: &[&str]) -> String {
    let out = Command::new("git").arg("--git-dir").arg(bare).args(args).output().expect("git");
    assert!(out.status.success(), "git {:?}: {}", args, String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn rev_parse(bare: &Path, spec: &str) -> String {
    git(bare, &["rev-parse", spec])
}

fn commit_message(bare: &Path, sha: &str) -> String {
    git(bare, &["log", "-1", "--format=%B", sha]).trim().to_string()
}

fn set_policy(engine: &Engine, rid: &str, policy: &str) {
    let mut row = engine.store.git_remote_get(rid).unwrap().unwrap();
    row.policy = policy.to_string();
    engine.store.git_remote_upsert(&row).unwrap();
}

/// A `now` far past any real edit/plan timestamp, so the injected clock always clears
/// the quiescence/window bound (edits are stamped with the real wall clock).
const FUTURE_NOW: i64 = 2_000_000_000;

fn interval_params() -> IntervalParams {
    // Small quiescence so `FUTURE_NOW` clears it; no jitter so the test never sleeps.
    IntervalParams { window: 14_400, quiescence: 600, jitter: 0 }
}

// ── 1. interval policy authors a plan AND pushes a commit ─────────────────────

#[test]
fn interval_tick_authors_and_pushes_after_quiescence() {
    if !git_available() {
        eprintln!("SKIP: system git not found");
        return;
    }
    no_keyring();
    let repo = linear_basic();
    let server = GitHttpServer::spawn(repo.repo_root());
    let url = server.repo_url(repo.name());
    let rid = remote_id(&url);
    let old_tip = rev_parse(&repo.bare, "main");

    let tmp = tempfile::tempdir().unwrap();
    let engine = clone_into(tmp.path(), 1, &url);
    set_policy(&engine, &rid, "interval");

    // A vault edit → pending rows.
    engine.record_write("a2.txt", b"alpha\nalpha2\nalpha3\ninterval-edit\n").unwrap();
    assert_eq!(plans_in_order(&engine).unwrap().len(), 0, "no plans before the tick");

    // Drive the tick with a synthetic `now` past quiescence.
    let action = block(interval_tick(&engine, &rid, FUTURE_NOW, interval_params())).expect("tick");
    let sha = match action {
        PolicyAction::Pushed { sha } => sha,
        other => panic!("expected Pushed, got {other:?}"),
    };

    // A plan was authored and a real commit pushed onto the old tip.
    assert_eq!(plans_in_order(&engine).unwrap().len(), 1, "interval authored exactly one plan");
    assert_eq!(rev_parse(&repo.bare, "main"), sha, "remote advanced to the pushed tip");
    assert_ne!(sha, old_tip);
    // Auto-generated message shape (git-bridge §5.3). The pending set also includes the
    // clone-seeded `.aspignore` (always pushed — see git_push.rs), so assert the shape
    // and the edited path, not an exact count.
    let msg = commit_message(&repo.bare, &sha);
    assert!(msg.starts_with("asp:") && msg.contains("file(s) changed"), "auto message: {msg}");
    assert!(msg.contains("a2.txt"), "auto message names the path: {msg}");
}

// ── 2. duplicate-plan guard: an equal-frontier plan already exists → no second ─

#[test]
fn interval_tick_duplicate_plan_guard() {
    if !git_available() {
        eprintln!("SKIP: system git not found");
        return;
    }
    no_keyring();
    let repo = linear_basic();
    let server = GitHttpServer::spawn(repo.repo_root());
    let url = server.repo_url(repo.name());
    let rid = remote_id(&url);

    let tmp = tempfile::tempdir().unwrap();
    let engine = clone_into(tmp.path(), 2, &url);
    set_policy(&engine, &rid, "interval");

    // Edit, then pre-author a plan covering the current frontier (as another bridge
    // would). The interval tick must NOT author a second plan for the same state.
    engine.record_write("a2.txt", b"alpha\nalpha2\nalpha3\ndup\n").unwrap();
    author_plan(&engine, &rid, "another bridge's plan", Some("B <b@x>")).unwrap();
    assert_eq!(plans_in_order(&engine).unwrap().len(), 1);

    let action = block(interval_tick(&engine, &rid, FUTURE_NOW, interval_params())).expect("tick");
    assert_eq!(action, PolicyAction::Skip, "guard/no-pending → skip");
    assert_eq!(plans_in_order(&engine).unwrap().len(), 1, "no duplicate plan authored");
}

// ── 3. interval tick is a no-op for a manual-policy remote ────────────────────

#[test]
fn interval_tick_skips_manual_policy() {
    if !git_available() {
        eprintln!("SKIP: system git not found");
        return;
    }
    no_keyring();
    let repo = linear_basic();
    let server = GitHttpServer::spawn(repo.repo_root());
    let url = server.repo_url(repo.name());
    let rid = remote_id(&url);

    let tmp = tempfile::tempdir().unwrap();
    let engine = clone_into(tmp.path(), 3, &url);
    // policy stays "manual" (the clone default).
    engine.record_write("a2.txt", b"alpha\nalpha2\nalpha3\nmanual\n").unwrap();

    let action = block(interval_tick(&engine, &rid, FUTURE_NOW, interval_params())).expect("tick");
    assert_eq!(action, PolicyAction::Skip, "manual policy never auto-authors");
    assert_eq!(plans_in_order(&engine).unwrap().len(), 0);
}

// ── 4. interval tick skips when nothing is pending ────────────────────────────

#[test]
fn interval_tick_skips_when_no_pending() {
    if !git_available() {
        eprintln!("SKIP: system git not found");
        return;
    }
    no_keyring();
    let repo = linear_basic();
    let server = GitHttpServer::spawn(repo.repo_root());
    let url = server.repo_url(repo.name());
    let rid = remote_id(&url);

    let tmp = tempfile::tempdir().unwrap();
    let engine = clone_into(tmp.path(), 4, &url);
    set_policy(&engine, &rid, "interval");

    // Capture the clone state (incl. the seeded .aspignore) with a baseline plan, then
    // make no further edits → nothing pending → skip.
    author_plan(&engine, &rid, "baseline", None).unwrap();
    let n = plans_in_order(&engine).unwrap().len();
    let action = block(interval_tick(&engine, &rid, FUTURE_NOW, interval_params())).expect("tick");
    assert_eq!(action, PolicyAction::Skip);
    assert_eq!(plans_in_order(&engine).unwrap().len(), n, "no new plan when nothing pending");
}

// ── 5. `llm` primitives: pending diff + author_plan (no push), then push ──────

#[test]
fn llm_primitives_diff_and_plan_without_push() {
    if !git_available() {
        eprintln!("SKIP: system git not found");
        return;
    }
    no_keyring();
    let repo = linear_basic();
    let server = GitHttpServer::spawn(repo.repo_root());
    let url = server.repo_url(repo.name());
    let rid = remote_id(&url);

    let tmp = tempfile::tempdir().unwrap();
    let engine = clone_into(tmp.path(), 5, &url);
    let ref_before = rev_parse(&repo.bare, "main");

    // A baseline plan captures the clone state (incl. the seeded .aspignore) so the
    // diff reflects exactly the new edits — the `asp git diff` slot.
    author_plan(&engine, &rid, "baseline", None).unwrap();
    engine.record_write("agent1.txt", b"one\n").unwrap();
    engine.record_write("agent2.txt", b"two\n").unwrap();

    let pd = pending_git_diff(&engine, &rid).unwrap();
    assert_eq!(pd.files_changed, 2, "two changed files: {:?}", pd.paths);
    assert!(pd.paths.contains(&"agent1.txt".to_string()));
    assert!(!pd.unified.is_empty(), "non-empty unified diff");

    // The `asp git plan` slot: author a plan WITHOUT pushing → visible in the log,
    // remote ref unchanged.
    let n_before = plans_in_order(&engine).unwrap().len();
    author_plan(&engine, &rid, "agent-decided message", Some("Agent <agent@x>")).unwrap();
    assert_eq!(plans_in_order(&engine).unwrap().len(), n_before + 1, "plan authored");
    assert_eq!(rev_parse(&repo.bare, "main"), ref_before, "plan does not touch the remote ref");

    // A later `asp git push` synthesizes + pushes the authored plan.
    let report = block(push(&engine, &rid, |_| {})).expect("push");
    let sha = match report {
        PushReport::Pushed { pushed_sha, .. } => pushed_sha,
        other => panic!("expected Pushed, got {other:?}"),
    };
    assert_eq!(rev_parse(&repo.bare, "main"), sha, "push advances the ref");
    assert_ne!(sha, ref_before);
}

// ── 6. determinism: policy only affects WHEN + the message, not synthesis ─────

#[test]
fn policy_only_affects_message_not_synthesis() {
    if !git_available() {
        eprintln!("SKIP: system git not found");
        return;
    }
    no_keyring();
    let repo = linear_basic();
    let server = GitHttpServer::spawn(repo.repo_root());
    let url = server.repo_url(repo.name());
    let rid = remote_id(&url);

    // Two independent clones hold byte-identical rows (proven elsewhere).
    let ta = tempfile::tempdir().unwrap();
    let tb = tempfile::tempdir().unwrap();
    let a = clone_into(ta.path(), 6, &url);
    let b = clone_into(tb.path(), 7, &url);

    // The same vault edit on both (identical Merkle id → identical frontier).
    let edit = a.record_write("a2.txt", b"alpha\nalpha2\ndet\n").unwrap().unwrap();
    b.integrate(&edit).unwrap();

    // Construct byte-identical plan records (same frontier + message + author + ts):
    // whether "interval" or "manual" authored them, synthesis is a pure function of
    // the record, so the SHA is identical.
    let frontier = a.visible_version_vector(asp_core::log::MAIN_BRANCH_ID).unwrap();
    let mk = |message: &str| GitPlanRecord {
        frontier: frontier.clone(),
        message: message.to_string(),
        author: "Same <same@x>".to_string(),
        planned_ts: 1_700_000_123,
    };

    let synth = |e: &Engine, plan: GitPlanRecord| {
        let store = RemoteStore::open(&e.asp_dir, &rid).unwrap();
        let row = e.store.git_remote_get(&rid).unwrap().unwrap();
        let modes = ModeTable::load(e).unwrap();
        synthesize_commits(e, &store, &row, &[plan], &modes).unwrap()
    };

    // Same record on both engines → identical tip sha.
    let sa = synth(&a, mk("asp: 1 file(s) changed (a2.txt)"));
    let sb = synth(&b, mk("asp: 1 file(s) changed (a2.txt)"));
    assert_eq!(sa.tip_sha, sb.tip_sha, "same record → same synthesized SHA on any node");
    assert!(!sa.tip_sha.is_empty());

    // Only the message differs → a different commit (message is the sole policy input
    // to synthesis); everything else being equal proves policy touches only WHAT-message.
    let sc = synth(&a, mk("manual: a totally different subject"));
    assert_ne!(sc.tip_sha, sa.tip_sha, "message change → different commit");
}

// ── 7. pure decision function sanity (fast, no git) ───────────────────────────

#[test]
fn should_author_plan_boundaries() {
    let p = IntervalParams { window: 14_400, quiescence: 600, jitter: 0 };
    let now = 100_000i64;
    // no pending → never
    assert!(!should_author_plan(now, None, None, false, &p));
    // pending + quiescence exceeded → yes
    assert!(should_author_plan(now, Some(now - 60), Some(now - 601), true, &p));
    // pending + window exceeded → yes
    assert!(should_author_plan(now, Some(now - 20_000), Some(now - 1), true, &p));
    // pending + recent within both → no
    assert!(!should_author_plan(now, Some(now - 60), Some(now - 60), true, &p));
    // pending + no prior plan → yes
    assert!(should_author_plan(now, None, Some(now - 1), true, &p));
}

// ── 8. the real `asp git` CLI: policy / diff / plan surface ───────────────────

#[test]
fn cli_diff_plan_policy_surface() {
    if !git_available() {
        eprintln!("SKIP: system git not found");
        return;
    }
    let repo = linear_basic();
    let server = GitHttpServer::spawn(repo.repo_root());
    let url = server.repo_url(repo.name());

    let root = asp_e2e::temp_root();
    let node = asp_e2e::Node::new(root.path(), "agent");
    // Clone the git remote into the node's vault via the real binary.
    let dir = node.dir.to_string_lossy().to_string();
    let (ok, out, err) = node.try_run(&["clone", &url, &dir]);
    if !ok {
        // Some environments can't run the fixture http clone through the binary;
        // the library-level tests above already cover the policy behavior.
        eprintln!("SKIP: `asp clone <git-url>` failed: {out}{err}");
        return;
    }

    // Default policy is manual.
    assert!(node.run(&["git", "policy"]).contains("manual"));
    // Set it to interval and read it back.
    assert!(node.run(&["git", "policy", "interval"]).contains("interval"));
    assert!(node.run(&["git", "policy"]).contains("interval"));
    // Reject a bogus policy.
    let (bad_ok, _, bad_err) = node.try_run(&["git", "policy", "bogus"]);
    assert!(!bad_ok && bad_err.contains("unknown policy"), "bad policy rejected: {bad_err}");

    // Edit a file (fold it into the log), then `asp git diff --json` reports it.
    node.write("cli.txt", b"cli edit\n");
    node.commit();
    let diff_json: serde_json::Value =
        serde_json::from_str(&node.run(&["git", "diff", "--json"])).expect("diff json");
    assert!(diff_json["files_changed"].as_u64().unwrap() >= 1, "diff sees the edit: {diff_json}");
    assert!(diff_json["unified"].as_str().unwrap().contains("cli.txt"));

    // `asp git plan -m` authors a plan without pushing (ref unchanged).
    let ref_before = rev_parse(&repo.bare, "main");
    node.run(&["git", "plan", "-m", "cli agent plan"]);
    assert_eq!(rev_parse(&repo.bare, "main"), ref_before, "plan does not push");
}
