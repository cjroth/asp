//! Cross-surface sync fuzzer.
//!
//! Spins up a REAL `asp watch --listen` CLI vault (the same binary a user runs),
//! connects N REAL desktop `DesktopEngine`s (the exact backend the Tauri/web app
//! drives) by cloning the CLI's ticket, then fuzzes file operations on EVERY side
//! and asserts cross-surface convergence after each round:
//!
//!   * the CLI vault dir on disk            (what the `asp` user sees)
//!   * each desktop engine dir on disk      (what the app materializes)
//!   * each desktop engine API view         (list_files/read_file — what the UI renders)
//!
//! All surfaces must agree on the live file set and byte-identical content. With
//! `--peers 2` it also exercises TRANSITIVE sync: engine A's edit fans out through
//! the CLI hub to engine B. It times per-op + convergence latency and flags perf.
//!
//! Loops until `--clean-streak` consecutive clean rounds, or `--rounds`. Exit 0 =
//! streak reached with no failures, 1 = a real bug / perf issue was found.
//!
//!   cargo run --release -p asp-desktop-engine --example sync_fuzz -- \
//!       --seed 1 --rounds 250 --peers 2 --clean-streak 10
//!
//! ASP_NO_RELAY=1 is forced (hermetic, direct/LAN dialing — no public relays).

use asp_core::Identity;
use asp_desktop_engine::DesktopEngine;
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

// ----------------------------- tiny deterministic PRNG ----------------------
struct Rng(u64);
impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(1))
    }
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % (n as u64)) as usize
    }
    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len())]
    }
}

// ----------------------------- CLI hub (real subprocess) --------------------
struct Hub {
    child: Child,
    ticket: String,
    dir: PathBuf,
    _drain: std::thread::JoinHandle<()>,
}

fn asp_bin() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest.parent().unwrap().parent().unwrap();
    for profile in ["release", "debug"] {
        let p = workspace.join("target").join(profile).join("asp");
        if p.exists() {
            return p;
        }
    }
    panic!("asp binary not found; run `cargo build --release -p asp`");
}

impl Hub {
    fn start(root: &Path, auth_key: &str, debounce_ms: u64) -> Hub {
        let dir = root.join("cli-vault");
        let home = root.join("cli-home");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(dir.join("README.md"), b"# CLI vault\n\nseeded by the cli.\n").unwrap();

        let mut c = Command::new(asp_bin());
        c.env("ASP_HOME", &home)
            .env("ASP_LOG", "warn")
            .env("ASP_NO_RELAY", "1")
            .arg("--dir")
            .arg(&dir)
            .args(["watch", "--listen", "--auth-key", auth_key, "--debounce"])
            .arg(debounce_ms.to_string());
        c.stdout(Stdio::piped()).stderr(Stdio::null());
        let mut child = c.spawn().expect("spawn cli hub");
        let stdout = child.stdout.take().unwrap();

        let (tx, rx) = mpsc::channel::<String>();
        let drain = std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            let mut sent = false;
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) => {
                        if !sent {
                            if let Some(t) = line.strip_prefix("ticket: ") {
                                let _ = tx.send(t.trim().to_string());
                                sent = true;
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        let ticket = rx
            .recv_timeout(Duration::from_secs(25))
            .expect("cli hub did not announce a ticket");
        Hub { child, ticket, dir, _drain: drain }
    }
}
impl Drop for Hub {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ----------------------------- a desktop peer -------------------------------
struct Peer {
    de: DesktopEngine,
    id: String,
    dir: PathBuf,
}

// ----------------------------- disk snapshot --------------------------------
fn snapshot_dir(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    fn walk(base: &Path, cur: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        let rd = match std::fs::read_dir(cur) {
            Ok(r) => r,
            Err(_) => return,
        };
        for ent in rd.flatten() {
            let name = ent.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') {
                continue; // .asp, .git, .aspignore
            }
            let p = ent.path();
            let ft = match ent.file_type() {
                Ok(t) => t,
                Err(_) => continue,
            };
            if ft.is_dir() {
                walk(base, &p, out);
            } else if ft.is_file() {
                let rel = p.strip_prefix(base).unwrap().to_string_lossy().to_string();
                if let Ok(b) = std::fs::read(&p) {
                    out.insert(rel, b);
                }
            }
        }
    }
    walk(dir, dir, &mut out);
    out
}

fn engine_api_snapshot(de: &DesktopEngine, id: &str) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    if let Ok(files) = de.list_files(id) {
        for f in files {
            if f.is_dir {
                continue;
            }
            if let Ok(c) = de.read_file(id, &f.path) {
                out.insert(f.path, c.into_bytes());
            }
        }
    }
    out
}

fn diff_keys(
    a: &BTreeMap<String, Vec<u8>>,
    b: &BTreeMap<String, Vec<u8>>,
    label_a: &str,
    label_b: &str,
) -> Vec<String> {
    let mut problems = Vec::new();
    for (k, va) in a {
        match b.get(k) {
            None => problems.push(format!("{label_b} MISSING {k:?} (present in {label_a})")),
            Some(vb) if vb != va => problems.push(format!(
                "CONTENT MISMATCH {k:?}: {label_a}={}B, {label_b}={}B",
                va.len(),
                vb.len()
            )),
            _ => {}
        }
    }
    for k in b.keys() {
        if !a.contains_key(k) {
            problems.push(format!("{label_a} MISSING {k:?} (present in {label_b})"));
        }
    }
    problems
}

/// The derived-git commit on `main` for a vault dir (None if not yet written).
/// Converged nodes hold the same log → same max-lamport + same tree → the SAME
/// deterministic commit SHA, so this must agree across every surface.
fn git_head(dir: &Path) -> Option<String> {
    std::fs::read_to_string(dir.join(".asp/git/refs/heads/main")).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// Cross-surface convergence: CLI disk == every engine disk == every engine API,
/// AND the derived git head agrees across all surfaces (a deterministic function
/// of the converged tree — a mismatch means a stale/incorrect git export).
fn wait_converged(hub: &Hub, peers: &[Peer], timeout: Duration) -> (bool, Vec<String>) {
    let start = Instant::now();
    let mut last = Vec::new();
    loop {
        let cli = snapshot_dir(&hub.dir);
        let cli_head = git_head(&hub.dir);
        let mut problems = Vec::new();
        for (i, p) in peers.iter().enumerate() {
            let eng = snapshot_dir(&p.dir);
            let api = engine_api_snapshot(&p.de, &p.id);
            problems.extend(diff_keys(&cli, &eng, "cli-disk", &format!("eng{i}-disk")));
            // Derived git head must match the CLI's once both have written one.
            let eng_head = git_head(&p.dir);
            if let (Some(c), Some(e)) = (&cli_head, &eng_head) {
                if c != e {
                    problems.push(format!("GIT HEAD MISMATCH eng{i}: cli={c} eng{e}", e = e));
                }
            }
            // The engine's read_file API is utf8-LOSSY by design (the editor
            // renders text), so a binary file's API view is the lossy view of
            // its bytes — not byte-identical to disk. Compare the API against the
            // engine disk passed through the SAME lossy transform: identity for
            // valid utf8 (text/code), the correct lossy view for binary. This
            // keeps the invariant honest for binary instead of excluding it.
            let eng_lossy: BTreeMap<String, Vec<u8>> = eng
                .iter()
                .map(|(k, v)| (k.clone(), String::from_utf8_lossy(v).into_owned().into_bytes()))
                .collect();
            problems.extend(diff_keys(&eng_lossy, &api, &format!("eng{i}-disk(lossy)"), &format!("eng{i}-api")));
        }
        if problems.is_empty() {
            return (true, Vec::new());
        }
        last = problems;
        if start.elapsed() >= timeout {
            return (false, last);
        }
        std::thread::sleep(Duration::from_millis(120));
    }
}

// Where an op is applied: the CLI vault (disk) or one of the N engines (by index).
#[derive(Clone, Copy, Debug)]
enum Side {
    Cli,
    Engine(usize),
}

const NAMES: &[&str] = &[
    "alpha.md", "beta.md", "notes/inbox.md", "notes/deep/nested/leaf.md",
    "docs/guide.md", "café-menü.md", "with space.md", "weird_#$.md",
    "data.txt", "code.rs",
];

fn rand_content(rng: &mut Rng, tag: &str) -> String {
    let lines = 1 + rng.below(6);
    let mut s = format!("# {tag}\n\n");
    for i in 0..lines {
        s.push_str(&format!("- line {i} :: {}\n", rng.next_u64()));
    }
    s
}

/// Non-utf8, null-containing bytes (classified `Binary`; whole-file LWW). Shaped
/// like a small binary blob: a fake header, embedded NULs, and high bytes that
/// are invalid utf8 — so it exercises the binary merge class and the engine's
/// utf8-lossy API view, not just the text path.
fn rand_binary(rng: &mut Rng, n: usize) -> Vec<u8> {
    let mut v = Vec::with_capacity(n + 8);
    v.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x00, 0x1a]);
    for _ in 0..n {
        v.push((rng.next_u64() & 0xff) as u8); // full 0..=255 range incl. NUL/high
    }
    v
}

/// A realistic code file (classified `Code` — surface-aware merge, distinct from
/// prose `Text`). Varying the body lets concurrent edits land in different code
/// regions.
fn rand_code(rng: &mut Rng, tag: &str) -> String {
    let mut s = format!("// {tag}\nuse std::collections::HashMap;\n\n");
    let fns = 1 + rng.below(4);
    for i in 0..fns {
        s.push_str(&format!(
            "pub fn f{i}(x: u64) -> u64 {{\n    let k = {};\n    x.wrapping_mul(k).wrapping_add({i})\n}}\n\n",
            rng.next_u64()
        ));
    }
    s
}

// ----------------------------- main loop ------------------------------------
fn main() {
    std::env::set_var("ASP_NO_RELAY", "1");

    let mut seed = 1u64;
    let mut rounds = 250usize;
    let mut clean_target = 10usize;
    let mut conv_timeout = Duration::from_secs(20);
    let mut debounce_ms = 60u64;
    let mut npeers = 1usize;
    let args: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--seed" => { seed = args[i + 1].parse().unwrap(); i += 2; }
            "--rounds" => { rounds = args[i + 1].parse().unwrap(); i += 2; }
            "--clean-streak" => { clean_target = args[i + 1].parse().unwrap(); i += 2; }
            "--timeout" => { conv_timeout = Duration::from_secs(args[i + 1].parse().unwrap()); i += 2; }
            "--debounce" => { debounce_ms = args[i + 1].parse().unwrap(); i += 2; }
            "--peers" => { npeers = args[i + 1].parse::<usize>().unwrap().max(1); i += 2; }
            _ => { i += 1; }
        }
    }

    let auth = "FUZZS3CRET";
    let root = tempfile::tempdir().unwrap();
    std::env::set_var("HOME", root.path());

    eprintln!("[setup] starting CLI vault (asp watch --listen, debounce={debounce_ms}ms)…");
    let hub = Hub::start(root.path(), auth, debounce_ms);
    eprintln!("[setup] CLI ticket: {}…", &hub.ticket[..hub.ticket.len().min(40)]);

    let mut peers: Vec<Peer> = Vec::new();
    for n in 0..npeers {
        // Distinct identities → distinct iroh NodeIds (engines must not self-dial).
        let mut seed_bytes = [0u8; 32];
        seed_bytes[0] = 42 + n as u8;
        let de = DesktopEngine::new(Identity::from_seed(&seed_bytes)).unwrap();
        let dir = root.path().join(format!("engine-vault-{n}"));
        std::fs::create_dir_all(&dir).unwrap();
        eprintln!("[setup] engine {n} cloning the CLI vault…");
        let v = de.clone_remote(&dir, &hub.ticket, Some(auth)).expect("engine clone");
        // Each engine also listens so the topology is a realistic mesh of full nodes.
        let _ = de.set_allow_connections(&v.id, true, Some(auth));
        peers.push(Peer { de, id: v.id, dir });
    }

    let (ok, problems) = wait_converged(&hub, &peers, conv_timeout);
    if !ok {
        eprintln!("[FAIL] initial convergence failed:\n{}", problems.join("\n"));
        std::process::exit(1);
    }
    eprintln!("[setup] initial convergence OK across {npeers} engine(s)\n");

    let mut rng = Rng::new(seed);
    let mut live: Vec<String> = vec!["README.md".into()];
    let mut streak = 0usize;
    let mut conv_latencies: Vec<u128> = Vec::new();
    let mut op_latencies: Vec<u128> = Vec::new();
    let mut max_files_seen = 0usize;
    let mut failures: Vec<String> = Vec::new();

    for round in 1..=rounds {
        let scenario = pick_scenario(&mut rng, round);
        let desc = apply_scenario(&scenario, &mut rng, &hub, &peers, &mut live, &mut op_latencies);

        let start = Instant::now();
        let (conv, problems) = wait_converged(&hub, &peers, conv_timeout);
        let latency = start.elapsed();
        let nfiles = snapshot_dir(&peers[0].dir).len();
        max_files_seen = max_files_seen.max(nfiles);

        if conv {
            conv_latencies.push(latency.as_millis());
            if latency > Duration::from_secs(10) {
                streak = 0;
                let msg = format!("round {round} [{desc}] PERF: converged but took {latency:?} (>10s) at {nfiles} files");
                eprintln!("[SLOW] {msg}");
                failures.push(msg);
            } else {
                streak += 1;
                eprintln!(
                    "round {round:>3} [{desc:<36}] OK  {:>5}ms  files={nfiles:<4} streak={streak}/{clean_target}",
                    latency.as_millis()
                );
            }
        } else {
            streak = 0;
            let msg = format!(
                "round {round} [{desc}] DIVERGED after {latency:?} ({nfiles} files):\n    {}",
                problems.join("\n    ")
            );
            eprintln!("[FAIL] {msg}\n");
            failures.push(msg);
            // Best-effort re-sync so one stuck state doesn't poison the rest.
            for p in &peers {
                let _ = p.de.sync(&p.id, &hub.ticket, Some(auth));
            }
            std::thread::sleep(Duration::from_millis(500));
        }

        if streak >= clean_target {
            eprintln!("\n=== reached {clean_target} clean rounds in a row at round {round} ===");
            break;
        }
    }

    eprintln!("\n========== SYNC FUZZ REPORT (seed={seed}, peers={npeers}) ==========");
    eprintln!("rounds run         : up to {rounds}");
    eprintln!("final clean streak : {streak}/{clean_target}");
    eprintln!("max files in vault : {max_files_seen}");
    if !conv_latencies.is_empty() {
        conv_latencies.sort_unstable();
        let p50 = conv_latencies[conv_latencies.len() / 2];
        let p95 = conv_latencies[conv_latencies.len() * 95 / 100];
        let max = *conv_latencies.last().unwrap();
        eprintln!("convergence latency: p50={p50}ms p95={p95}ms max={max}ms (n={})", conv_latencies.len());
    }
    if !op_latencies.is_empty() {
        op_latencies.sort_unstable();
        let p50 = op_latencies[op_latencies.len() / 2];
        let max = *op_latencies.last().unwrap();
        eprintln!("engine op latency  : p50={p50}ms max={max}ms (n={})", op_latencies.len());
    }
    eprintln!("failures           : {}", failures.len());
    for f in &failures {
        eprintln!("  - {f}");
    }
    eprintln!("====================================================");

    std::process::exit(if failures.is_empty() { 0 } else { 1 });
}

#[derive(Clone, Debug)]
enum Scenario {
    EditExisting,
    NewFile,
    Rename,
    Delete,
    DeleteRecreate,
    RapidBurst,
    LargeFile,
    ManyFiles,
    ConcurrentSameFile,
    EmptyFile,
    DeepNesting,
    TruncateToEmpty,
    SwapNames,
    RenameOntoExisting,
    CaseOnlyRename,
    RenameThenEdit,
    ExternalRescan,
    BinaryFile,
    HugeFile,
    CodeFile,
}

fn pick_scenario(rng: &mut Rng, round: usize) -> Scenario {
    use Scenario::*;
    let menu = if round < 4 {
        vec![NewFile, EditExisting, NewFile]
    } else {
        vec![
            EditExisting, NewFile, Rename, Delete, DeleteRecreate, RapidBurst,
            LargeFile, ManyFiles, ConcurrentSameFile, EmptyFile, DeepNesting,
            TruncateToEmpty, SwapNames, RenameOntoExisting, CaseOnlyRename, RenameThenEdit,
            ExternalRescan, BinaryFile, HugeFile, CodeFile,
        ]
    };
    rng.pick(&menu).clone()
}

fn pick_side(rng: &mut Rng, npeers: usize) -> Side {
    // 0 → CLI, 1..=npeers → engine index. Engines weighted so multi-peer
    // transitive paths get exercised.
    let n = rng.below(npeers + 1);
    if n == 0 { Side::Cli } else { Side::Engine(n - 1) }
}

/// Apply an op on a given side (CLI disk or an engine API), timing engine ops.
fn do_write(side: Side, hub: &Hub, peers: &[Peer], path: &str, content: &str, lat: &mut Vec<u128>) {
    match side {
        Side::Cli => write_cli(&hub.dir, path, content.as_bytes()),
        Side::Engine(i) => {
            let t = Instant::now();
            let _ = peers[i].de.write_file(&peers[i].id, path, content);
            lat.push(t.elapsed().as_millis());
        }
    }
}
fn do_rename(side: Side, hub: &Hub, peers: &[Peer], from: &str, to: &str, lat: &mut Vec<u128>) {
    match side {
        Side::Cli => rename_cli(&hub.dir, from, to),
        Side::Engine(i) => {
            let t = Instant::now();
            let _ = peers[i].de.rename_file(&peers[i].id, from, to);
            lat.push(t.elapsed().as_millis());
        }
    }
}
fn do_delete(side: Side, hub: &Hub, peers: &[Peer], path: &str, lat: &mut Vec<u128>) {
    match side {
        Side::Cli => delete_cli(&hub.dir, path),
        Side::Engine(i) => {
            let t = Instant::now();
            let _ = peers[i].de.delete_file(&peers[i].id, path);
            lat.push(t.elapsed().as_millis());
        }
    }
}

fn apply_scenario(
    sc: &Scenario,
    rng: &mut Rng,
    hub: &Hub,
    peers: &[Peer],
    live: &mut Vec<String>,
    lat: &mut Vec<u128>,
) -> String {
    use Scenario::*;
    let np = peers.len();

    match sc {
        EditExisting => {
            let side = pick_side(rng, np);
            let path = if live.is_empty() { "README.md".to_string() } else { live[rng.below(live.len())].clone() };
            let content = rand_content(rng, &format!("edit {path}"));
            do_write(side, hub, peers, &path, &content, lat);
            format!("edit/{side:?} {path}")
        }
        NewFile => {
            let side = pick_side(rng, np);
            let name = format!("gen/{:04}-{}", rng.next_u64() % 10000, rng.pick(NAMES));
            do_write(side, hub, peers, &name, &rand_content(rng, "new file"), lat);
            if !live.contains(&name) { live.push(name.clone()); }
            format!("new/{side:?} {name}")
        }
        Rename => {
            if live.is_empty() { return "rename(skip)".into(); }
            let from = live[rng.below(live.len())].clone();
            let to = format!("renamed/{:04}.md", rng.next_u64() % 10000);
            let side = pick_side(rng, np);
            do_rename(side, hub, peers, &from, &to, lat);
            live.retain(|p| p != &from);
            live.push(to.clone());
            format!("rename/{side:?} {from}->{to}")
        }
        Delete => {
            if live.is_empty() { return "delete(skip)".into(); }
            let idx = rng.below(live.len());
            let path = live.remove(idx);
            let side = pick_side(rng, np);
            do_delete(side, hub, peers, &path, lat);
            format!("delete/{side:?} {path}")
        }
        DeleteRecreate => {
            if live.is_empty() { return "del-recreate(skip)".into(); }
            let path = live[rng.below(live.len())].clone();
            delete_cli(&hub.dir, &path);
            do_write(Side::Engine(rng.below(np)), hub, peers, &path, &rand_content(rng, "recreated"), lat);
            format!("del+recreate {path}")
        }
        RapidBurst => {
            let path = "burst.md".to_string();
            let side = pick_side(rng, np);
            for k in 0..20 {
                do_write(side, hub, peers, &path, &format!("# burst\n\niter {k}\n{}\n", rng.next_u64()), lat);
            }
            if !live.contains(&path) { live.push(path); }
            format!("rapid-burst x20/{side:?}")
        }
        LargeFile => {
            let path = format!("large/{}.md", rng.next_u64() % 100);
            let mut big = String::with_capacity(600_000);
            big.push_str("# Large\n\n");
            for n in 0..8000 {
                big.push_str(&format!("line {n} lorem ipsum {}\n", rng.next_u64()));
            }
            let side = pick_side(rng, np);
            do_write(side, hub, peers, &path, &big, lat);
            if !live.contains(&path) { live.push(path.clone()); }
            format!("large(~{}KB)/{side:?}", big.len() / 1024)
        }
        ManyFiles => {
            let n = 30;
            let base = rng.next_u64() % 1000;
            for k in 0..n {
                let p = format!("batch/{base}/f{k:03}.md");
                let side = pick_side(rng, np);
                do_write(side, hub, peers, &p, &format!("# f{k}\n\nbatch {k}\n"), lat);
                if !live.contains(&p) { live.push(p); }
            }
            format!("many-files x{n}")
        }
        ConcurrentSameFile => {
            // Every side writes the SAME path near-simultaneously → must converge to one.
            let path = "shared/contended.md".to_string();
            write_cli(&hub.dir, &path, format!("# shared\n\nCLI {}\n", rng.next_u64()).as_bytes());
            for i in 0..np {
                let _ = peers[i].de.write_file(&peers[i].id, &path, &format!("# shared\n\nENG{i} {}\n", rng.next_u64()));
            }
            if !live.contains(&path) { live.push(path); }
            "concurrent-same-file".into()
        }
        EmptyFile => {
            let path = format!("empty/{}.md", rng.next_u64() % 100);
            do_write(pick_side(rng, np), hub, peers, &path, "", lat);
            if !live.contains(&path) { live.push(path.clone()); }
            "empty-file".into()
        }
        DeepNesting => {
            let path = "a/b/c/d/e/f/g/deep.md".to_string();
            do_write(pick_side(rng, np), hub, peers, &path, &rand_content(rng, "deep"), lat);
            if !live.contains(&path) { live.push(path); }
            "deep-nest".into()
        }
        TruncateToEmpty => {
            if live.is_empty() { return "truncate(skip)".into(); }
            let path = live[rng.below(live.len())].clone();
            do_write(pick_side(rng, np), hub, peers, &path, "", lat);
            format!("truncate {path}")
        }
        SwapNames => {
            // a→tmp, b→a, tmp→b : classic name swap that trips naive rename handling.
            if live.len() < 2 { return "swap(skip)".into(); }
            let a = live[rng.below(live.len())].clone();
            let b = live[rng.below(live.len())].clone();
            if b == a { return "swap(skip:same)".into(); }
            // Swap the two files' contents via disk renames on the CLI side. Both
            // paths still exist afterwards (contents exchanged), so `live` is unchanged.
            let tmp = "swap.tmp".to_string();
            rename_cli(&hub.dir, &a, &tmp);
            rename_cli(&hub.dir, &b, &a);
            rename_cli(&hub.dir, &tmp, &b);
            "swap-names (cli)".into()
        }
        RenameOntoExisting => {
            // Rename a file onto a path that already exists (overwrite semantics).
            if live.len() < 2 { return "rename-onto(skip)".into(); }
            let from = live[rng.below(live.len())].clone();
            let onto = live[rng.below(live.len())].clone();
            if from == onto { return "rename-onto(skip:same)".into(); }
            rename_cli(&hub.dir, &from, &onto);
            live.retain(|p| p != &from);
            format!("rename-onto-existing {from}->{onto}")
        }
        CaseOnlyRename => {
            if live.is_empty() { return "case-rename(skip)".into(); }
            let from = live[rng.below(live.len())].clone();
            let to = format!("CASE-{}.md", rng.next_u64() % 1000);
            do_rename(pick_side(rng, np), hub, peers, &from, &to, lat);
            live.retain(|p| p != &from);
            live.push(to);
            "case-rename".into()
        }
        RenameThenEdit => {
            if live.is_empty() { return "rename+edit(skip)".into(); }
            let from = live[rng.below(live.len())].clone();
            let to = format!("moved/{:04}.md", rng.next_u64() % 10000);
            let side = pick_side(rng, np);
            do_rename(side, hub, peers, &from, &to, lat);
            do_write(side, hub, peers, &to, &rand_content(rng, "moved+edited"), lat);
            live.retain(|p| p != &from);
            live.push(to.clone());
            format!("rename+edit/{side:?} ->{to}")
        }
        ExternalRescan => {
            // An edit made BEHIND an engine's API — written straight to its vault
            // dir by some external tool (editor, git pull, script) — then captured
            // via `rescan`. The desktop engine doesn't watch the filesystem, so
            // rescan is the only capture path, and it must broadcast the captured
            // rows live to peers (mirrors the CLI's auto-capture). Picks an engine
            // side (the CLI hub auto-captures via `watch`, so rescan is moot there).
            let i = rng.below(np);
            let name = format!("ext/{:04}-{}", rng.next_u64() % 10000, rng.pick(NAMES));
            let full = peers[i].dir.join(&name);
            if let Some(parent) = full.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&full, rand_content(rng, "external edit").as_bytes());
            let t = Instant::now();
            let _ = peers[i].de.rescan(&peers[i].id);
            lat.push(t.elapsed().as_millis());
            if !live.contains(&name) {
                live.push(name.clone());
            }
            format!("external-rescan/Engine({i}) {name}")
        }
        BinaryFile => {
            // A binary blob (non-utf8, embedded NULs) — classified Binary, synced
            // whole-file LWW. The engine API can't author non-utf8 (write_file is
            // &str), so binary originates on a disk side: either the CLI hub, or
            // written behind an engine + captured via rescan (engine-origin). Both
            // must converge byte-exact on every disk; the lossy API view is checked
            // against the lossy disk in wait_converged.
            let name = format!("assets/{:04}.bin", rng.next_u64() % 10000);
            let nbytes = 200 + rng.below(2000);
            let bytes = rand_binary(rng, nbytes);
            if rng.below(2) == 0 || np == 0 {
                write_cli(&hub.dir, &name, &bytes);
                if !live.contains(&name) { live.push(name.clone()); }
                format!("binary/Cli {name} ({}B)", bytes.len())
            } else {
                let i = rng.below(np);
                let full = peers[i].dir.join(&name);
                if let Some(parent) = full.parent() { let _ = std::fs::create_dir_all(parent); }
                let _ = std::fs::write(&full, &bytes);
                let t = Instant::now();
                let _ = peers[i].de.rescan(&peers[i].id);
                lat.push(t.elapsed().as_millis());
                if !live.contains(&name) { live.push(name.clone()); }
                format!("binary/Engine({i})+rescan {name} ({}B)", bytes.len())
            }
        }
        HugeFile => {
            // A large file up to ~4MB — the upper end the app must stay correct on.
            // Exercises blob storage, materialize, and convergence of multi-MB
            // content across surfaces.
            let path = format!("huge/{}.md", rng.next_u64() % 20);
            let target = 1_000_000 + rng.below(3_200_000); // ~1–4.2 MB
            let mut big = String::with_capacity(target + 64);
            big.push_str("# Huge\n\n");
            let mut n = 0u64;
            while big.len() < target {
                big.push_str(&format!("line {n} :: lorem ipsum dolor sit amet :: {}\n", rng.next_u64()));
                n += 1;
            }
            let side = pick_side(rng, np);
            do_write(side, hub, peers, &path, &big, lat);
            if !live.contains(&path) { live.push(path.clone()); }
            format!("huge(~{}MB)/{side:?}", big.len() / 1_000_000)
        }
        CodeFile => {
            // A code file (Code merge class). Two sides may edit different fns of
            // the same file → a 3-way code merge that must converge identically.
            let path = format!("src/mod_{:03}.rs", rng.next_u64() % 200);
            let side = pick_side(rng, np);
            do_write(side, hub, peers, &path, &rand_code(rng, "code file"), lat);
            if !live.contains(&path) { live.push(path.clone()); }
            // Occasionally a concurrent edit from another side for a real code merge.
            if np > 0 && rng.below(2) == 0 {
                let other = pick_side(rng, np);
                do_write(other, hub, peers, &path, &rand_code(rng, "concurrent code edit"), lat);
            }
            format!("code/{side:?} {path}")
        }
    }
}

// ----------------------------- CLI-side disk ops ----------------------------
fn write_cli(dir: &Path, rel: &str, bytes: &[u8]) {
    let p = dir.join(rel);
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(p, bytes);
}
fn rename_cli(dir: &Path, from: &str, to: &str) {
    let fp = dir.join(from);
    let tp = dir.join(to);
    if let Some(parent) = tp.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::rename(fp, tp);
}
fn delete_cli(dir: &Path, rel: &str) {
    let _ = std::fs::remove_file(dir.join(rel));
}
