//! Opt-in real-network integration test against fly.io. It provisions an
//! EPHEMERAL all-in-one box (`asp watch --listen --relay`), clones it from this
//! machine over the public internet (through the box's co-hosted relay), and
//! ALWAYS tears the app down again — the heavy lifting lives in
//! `scripts/fly_integration_test.sh` (guaranteed teardown via an EXIT trap).
//!
//! `#[ignore]` because it costs money, needs an authenticated `flyctl`, and
//! takes a few minutes. Run it explicitly:
//!
//!     cargo test -p asp-e2e --test fly_integration -- --ignored --nocapture
//!
//! Skips gracefully (passes) if `flyctl` is not installed, so CI without fly
//! credentials doesn't spuriously fail when someone runs the ignored set.

use std::path::PathBuf;
use std::process::Command;

fn flyctl_available() -> bool {
    Command::new("flyctl")
        .arg("version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
#[ignore = "real fly.io deploy: costs money, needs authenticated flyctl, ~minutes"]
fn fly_all_in_one_clone_over_real_network() {
    if !flyctl_available() {
        eprintln!("flyctl not found — skipping fly integration test");
        return;
    }
    // tests/e2e/tests/ -> repo root is three parents up.
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("repo root")
        .join("scripts/fly_integration_test.sh");
    assert!(script.exists(), "missing {}", script.display());

    let status = Command::new("bash")
        .arg(&script)
        .status()
        .expect("spawn fly_integration_test.sh");
    assert!(status.success(), "fly integration script failed (it tears down its app on exit regardless)");
}
