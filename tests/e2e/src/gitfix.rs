//! Hermetic git-fixture harness for the git-bridge feature (spec §10, §3).
//!
//! Three pieces, all dev-only and driving the *system* `git` binary (explicitly
//! allowed by the spec for tests):
//!
//! 1. [`FixtureRepo`] — a deterministic fixture-repo builder. Every git
//!    invocation runs with a cleared user config (`GIT_CONFIG_GLOBAL=/dev/null`,
//!    `GIT_CONFIG_SYSTEM=/dev/null`, `HOME` → a private tempdir), fixed author /
//!    committer identity, and a monotonically increasing author/committer clock
//!    (starts at 1_700_000_000 +0000, +60s per commit). Two independent builds of
//!    the same fixture therefore produce byte-identical commit SHAs — the property
//!    the importer's determinism tests lean on.
//! 2. A library of canned fixtures ([`linear_basic`], [`merged_prs`],
//!    [`criss_cross`], [`octopus`], …) covering the DAG shapes the lane-assignment
//!    importer must survive (spec §10 "DAG fidelity", risk R2).
//! 3. [`GitHttpServer`] — a tiny hyper server that CGI-execs `git http-backend`,
//!    serving any bare repo under a root at `/<name>.git`. Real `git clone`/`fetch`/
//!    `push` over smart-HTTP protocol v2 without touching the network.
//!
//! The tests live in `tests/git_harness.rs`; a `record_fixtures` binary captures
//! real wire bytes for the core team's `gitwire` parser fixtures.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use base64::Engine as _;
use http_body_util::{BodyExt, Full};
use hyper::body::{Bytes, Incoming};
use hyper::header::{AUTHORIZATION, CONTENT_TYPE};
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tempfile::TempDir;

/// First commit timestamp (2023-11-14T22:13:20Z), fixed so builds are reproducible.
const EPOCH: i64 = 1_700_000_000;
/// Seconds between successive commits.
const STEP: i64 = 60;

const AUTHOR_NAME: &str = "Fixture Author";
const AUTHOR_EMAIL: &str = "author@asp.test";
const COMMITTER_NAME: &str = "Fixture Committer";
const COMMITTER_EMAIL: &str = "committer@asp.test";

/// A git object id (40-hex SHA-1).
pub type Sha = String;

/// A deterministic, hermetic git fixture repository plus a bare mirror suitable
/// for serving over smart-HTTP.
///
/// `dir` is the working tree; `bare` is a `--bare` clone created by [`finish`]
/// (or any of the canned builders). [`repo_root`] is the parent of `bare`, which
/// is what you hand to [`GitHttpServer::spawn`] — the repo is then served at
/// `/<name>.git`.
///
/// [`finish`]: FixtureRepo::finish
/// [`repo_root`]: FixtureRepo::repo_root
pub struct FixtureRepo {
    /// Working-tree checkout.
    pub dir: PathBuf,
    /// Bare mirror (`<repo_root>/<name>.git`); empty until [`FixtureRepo::finish`].
    pub bare: PathBuf,
    /// Directory holding bare repos, i.e. the smart-HTTP `GIT_PROJECT_ROOT`.
    repo_root: PathBuf,
    /// Private `HOME` so no user `.gitconfig` leaks in.
    home: PathBuf,
    /// Fixture name (bare repo is served at `/<name>.git`).
    name: String,
    /// Monotonic author/committer clock.
    clock: i64,
    /// Keeps the whole scratch tree alive.
    _tmp: TempDir,
    /// (label, sha) of every commit-producing op, in order.
    pub commits: Vec<(String, Sha)>,
}

impl FixtureRepo {
    /// Create an empty, `git init`ed fixture named `name`.
    pub fn init(name: &str) -> FixtureRepo {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("work");
        let home = tmp.path().join("home");
        let repo_root = tmp.path().join("repos");
        for p in [&dir, &home, &repo_root] {
            std::fs::create_dir_all(p).unwrap();
        }
        let r = FixtureRepo {
            dir,
            bare: repo_root.join(format!("{name}.git")),
            repo_root,
            home,
            name: name.to_string(),
            clock: EPOCH,
            _tmp: tmp,
            commits: Vec::new(),
        };
        r.git_ok(&["init"]);
        r
    }

    /// The `GIT_PROJECT_ROOT` to hand [`GitHttpServer::spawn`]; the repo is served
    /// at `/<name>.git`.
    pub fn repo_root(&self) -> &Path {
        &self.repo_root
    }

    /// The fixture's name (its path component on the HTTP server).
    pub fn name(&self) -> &str {
        &self.name
    }

    // ---- low-level git plumbing ------------------------------------------

    fn base_cmd(&self) -> Command {
        let mut c = Command::new("git");
        c.current_dir(&self.dir)
            .env("HOME", &self.home)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_AUTHOR_NAME", AUTHOR_NAME)
            .env("GIT_AUTHOR_EMAIL", AUTHOR_EMAIL)
            .env("GIT_COMMITTER_NAME", COMMITTER_NAME)
            .env("GIT_COMMITTER_EMAIL", COMMITTER_EMAIL)
            .env("GIT_AUTHOR_DATE", format!("{} +0000", self.clock))
            .env("GIT_COMMITTER_DATE", format!("{} +0000", self.clock))
            .args([
                "-c",
                "init.defaultBranch=main",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.autocrlf=false",
                "-c",
                "gc.auto=0",
                "-c",
                "advice.detachedHead=false",
            ]);
        c
    }

    /// Run git, returning the raw [`Output`] (does not assert success).
    pub fn git(&self, args: &[&str]) -> Output {
        self.base_cmd().args(args).output().expect("spawn git")
    }

    /// Run git, asserting success; returns trimmed stdout.
    pub fn git_ok(&self, args: &[&str]) -> String {
        let out = self.git(args);
        if !out.status.success() {
            panic!(
                "git {:?} failed ({}):\nstdout: {}\nstderr: {}",
                args,
                out.status,
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr),
            );
        }
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn advance(&mut self) {
        self.clock += STEP;
    }

    // ---- working-tree operations -----------------------------------------

    /// Write (creating parents) and stage a file.
    pub fn write(&mut self, rel: &str, contents: &str) -> &mut Self {
        let p = self.dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, contents).unwrap();
        self.git_ok(&["add", "--", rel]);
        self
    }

    /// Create a directory on disk (git cannot track empty dirs; use with a
    /// subsequent [`write`](Self::write) or a placeholder).
    pub fn mkdir(&mut self, rel: &str) -> &mut Self {
        std::fs::create_dir_all(self.dir.join(rel)).unwrap();
        self
    }

    /// `git rm` a tracked path.
    pub fn rm(&mut self, rel: &str) -> &mut Self {
        self.git_ok(&["rm", "-r", "-f", "--", rel]);
        self
    }

    /// `git mv` (exact rename — same blob oid).
    pub fn mv(&mut self, from: &str, to: &str) -> &mut Self {
        if let Some(parent) = self.dir.join(to).parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        self.git_ok(&["mv", "--", from, to]);
        self
    }

    /// Mark a tracked file executable (`100755`).
    pub fn chmod_x(&mut self, rel: &str) -> &mut Self {
        self.git_ok(&["update-index", "--chmod=+x", "--", rel]);
        self
    }

    /// Create (or retarget) a symlink and stage it.
    pub fn symlink(&mut self, link: &str, target: &str) -> &mut Self {
        let p = self.dir.join(link);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let _ = std::fs::remove_file(&p);
        std::os::unix::fs::symlink(target, &p).unwrap();
        self.git_ok(&["add", "--", link]);
        self
    }

    /// Write a `.gitignore` in `dir` (use `"."` for the repo root) and stage it.
    pub fn gitignore(&mut self, dir: &str, contents: &str) -> &mut Self {
        let rel = if dir == "." || dir.is_empty() {
            ".gitignore".to_string()
        } else {
            format!("{dir}/.gitignore")
        };
        self.write(&rel, contents);
        self
    }

    /// Add a gitlink (submodule pointer) at `path` to `commit_sha` without a real
    /// submodule clone (spec §3.3 / §10 `pointers`).
    pub fn gitlink(&mut self, path: &str, commit_sha: &str) -> &mut Self {
        self.git_ok(&["update-index", "--add", "--cacheinfo", &format!("160000,{commit_sha},{path}")]);
        self
    }

    // ---- commits / branches / merges / tags ------------------------------

    /// Commit whatever is staged with `msg` (empty message allowed). Records and
    /// returns the resulting commit sha.
    pub fn commit(&mut self, msg: &str) -> Sha {
        self.git_ok(&["commit", "--allow-empty-message", "--no-verify", "-m", msg]);
        self.advance();
        let sha = self.head();
        self.commits.push((msg.to_string(), sha.clone()));
        sha
    }

    /// Convenience: write one file then commit.
    pub fn commit_file(&mut self, rel: &str, contents: &str, msg: &str) -> Sha {
        self.write(rel, contents);
        self.commit(msg)
    }

    /// The current `HEAD` commit sha.
    pub fn head(&self) -> Sha {
        self.git_ok(&["rev-parse", "HEAD"])
    }

    /// Resolve any revspec (branch, tag, sha) to a commit sha.
    pub fn rev(&self, spec: &str) -> Sha {
        self.git_ok(&["rev-parse", spec])
    }

    /// Create a branch at `HEAD` without switching.
    pub fn branch(&mut self, name: &str) -> &mut Self {
        self.git_ok(&["branch", name]);
        self
    }

    /// Switch to an existing branch.
    pub fn checkout(&mut self, name: &str) -> &mut Self {
        self.git_ok(&["checkout", name]);
        self
    }

    /// Create and switch to a new branch (optionally at `start`, default `HEAD`).
    pub fn checkout_new(&mut self, name: &str, start: Option<&str>) -> &mut Self {
        match start {
            Some(s) => self.git_ok(&["checkout", "-b", name, s]),
            None => self.git_ok(&["checkout", "-b", name]),
        };
        self
    }

    /// Delete a branch (mirrors GitHub's delete-after-merge).
    pub fn delete_branch(&mut self, name: &str) -> &mut Self {
        self.git_ok(&["branch", "-D", name]);
        self
    }

    /// Merge `other` (a branch or sha) into the current branch. `no_ff` forces a
    /// merge commit even when a fast-forward is possible. Records + returns the
    /// resulting sha.
    pub fn merge(&mut self, other: &str, msg: &str, no_ff: bool) -> Sha {
        let mut args = vec!["merge", "--no-edit"];
        if no_ff {
            args.push("--no-ff");
        }
        args.push("-m");
        args.push(msg);
        args.push(other);
        self.git_ok(&args);
        self.advance();
        let sha = self.head();
        self.commits.push((msg.to_string(), sha.clone()));
        sha
    }

    /// Octopus merge of 3+ heads into the current branch (one commit, N parents).
    pub fn merge_octopus(&mut self, others: &[&str], msg: &str) -> Sha {
        let mut args = vec!["merge", "--no-edit", "-m", msg];
        args.extend_from_slice(others);
        self.git_ok(&args);
        self.advance();
        let sha = self.head();
        self.commits.push((msg.to_string(), sha.clone()));
        sha
    }

    /// Merge an unrelated history (a second root; spec §10 `mid_history_root`).
    pub fn merge_unrelated(&mut self, other: &str, msg: &str) -> Sha {
        self.git_ok(&["merge", "--no-edit", "--no-ff", "--allow-unrelated-histories", "-m", msg, other]);
        self.advance();
        let sha = self.head();
        self.commits.push((msg.to_string(), sha.clone()));
        sha
    }

    /// Start a fresh orphan root and switch to it (used to graft a second root).
    pub fn orphan(&mut self, name: &str) -> &mut Self {
        self.git_ok(&["checkout", "--orphan", name]);
        // Clear the index/worktree carried over from the previous branch.
        let _ = self.git(&["rm", "-rf", "--cached", "."]);
        for entry in std::fs::read_dir(&self.dir).unwrap() {
            let path = entry.unwrap().path();
            if path.file_name().and_then(|n| n.to_str()) == Some(".git") {
                continue;
            }
            if path.is_dir() {
                let _ = std::fs::remove_dir_all(&path);
            } else {
                let _ = std::fs::remove_file(&path);
            }
        }
        self
    }

    /// Lightweight tag at `HEAD`.
    pub fn tag_light(&mut self, name: &str) -> &mut Self {
        self.git_ok(&["tag", name]);
        self
    }

    /// Annotated tag at `HEAD` (tagger date follows the fixture clock).
    pub fn tag_annotated(&mut self, name: &str, msg: &str) -> &mut Self {
        self.git_ok(&["tag", "-a", "-m", msg, name]);
        self.advance();
        self
    }

    // ---- inspection -------------------------------------------------------

    /// `git log --graph --pretty=format:%s` over `HEAD` — the deterministic
    /// topology snapshot (abbreviated SHAs are avoided; subjects are stable).
    pub fn graph(&self) -> String {
        self.git_ok(&["log", "--graph", "--pretty=format:%s", "HEAD"])
    }

    /// Parent shas of a commit, in order.
    pub fn parents(&self, spec: &str) -> Vec<Sha> {
        let out = self.git_ok(&["rev-list", "--parents", "-n", "1", spec]);
        out.split_whitespace().skip(1).map(|s| s.to_string()).collect()
    }

    // ---- finish -----------------------------------------------------------

    /// Create the bare mirror at [`bare`](Self::bare) and configure it for
    /// smart-HTTP fetch + push. Returns `self` for chaining.
    ///
    /// `receive.denyNonFastForwards` is left at git's default (**true**): ordinary
    /// fast-forward pushes succeed, but a history rewrite is rejected — see
    /// [`force_rewrite_tip`], which flips it deliberately for §4.4 force-push tests.
    pub fn finish(self) -> FixtureRepo {
        let bare = self.bare.to_string_lossy().to_string();
        self.git_ok(&["clone", "--bare", ".", &bare]);
        // Smart-HTTP push needs receivepack advertised on the bare side.
        self.git_ok(&["-C", &bare, "config", "http.receivepack", "true"]);
        // denyNonFastForwards intentionally left at its default (true).
        self
    }
}

// ==========================================================================
// Canned fixture library (spec §10 "DAG fidelity").
// ==========================================================================

/// (a) Linear history: adds, edits, deletes, an exact rename, a subdir, and an
/// empty-message commit.
pub fn linear_basic() -> FixtureRepo {
    let mut r = FixtureRepo::init("linear_basic");
    r.write("a.txt", "alpha\n");
    r.write("dir/b.txt", "bravo\n");
    r.commit("add a and dir/b");
    r.commit_file("a.txt", "alpha\nalpha2\n", "edit a");
    r.write("dir/c.txt", "charlie\n");
    r.rm("dir/b.txt");
    r.commit("add dir/c, delete dir/b");
    r.mv("a.txt", "a2.txt");
    r.commit("rename a -> a2");
    r.commit_file("a2.txt", "alpha\nalpha2\nalpha3\n", ""); // empty-message-safe
    r.finish()
}

/// (b) Two side branches, each merged `--no-ff` with a GitHub-style PR message and
/// deleted after merge.
pub fn merged_prs() -> FixtureRepo {
    let mut r = FixtureRepo::init("merged_prs");
    r.commit_file("README.md", "# repo\n", "initial commit");

    r.checkout_new("feature-1", None);
    r.commit_file("feature1.txt", "one\n", "feature 1 work");
    r.checkout("main");
    r.merge("feature-1", "Merge pull request #1 from owner/feature-1", true);
    r.delete_branch("feature-1");

    r.checkout_new("feature-2", None);
    r.commit_file("feature2.txt", "two\n", "feature 2 work");
    r.checkout("main");
    r.merge("feature-2", "Merge pull request #2 from owner/feature-2", true);
    r.delete_branch("feature-2");

    r.finish()
}

/// (c) Classic criss-cross: two merges (`M1`, `M2`) that each share the *same* two
/// merge bases (`A`, `B`), then both fold back into `main`.
///
/// ```text
///        A ─── M1
///       / \   /
///  C0 ─┤   \ /
///       \   X
///        \ / \
///         B ─── M2
/// ```
/// (`M1` parents = {A,B}; `M2` parents = {B,A}; main tip reaches both.)
pub fn criss_cross() -> FixtureRepo {
    let mut r = FixtureRepo::init("criss_cross");
    r.commit_file("base.txt", "base\n", "C0 base");

    r.checkout_new("branch-a", None);
    let a = r.commit_file("a.txt", "a\n", "A on branch-a");
    r.checkout("main");
    r.checkout_new("branch-b", Some("main"));
    let b = r.commit_file("b.txt", "b\n", "B on branch-b");

    // M1 on branch-a merges B (parents A, B).
    r.checkout("branch-a");
    r.merge(&b, "M1: merge branch-b into branch-a", true);
    // M2 on branch-b merges the *original* A commit (parents B, A) → criss-cross.
    r.checkout("branch-b");
    r.merge(&a, "M2: merge branch-a into branch-b", true);

    // Fold both back into main so HEAD reaches the whole DAG.
    r.checkout("main");
    r.merge("branch-a", "Merge branch 'branch-a'", true);
    r.merge("branch-b", "Merge branch 'branch-b'", true);
    r.finish()
}

/// (d) One octopus merge of three side branches (the merge commit has 4 parents).
///
/// ```text
///          x1
///         /   \
///  C0 ─┬─┼─ x2 ─ O   (O has 4 parents: main tip C1, x1, x2, x3)
///      │  \   /
///      │   x3
///      └─ C1 ──┘
/// ```
pub fn octopus() -> FixtureRepo {
    let mut r = FixtureRepo::init("octopus");
    r.commit_file("base.txt", "base\n", "C0 base");
    for (br, file) in [("oct-1", "x1.txt"), ("oct-2", "x2.txt"), ("oct-3", "x3.txt")] {
        r.checkout_new(br, Some("main"));
        r.commit_file(file, "leaf\n", &format!("{br} work"));
        r.checkout("main");
    }
    // Advance main past the merge base so the octopus can't fold a side branch
    // in as a fast-forward — a genuine 4-parent merge.
    r.commit_file("main.txt", "m\n", "main advances");
    r.merge_octopus(&["oct-1", "oct-2", "oct-3"], "Octopus merge of oct-1, oct-2, oct-3");
    r.finish()
}

/// (e) Merge `main` into a side branch, then merge that side branch back into
/// `main` (both `--no-ff`).
pub fn merge_into_side() -> FixtureRepo {
    let mut r = FixtureRepo::init("merge_into_side");
    r.commit_file("base.txt", "base\n", "C0 base");
    r.checkout_new("side", None);
    r.commit_file("side.txt", "s1\n", "side work");
    r.checkout("main");
    r.commit_file("main.txt", "c2\n", "main work");
    r.checkout("side");
    r.merge("main", "Merge branch 'main' into side", true);
    r.checkout("main");
    r.merge("side", "Merge branch 'side'", true);
    r.finish()
}

/// (f) Renames on both sides across a merge: the side branch renames `foo.txt`
/// while `main` edits it, then they merge.
pub fn renames_across_merge() -> FixtureRepo {
    let mut r = FixtureRepo::init("renames_across_merge");
    r.commit_file("foo.txt", "line1\nline2\n", "add foo");
    r.checkout_new("rename-side", None);
    r.mv("foo.txt", "bar.txt");
    r.commit("rename foo -> bar on side");
    r.checkout("main");
    r.commit_file("foo.txt", "line1\nline2\nline3\n", "edit foo on main");
    r.merge("rename-side", "Merge branch 'rename-side'", true);
    r.finish()
}

/// (g) A second root grafted mid-history via `merge --allow-unrelated-histories`.
pub fn mid_history_root() -> FixtureRepo {
    let mut r = FixtureRepo::init("mid_history_root");
    r.commit_file("main.txt", "m1\n", "main c0");
    r.commit_file("main.txt", "m1\nm2\n", "main c1");

    r.orphan("graft");
    r.commit_file("graft.txt", "g1\n", "independent root");

    r.checkout("main");
    r.merge_unrelated("graft", "Merge unrelated history 'graft'");
    r.finish()
}

/// (h) Executable bit + a symlink that is later retargeted (spec §3.3).
pub fn modes_and_symlinks() -> FixtureRepo {
    let mut r = FixtureRepo::init("modes_and_symlinks");
    r.write("script.sh", "#!/bin/sh\necho hi\n");
    r.chmod_x("script.sh");
    r.commit("add executable script");

    r.write("targetA.txt", "A\n");
    r.write("targetB.txt", "B\n");
    r.symlink("link", "targetA.txt");
    r.commit("add symlink -> targetA");

    r.symlink("link", "targetB.txt");
    r.commit("retarget symlink -> targetB");
    r.finish()
}

/// (i) Root `.gitignore` plus a nested one, both using negations.
pub fn gitignore_nested() -> FixtureRepo {
    let mut r = FixtureRepo::init("gitignore_nested");
    r.gitignore(".", "*.log\n!keep.log\nbuild/\n");
    r.write("keep.log", "kept\n");
    r.write("app.txt", "app\n");
    r.commit("root gitignore + tracked files");

    r.gitignore("sub", "*.tmp\n!important.tmp\n");
    r.write("sub/important.tmp", "kept nested\n");
    r.write("sub/code.txt", "code\n");
    r.commit("nested gitignore with negation");
    r.finish()
}

/// (j) An LFS-pointer-shaped text file plus a `.gitmodules` + gitlink entry
/// (created via `update-index --cacheinfo`, no real submodule clone; spec §3.3).
pub fn pointers() -> FixtureRepo {
    let mut r = FixtureRepo::init("pointers");
    r.write(
        "big.bin",
        "version https://git-lfs.github.com/spec/v1\n\
         oid sha256:4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393\n\
         size 12345\n",
    );
    r.write(
        ".gitmodules",
        "[submodule \"sub\"]\n\tpath = sub\n\turl = https://example.com/sub.git\n",
    );
    // A fixed, plausible gitlink target — the object need not exist for a gitlink.
    r.gitlink("sub", "a1b2c3d4e5f60718293a4b5c6d7e8f9012345678");
    r.commit("LFS pointer + gitmodules + gitlink");
    r.finish()
}

/// (k) A foxtrot merge: `main`'s first-parent chain diverts onto the feature
/// branch because a `main`-into-feature merge is fast-forwarded onto `main`.
///
/// ```text
///  C1 ─ F1 ─ Mf   (main, first-parent → F1, feature)
///   \       /
///    ─ C2 ─      (mainline C2 is only the *second* parent of Mf)
/// ```
pub fn foxtrot() -> FixtureRepo {
    let mut r = FixtureRepo::init("foxtrot");
    r.commit_file("base.txt", "c1\n", "C1 base");
    r.checkout_new("feature", None);
    r.commit_file("feature.txt", "f1\n", "F1 feature work");
    r.checkout("main");
    r.commit_file("base.txt", "c1\nc2\n", "C2 main work");
    // Merge main into feature: the merge's first parent is F1 (feature).
    r.checkout("feature");
    r.merge("main", "Merge branch 'main' into feature", true);
    // Fast-forward main onto that merge → foxtrot (main first-parent now = feature).
    r.checkout("main");
    r.merge("feature", "ff main to feature", false);
    r.finish()
}

/// A canned-fixture builder function.
pub type FixtureFn = fn() -> FixtureRepo;

/// Every canned fixture, paired with its name (for corpus/self-test loops).
pub fn all_fixtures() -> Vec<(&'static str, FixtureFn)> {
    vec![
        ("linear_basic", linear_basic as FixtureFn),
        ("merged_prs", merged_prs),
        ("criss_cross", criss_cross),
        ("octopus", octopus),
        ("merge_into_side", merge_into_side),
        ("renames_across_merge", renames_across_merge),
        ("mid_history_root", mid_history_root),
        ("modes_and_symlinks", modes_and_symlinks),
        ("gitignore_nested", gitignore_nested),
        ("pointers", pointers),
        ("foxtrot", foxtrot),
    ]
}

// ==========================================================================
// Force-push helper (spec §4.4).
// ==========================================================================

/// Rewrite the tip of `bare`'s default branch (`main`) and force-push it back,
/// simulating an upstream history rewrite. Temporarily flips
/// `receive.denyNonFastForwards` on the bare so the non-FF push is accepted, then
/// restores the default. Returns the new tip sha.
pub fn force_rewrite_tip(bare: &Path) -> Sha {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let clone = tmp.path().join("clone");
    let bare_s = bare.to_string_lossy().to_string();
    let clone_s = clone.to_string_lossy().to_string();

    let det = |cwd: &Path, args: &[&str]| -> Output {
        Command::new("git")
            .current_dir(cwd)
            .env("HOME", &home)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_AUTHOR_NAME", AUTHOR_NAME)
            .env("GIT_AUTHOR_EMAIL", AUTHOR_EMAIL)
            .env("GIT_COMMITTER_NAME", COMMITTER_NAME)
            .env("GIT_COMMITTER_EMAIL", COMMITTER_EMAIL)
            .env("GIT_AUTHOR_DATE", "1800000000 +0000")
            .env("GIT_COMMITTER_DATE", "1800000000 +0000")
            .args(args)
            .output()
            .expect("spawn git")
    };
    let ok = |cwd: &Path, args: &[&str]| {
        let out = det(cwd, args);
        assert!(out.status.success(), "git {:?}: {}", args, String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };

    ok(tmp.path(), &["clone", &bare_s, &clone_s]);
    // Amend the tip → a divergent (non-fast-forward) sha.
    std::fs::write(clone.join("REWRITTEN.txt"), "rewritten upstream\n").unwrap();
    ok(&clone, &["add", "-A"]);
    ok(&clone, &["commit", "--amend", "--no-edit", "-m", "rewritten upstream history"]);
    let new_sha = ok(&clone, &["rev-parse", "HEAD"]);

    // Allow the rewrite on the bare, push, then restore the safe default.
    ok(bare, &["config", "receive.denyNonFastForwards", "false"]);
    ok(&clone, &["push", "--force", "origin", "HEAD:main"]);
    ok(bare, &["config", "receive.denyNonFastForwards", "true"]);

    new_sha
}

/// Fast-forward `bare`'s default branch (`main`) by adding one new commit that writes
/// `path=contents`, via a throwaway clone + normal push. Simulates an ordinary
/// upstream advance for ongoing-pull tests (§4.2). Returns the new tip sha.
pub fn advance_tip(bare: &Path, path: &str, contents: &str, msg: &str) -> Sha {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let clone = tmp.path().join("clone");
    let bare_s = bare.to_string_lossy().to_string();
    let clone_s = clone.to_string_lossy().to_string();

    let det = |cwd: &Path, args: &[&str]| -> Output {
        Command::new("git")
            .current_dir(cwd)
            .env("HOME", &home)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_AUTHOR_NAME", AUTHOR_NAME)
            .env("GIT_AUTHOR_EMAIL", AUTHOR_EMAIL)
            .env("GIT_COMMITTER_NAME", COMMITTER_NAME)
            .env("GIT_COMMITTER_EMAIL", COMMITTER_EMAIL)
            .env("GIT_AUTHOR_DATE", "1810000000 +0000")
            .env("GIT_COMMITTER_DATE", "1810000000 +0000")
            .args(args)
            .output()
            .expect("spawn git")
    };
    let ok = |cwd: &Path, args: &[&str]| {
        let out = det(cwd, args);
        assert!(out.status.success(), "git {:?}: {}", args, String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };

    ok(tmp.path(), &["clone", &bare_s, &clone_s]);
    let p = clone.join(path);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, contents).unwrap();
    ok(&clone, &["add", "-A"]);
    ok(&clone, &["commit", "-m", msg]);
    let new_sha = ok(&clone, &["rev-parse", "HEAD"]);
    ok(&clone, &["push", "origin", "HEAD:main"]);
    new_sha
}

// ==========================================================================
// Smart-HTTP server (git http-backend CGI shim).
// ==========================================================================

/// A hermetic smart-HTTP git server. Serves any bare repo found under its
/// `repo_root` at `/<name>.git` by CGI-exec'ing `git http-backend`, so real
/// `git clone`/`fetch`/`ls-remote`/`push` over protocol v2 work end-to-end.
///
/// - URL shape: `{base_url}/<name>.git` (e.g. `http://127.0.0.1:PORT/linear_basic.git`).
/// - `GIT_PROTOCOL`: the client's `Git-Protocol` request header is forwarded
///   (as `HTTP_GIT_PROTOCOL`), which is **required** for protocol v2.
/// - `Content-Encoding: gzip` on POSTs is forwarded (`HTTP_CONTENT_ENCODING`);
///   http-backend inflates it.
/// - Auth: [`spawn`](Self::spawn) is anonymous; [`spawn_with_token`](Self::spawn_with_token)
///   requires `Authorization: Basic base64("x-access-token:<token>")` (any
///   username; token as password — the form GitHub uses) **or** `Bearer <token>`,
///   else `401` with `WWW-Authenticate: Basic`.
/// - Shuts the listener down on `Drop`.
pub struct GitHttpServer {
    /// Base URL, e.g. `http://127.0.0.1:PORT`.
    pub base_url: String,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl GitHttpServer {
    /// Serve every bare repo under `repo_root` anonymously.
    pub fn spawn(repo_root: &Path) -> GitHttpServer {
        Self::spawn_inner(repo_root, None)
    }

    /// Like [`spawn`](Self::spawn) but requires a bearer/basic token (see struct docs).
    pub fn spawn_with_token(repo_root: &Path, token: &str) -> GitHttpServer {
        Self::spawn_inner(repo_root, Some(token.to_string()))
    }

    fn spawn_inner(repo_root: &Path, token: Option<String>) -> GitHttpServer {
        let repo_root = repo_root.to_path_buf();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let base_url = format!("http://{addr}");
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();

        let thread = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");
            rt.block_on(async move {
                let listener = tokio::net::TcpListener::from_std(listener).expect("from_std");
                let mut shutdown = rx;
                loop {
                    tokio::select! {
                        _ = &mut shutdown => break,
                        accept = listener.accept() => {
                            let (stream, _) = match accept {
                                Ok(s) => s,
                                Err(_) => continue,
                            };
                            let io = TokioIo::new(stream);
                            let root = repo_root.clone();
                            let token = token.clone();
                            tokio::task::spawn(async move {
                                let svc = service_fn(move |req| handle(req, root.clone(), token.clone()));
                                let _ = hyper::server::conn::http1::Builder::new()
                                    .serve_connection(io, svc)
                                    .await;
                            });
                        }
                    }
                }
            });
        });

        GitHttpServer { base_url, shutdown: Some(tx), thread: Some(thread) }
    }

    /// The full clone URL for a fixture served under this server, e.g.
    /// `http://127.0.0.1:PORT/linear_basic.git`.
    pub fn repo_url(&self, name: &str) -> String {
        format!("{}/{}.git", self.base_url, name)
    }
}

impl Drop for GitHttpServer {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn authorized(headers: &hyper::HeaderMap, token: &str) -> bool {
    let Some(val) = headers.get(AUTHORIZATION).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    if let Some(b64) = val.strip_prefix("Basic ") {
        if let Ok(raw) = base64::engine::general_purpose::STANDARD.decode(b64.trim()) {
            let decoded = String::from_utf8_lossy(&raw);
            // GitHub form: any username, token as the password.
            if let Some((_user, pass)) = decoded.split_once(':') {
                return pass == token;
            }
        }
        false
    } else if let Some(bearer) = val.strip_prefix("Bearer ") {
        bearer.trim() == token
    } else {
        false
    }
}

async fn handle(
    req: Request<Incoming>,
    root: PathBuf,
    token: Option<String>,
) -> Result<Response<Full<Bytes>>, std::convert::Infallible> {
    if let Some(tok) = &token {
        if !authorized(req.headers(), tok) {
            let resp = Response::builder()
                .status(StatusCode::UNAUTHORIZED)
                .header("WWW-Authenticate", "Basic realm=\"git\"")
                .body(Full::new(Bytes::from_static(b"authentication required")))
                .unwrap();
            return Ok(resp);
        }
    }

    let method = req.method().as_str().to_string();
    let path_info = req.uri().path().to_string();
    let query = req.uri().query().unwrap_or("").to_string();
    let headers = req.headers().clone();
    let content_type = headers.get(CONTENT_TYPE).and_then(|v| v.to_str().ok()).map(String::from);
    let git_protocol = headers.get("git-protocol").and_then(|v| v.to_str().ok()).map(String::from);
    let content_encoding = headers.get("content-encoding").and_then(|v| v.to_str().ok()).map(String::from);

    let body = match req.into_body().collect().await {
        Ok(b) => b.to_bytes(),
        Err(_) => {
            return Ok(Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(Full::new(Bytes::from_static(b"bad request body")))
                .unwrap());
        }
    };

    let cgi = CgiRequest {
        root,
        method,
        path_info,
        query,
        content_type,
        git_protocol,
        content_encoding,
        body: body.to_vec(),
    };

    let out = tokio::task::spawn_blocking(move || run_http_backend(cgi)).await;
    match out {
        Ok((status, resp_headers, resp_body)) => {
            let mut builder = Response::builder().status(status);
            for (k, v) in resp_headers {
                builder = builder.header(k, v);
            }
            Ok(builder.body(Full::new(Bytes::from(resp_body))).unwrap())
        }
        Err(_) => Ok(Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .body(Full::new(Bytes::from_static(b"cgi task panicked")))
            .unwrap()),
    }
}

struct CgiRequest {
    root: PathBuf,
    method: String,
    path_info: String,
    query: String,
    content_type: Option<String>,
    git_protocol: Option<String>,
    content_encoding: Option<String>,
    body: Vec<u8>,
}

/// Locate `git-http-backend` via `git --exec-path`.
fn http_backend_path() -> PathBuf {
    let out = Command::new("git").arg("--exec-path").output().expect("git --exec-path");
    let dir = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Path::new(&dir).join("git-http-backend")
}

/// CGI-exec `git http-backend` and parse its response.
fn run_http_backend(req: CgiRequest) -> (StatusCode, Vec<(String, String)>, Vec<u8>) {
    let mut cmd = Command::new(http_backend_path());
    cmd.env("GIT_PROJECT_ROOT", &req.root)
        .env("GIT_HTTP_EXPORT_ALL", "1")
        .env("PATH_INFO", &req.path_info)
        .env("REQUEST_METHOD", &req.method)
        .env("QUERY_STRING", &req.query)
        .env("REMOTE_ADDR", "127.0.0.1")
        .env("CONTENT_LENGTH", req.body.len().to_string());
    if let Some(ct) = &req.content_type {
        cmd.env("CONTENT_TYPE", ct);
    }
    if let Some(gp) = &req.git_protocol {
        // http-backend reads HTTP_GIT_PROTOCOL and re-exports GIT_PROTOCOL to the
        // spawned upload-pack/receive-pack — required for protocol v2.
        cmd.env("HTTP_GIT_PROTOCOL", gp);
        cmd.env("GIT_PROTOCOL", gp);
    }
    if let Some(ce) = &req.content_encoding {
        cmd.env("HTTP_CONTENT_ENCODING", ce);
    }
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());

    let mut child = cmd.spawn().expect("spawn git-http-backend");
    let mut stdin = child.stdin.take().unwrap();
    let body = req.body;
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&body);
        // drop closes the pipe
    });
    let out = child.wait_with_output().expect("http-backend output");
    let _ = writer.join();

    parse_cgi(&out.stdout)
}

/// Split CGI output into (status, headers, body). Headers end at the first blank
/// line (`\r\n\r\n` or `\n\n`); a `Status:` header sets the response code.
fn parse_cgi(out: &[u8]) -> (StatusCode, Vec<(String, String)>, Vec<u8>) {
    let (head, body) = if let Some(i) = find_sub(out, b"\r\n\r\n") {
        (&out[..i], out[i + 4..].to_vec())
    } else if let Some(i) = find_sub(out, b"\n\n") {
        (&out[..i], out[i + 2..].to_vec())
    } else {
        (out, Vec::new())
    };

    let mut status = StatusCode::OK;
    let mut headers = Vec::new();
    let head_str = String::from_utf8_lossy(head);
    for line in head_str.split('\n') {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        if let Some((name, value)) = line.split_once(':') {
            let name = name.trim();
            let value = value.trim();
            if name.eq_ignore_ascii_case("Status") {
                if let Some(code) = value.split_whitespace().next() {
                    if let Ok(c) = code.parse::<u16>() {
                        status = StatusCode::from_u16(c).unwrap_or(StatusCode::OK);
                    }
                }
            } else {
                headers.push((name.to_string(), value.to_string()));
            }
        }
    }
    (status, headers, body)
}

fn find_sub(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}
