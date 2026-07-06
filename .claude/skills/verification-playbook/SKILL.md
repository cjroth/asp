---
name: verification-playbook
description: How to verify work in this repo — which verification types actually earn confidence (evidence-ranked from the git-bridge build), the three high-leverage test patterns (ground-truth invariant, byte-determinism, N-vs-2N scaling ratio), and the flaky-environment protocol. Use when writing tests for a new feature, judging whether something is "tested enough", planning a test strategy, or investigating a test failure that might be environmental.
---

# Verification playbook

Ranked by how much each layer actually raised confidence during the git-bridge
build (M1–M6, ~55 e2e + 250 unit tests), with what each one caught. Spend your
effort top-down.

| Rank | Type | Value | What it caught here |
|---|---|---|---|
| 1 | Integration vs external ground truth | 90 | Import/push correctness proven against real `git` (`ls-tree`, `log`, `cat-file`) via the `gitfix` fixture server |
| 2 | Determinism / property tests | 80 | The "two independent nodes produce byte-identical state" architectural claim |
| 3 | Targeted unit tests (incl. frozen-value tripwires) | 70 | Criss-cross lane bug; UNIQUE(site_id,seq) violation; pins identity-bearing rules |
| 4 | Perf scaling-ratio tests | 60 | A real O(n²) in `build_graph` that wall-clock bounds alone would have missed |
| 5 | Cross-node race tests | 50 | Double-ingest convergence (and revealed behavior stronger than spec) |
| 6 | LCG fuzz loops | 40 | Nothing new here — but locks in "parser never panics on garbage"; keep as regression net |
| 7 | UI/TS tests (jsdom) | 35 | Routing/wiring (which API gets called), not real-browser behavior |
| 8 | Human/orchestrator review | 35 | Bugs tests CAN'T catch because the test would encode the same wrong assumption (see below) |
| 9 | Browser e2e / live soak | 10–30 | Written but environment-limited here; the only proof of "a real browser really does it" — run in CI/capable machine |

## The three high-leverage patterns (copy these)

1. **Ground-truth invariant.** When your feature mirrors an external system,
   assert equality against that system's own tooling at every step, not against
   your own expectations. Example: after replaying each git commit, the fold
   must equal `git ls-tree -r <sha>` exactly (`git_import_model.rs`,
   `git_genesis.rs`). This catches whole classes of bugs no hand-written
   expectation would.
2. **Byte-determinism across independent builds.** If two nodes must converge,
   don't test "similar" — build the same state twice through *different paths*
   (different pack layouts, independent clones, random op interleavings) and
   assert byte-identical outputs (row ids, vault_id, synthesized commit SHAs).
   See `git_convergence_prop.rs`, `git_ingest_race.rs`.
3. **N-vs-2N scaling ratio.** Wall-clock bounds are machine-dependent and
   rot. Time the op at N and 2N and assert `t(2N) < 3.0 × t(N)` (linear ≈ 2×,
   quadratic ≈ 4×; skip the assert below a ~2ms signal floor). This is what
   caught the `build_graph` O(lanes²). See `crates/asp-core/tests/branch_scale.rs`.

## What tests cannot catch — review checklist

A test written by the same mind (or agent) that wrote the code encodes the same
wrong assumption. During the git-bridge build these were only caught by review:
- **Unit/convention mismatches** (`LogRow.ts` in ms vs the codebase's seconds —
  every test passed; the timeline UI would have shown year 53000). When a value
  crosses a module boundary, grep the convention at the *other* side.
- **Policy decisions hiding as code** (`.aspignore` being pushed upstream — the
  fuzz test "fixed" it by stripping the file from comparison instead of asking
  whether it should be there). A test that works around a behavior is a flag.
When reviewing an agent's diff, look specifically for units, conventions, and
comparison-workarounds — not just logic.

## Repo idioms

- Fuzz = deterministic LCG loops inside `#[test]` (seeded, reproducible). NO
  proptest/cargo-fuzz — don't introduce them.
- Perf tests: `std::time::Instant` + generous bounds + `--nocapture` prints
  (`perf_capture.rs`, `branch_scale.rs` style). No criterion.
- E2E: real `asp` binaries (`tests/e2e/src/lib.rs`), real `git http-backend`
  (`tests/e2e/src/gitfix.rs`). Hermetic; system git is a sanctioned dev-only dep.

## Flaky-environment protocol (do this BEFORE debugging a "regression")

The networked e2e lane (iroh QUIC via relay) fails under this VM's load with no
code change at all. If a networked test fails and your change plausibly
couldn't affect it:
1. `git worktree add <scratch>/clean-main HEAD` (clean tree, no changes)
2. Build + run the same test there with a separate `CARGO_TARGET_DIR`.
3. Fails there too → pre-existing/environmental; note it and move on.
   Passes there → it's yours; debug for real.
4. Remove the worktree (and its target dir) when done.
This one step saved hours: `concurrent_merge` failed 4/4 on clean HEAD here.

## Residual-risk statement

When reporting "done", separate: (a) proven by ground-truth/determinism tests,
(b) proven by unit/UI wiring tests, (c) written-but-not-run here (browser e2e,
live soak — need CI or a machine with display/network headroom). Never let (c)
silently read as (a).
