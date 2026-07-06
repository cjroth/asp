//! gitpolicy — the `interval` auto-plan rollup policy (git-bridge §5.3, M6).
//!
//! Policy lives entirely in *plan authorship*: synthesis + push (`gitpush`) stay
//! fixed and deterministic, so the `interval` policy is just "author a plan at the
//! right time, then push". Two pieces:
//!
//! * [`should_author_plan`] — a **pure, time-injected** decision function (author
//!   when there are pending rows AND either the vault has gone quiet for
//!   `quiescence` OR `window` elapsed since the last plan). Unit-tested with a
//!   truth table; no clock, no I/O.
//! * [`interval_tick`] — the driver run from the `asp watch` loop: compute pending +
//!   timing, apply the duplicate-plan guard (wait `jitter`, skip if an equal-frontier
//!   plan raced in), then `author_plan` + `push`.
//!
//! The `llm` policy is *not* here: the engine never calls a model. It is the
//! `manual` policy plus the `pending_git_diff` / `author_plan` primitives an external
//! agent drives on its own cadence (exposed as `asp git diff` / `asp git plan`).
//!
//! Native-only: it drives the on-disk [`Engine`] + the async push transport.

use crate::branch::VersionVector;
use crate::config::VaultConfig;
use crate::engine::Engine;
use crate::error::{AspError, AspResult};
use crate::gitgenesis::git_site_id;
use crate::gitpush::{author_plan, pending_git_diff, plans_in_order, push, PushReport};
use crate::log::{Kind, MAIN_BRANCH_ID};

/// The `interval` policy value stored in `git_remotes.policy`.
pub const POLICY_INTERVAL: &str = "interval";
/// The default (do-nothing-automatic) policy value.
pub const POLICY_MANUAL: &str = "manual";

/// Timing parameters for the `interval` policy, in **seconds** (git-bridge §5.3).
/// Seconds (not `Duration`) so the decision function is a plain integer comparison
/// over unix timestamps and trivially table-testable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IntervalParams {
    /// Max time between plans while pending rows exist before one is force-authored.
    pub window: i64,
    /// Quiet period after the last edit before a plan is authored.
    pub quiescence: i64,
    /// Dedup jitter waited before authoring (an equal-frontier plan cancels ours).
    pub jitter: i64,
}

impl Default for IntervalParams {
    fn default() -> Self {
        IntervalParams {
            window: crate::config::DEFAULT_GIT_INTERVAL_WINDOW_SECS,
            quiescence: crate::config::DEFAULT_GIT_INTERVAL_QUIESCENCE_SECS,
            jitter: crate::config::DEFAULT_GIT_INTERVAL_JITTER_SECS,
        }
    }
}

impl IntervalParams {
    /// Read the params from vault config, falling back to the defaults per key.
    pub fn from_config(cfg: &VaultConfig) -> AspResult<IntervalParams> {
        Ok(IntervalParams {
            window: cfg.git_interval_window_secs()?,
            quiescence: cfg.git_interval_quiescence_secs()?,
            jitter: cfg.git_interval_jitter_secs()?,
        })
    }
}

/// The outcome of an [`interval_tick`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyAction {
    /// Nothing to do (policy not `interval`, frozen, no pending rows, timing not yet
    /// reached, or the duplicate-plan guard fired).
    Skip,
    /// A plan was authored but the push produced no new commit (e.g. it only re-covered
    /// already-pushed content).
    Authored { plans: usize },
    /// A plan was authored and pushed to the remote.
    Pushed { sha: String },
}

/// Decide whether the `interval` policy should author a plan **now** (git-bridge §5.3).
/// Pure: all clock/state is injected, so it is exhaustively table-tested.
///
/// * No pending rows → never (nothing to commit).
/// * No prior plan → treat as window-exceeded (author the first plan once pending).
/// * Otherwise author iff the vault has been quiet for at least `quiescence` seconds
///   since the last edit, OR at least `window` seconds elapsed since the last plan.
pub fn should_author_plan(
    now: i64,
    last_plan_ts: Option<i64>,
    last_row_ts: Option<i64>,
    has_pending: bool,
    params: &IntervalParams,
) -> bool {
    if !has_pending {
        return false;
    }
    // Quiet long enough since the last edit? No known edit ts → treat as quiesced.
    let quiesced = match last_row_ts {
        Some(t) => now - t >= params.quiescence,
        None => true,
    };
    // Window elapsed since the last plan? No prior plan → force the first one.
    let window_elapsed = match last_plan_ts {
        Some(t) => now - t >= params.window,
        None => true,
    };
    quiesced || window_elapsed
}

/// `(last_plan_ts, last_row_ts)` for the timing decision:
/// * `last_plan_ts` = max `planned_ts` over every authored plan (the vault's most
///   recent commit boundary).
/// * `last_row_ts` = max `ts` over local (non-repo-site) content edits on `main` —
///   the vault's most recent write activity. Imported (repo-site) rows are excluded
///   so an ongoing upstream pull does not look like local activity.
fn interval_timing(engine: &Engine, root_sha: Option<&str>) -> AspResult<(Option<i64>, Option<i64>)> {
    let all = engine.store.all_rows()?;
    let repo_site = root_sha.map(git_site_id);
    let mut last_row_ts: Option<i64> = None;
    for r in &all {
        if r.branch_id != MAIN_BRANCH_ID {
            continue;
        }
        if !matches!(r.kind, Kind::Create | Kind::Edit | Kind::Delete | Kind::Rename) {
            continue;
        }
        if repo_site.as_deref() == Some(r.site_id.as_str()) {
            continue; // imported upstream row, not local activity
        }
        last_row_ts = Some(last_row_ts.map_or(r.ts, |t| t.max(r.ts)));
    }
    let last_plan_ts = plans_in_order(engine)?.iter().map(|p| p.planned_ts).max();
    Ok((last_plan_ts, last_row_ts))
}

/// One `interval`-policy pass for a remote (git-bridge §5.3), run from the `asp watch`
/// loop. `now` is injected (unix seconds) so callers/tests control the clock.
///
/// Sequence: skip unless `policy == interval` and not frozen; skip if nothing pending;
/// skip unless [`should_author_plan`]; then the **duplicate-plan guard** — sleep
/// `jitter`, and if an equal-frontier plan arrived meanwhile (another bridge authored
/// one), skip; otherwise author `"asp: N file(s) changed (paths…)"` and push.
pub async fn interval_tick(
    engine: &Engine,
    remote_id: &str,
    now: i64,
    params: IntervalParams,
) -> AspResult<PolicyAction> {
    let row = engine
        .store
        .git_remote_get(remote_id)?
        .ok_or_else(|| AspError::NotFound(format!("no git remote configured (id {remote_id})")))?;

    if row.policy != POLICY_INTERVAL {
        return Ok(PolicyAction::Skip);
    }
    if row.frozen {
        // Frozen bridge: never author while upstream history is being reconciled.
        return Ok(PolicyAction::Skip);
    }

    // Pending rows since the last plan/ingest frontier (also supplies the message).
    let pending = pending_git_diff(engine, remote_id)?;
    if pending.files_changed == 0 {
        return Ok(PolicyAction::Skip);
    }

    let (last_plan_ts, last_row_ts) = interval_timing(engine, row.root_sha.as_deref())?;
    if !should_author_plan(now, last_plan_ts, last_row_ts, true, &params) {
        return Ok(PolicyAction::Skip);
    }

    // Duplicate-plan guard (§5.3): the plan we would author covers the current main
    // frontier. Wait `jitter`; if another bridge authored an equal-frontier plan in
    // that window, stand down — its plan already commits this state.
    let target: VersionVector = engine.visible_version_vector(MAIN_BRANCH_ID)?;
    if params.jitter > 0 {
        tokio::time::sleep(std::time::Duration::from_secs(params.jitter as u64)).await;
    }
    if plans_in_order(engine)?.iter().any(|p| p.frontier == target) {
        return Ok(PolicyAction::Skip);
    }

    // Author + push. `author_plan(None)` picks up the vault-wide `git.author` config.
    let message = interval_message(&pending.files_changed, &pending.paths);
    author_plan(engine, remote_id, &message, None)?;
    match push(engine, remote_id, |_phase| {}).await? {
        PushReport::Pushed { pushed_sha, .. } => Ok(PolicyAction::Pushed { sha: pushed_sha }),
        PushReport::Nothing => Ok(PolicyAction::Authored { plans: 1 }),
    }
}

/// The auto-generated interval commit message: `"asp: N file(s) changed (a, b, …)"`
/// (git-bridge §5.3). Paths are truncated so a wide change set stays readable.
fn interval_message(files_changed: &usize, paths: &[String]) -> String {
    const MAX_PATHS: usize = 8;
    let shown: Vec<&str> = paths.iter().take(MAX_PATHS).map(String::as_str).collect();
    let mut list = shown.join(", ");
    if paths.len() > MAX_PATHS {
        list.push_str(&format!(", +{} more", paths.len() - MAX_PATHS));
    }
    format!("asp: {files_changed} file(s) changed ({list})")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> IntervalParams {
        // window 4h, quiescence 10min, no jitter (unit tests don't sleep).
        IntervalParams { window: 14_400, quiescence: 600, jitter: 0 }
    }

    #[test]
    fn should_author_truth_table() {
        let p = params();
        let now = 100_000i64;

        // No pending rows → never, regardless of timing.
        assert!(!should_author_plan(now, Some(now - 99_999), Some(now - 99_999), false, &p));
        assert!(!should_author_plan(now, None, None, false, &p));

        // Pending + quiescence exceeded (last edit 11min ago) → yes, even with a recent plan.
        assert!(should_author_plan(now, Some(now - 60), Some(now - 660), true, &p));

        // Pending + window exceeded (last plan >4h ago) → yes, even with recent activity.
        assert!(should_author_plan(now, Some(now - 20_000), Some(now - 5), true, &p));

        // Pending but recent activity within BOTH thresholds → no.
        assert!(!should_author_plan(now, Some(now - 60), Some(now - 60), true, &p));

        // Pending + no prior plan → window-exceeded once, so yes.
        assert!(should_author_plan(now, None, Some(now - 5), true, &p));

        // Pending + no known row ts → treated as quiesced → yes.
        assert!(should_author_plan(now, Some(now - 60), None, true, &p));

        // Exact boundaries are inclusive (>=).
        assert!(should_author_plan(now, Some(now - 60), Some(now - 600), true, &p)); // quiescence == 600
        assert!(should_author_plan(now, Some(now - 14_400), Some(now - 60), true, &p)); // window == 14_400
    }

    #[test]
    fn params_from_config_defaults() {
        let store = crate::sqlite::SqliteStore::open_memory().unwrap();
        let cfg = VaultConfig::new(&store);
        let p = IntervalParams::from_config(&cfg).unwrap();
        assert_eq!(p, IntervalParams::default());
        assert_eq!(p.window, 4 * 60 * 60);
        assert_eq!(p.quiescence, 10 * 60);
        assert_eq!(p.jitter, 3);

        // A config override is honored.
        store.set_config(crate::config::KEY_GIT_INTERVAL_QUIESCENCE, "42").unwrap();
        assert_eq!(IntervalParams::from_config(&cfg).unwrap().quiescence, 42);
    }

    #[test]
    fn interval_message_format_and_truncation() {
        assert_eq!(
            interval_message(&2, &["a.txt".into(), "b.txt".into()]),
            "asp: 2 file(s) changed (a.txt, b.txt)"
        );
        let many: Vec<String> = (0..10).map(|i| format!("f{i}")).collect();
        let m = interval_message(&10, &many);
        assert!(m.contains("+2 more"), "{m}");
        assert!(m.starts_with("asp: 10 file(s) changed (f0, f1"));
    }
}
