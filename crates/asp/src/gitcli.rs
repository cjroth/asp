//! `asp git` — **read-only** inspection of the engine-owned derived repository
//! (§Derived git history). Deny-by-default allowlist: every mutating verb is
//! refused with a pointer to the proper `asp` command, because the repo is
//! engine-owned and a write reaching it is silent corruption. Restore is
//! `asp restore`, never `git checkout`.

use anyhow::{anyhow, Result};
use std::path::Path;
use std::process::Command;

/// Verbs an agent/human may run read-only against the derived repo.
const ALLOWED: &[&str] = &[
    "log", "show", "diff", "status", "blame", "cat-file", "ls-tree", "ls-files", "rev-list",
    "rev-parse", "grep", "for-each-ref", "describe", "shortlog", "reflog",
];

pub fn run(git_dir: &Path, args: &[String]) -> Result<()> {
    let verb = args.first().map(|s| s.as_str()).unwrap_or("");
    if verb.is_empty() {
        return Err(anyhow!("usage: asp git <log|show|diff|status|...> [args]"));
    }
    if !ALLOWED.contains(&verb) {
        return Err(anyhow!(
            "`git {verb}` is refused: the derived repo is read-only and engine-owned.\n\
             Use `asp restore` to roll back, `asp snapshot` to pin a point in time."
        ));
    }
    let status = Command::new("git")
        .arg("--git-dir")
        .arg(git_dir)
        .args(args)
        .status()
        .map_err(|e| anyhow!("running git: {e}"))?;
    if !status.success() {
        return Err(anyhow!("git {verb} exited with {status}"));
    }
    Ok(())
}

/// Capture `git log` output for `asp log` (one-line history of the derived repo).
pub fn log_oneline(git_dir: &Path) -> Result<String> {
    let out = Command::new("git")
        .arg("--git-dir")
        .arg(git_dir)
        .args(["log", "--oneline", "--no-color"])
        .output()
        .map_err(|e| anyhow!("running git: {e}"))?;
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}
