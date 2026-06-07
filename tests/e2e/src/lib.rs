//! Multi-process e2e harness (§Testing). Spawns **real `asp` processes** in
//! isolated temp dirs, each with its own `$ASP_HOME` device identity, including a
//! listening relay — exercising the spec's 100% edge-case matrix against the
//! actual binary, not an in-process shortcut. Asserts byte-identical working
//! trees, derived-git coherence, PITR, and auth behavior.

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Locate the built `asp` binary (debug preferred, else release).
pub fn asp_bin() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest.parent().unwrap().parent().unwrap();
    for profile in ["debug", "release"] {
        let p = workspace.join("target").join(profile).join("asp");
        if p.exists() {
            return p;
        }
    }
    // Build it if missing (guarded best-effort).
    let _ = Command::new(env!("CARGO")).args(["build", "-p", "asp"]).status();
    let p = workspace.join("target/debug/asp");
    assert!(p.exists(), "asp binary not found; run `cargo build -p asp` first");
    p
}

/// One isolated node: a vault dir + a private device-identity home.
pub struct Node {
    pub dir: PathBuf,
    pub home: PathBuf,
    bin: PathBuf,
    pub name: String,
}

impl Node {
    pub fn new(root: &Path, name: &str) -> Node {
        let dir = root.join(name);
        let home = root.join(format!("home-{name}"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        Node { dir, home, bin: asp_bin(), name: name.to_string() }
    }

    fn base(&self) -> Command {
        let mut c = Command::new(&self.bin);
        c.env("ASP_HOME", &self.home);
        c.env("ASP_LOG", "warn");
        c.arg("--dir").arg(&self.dir);
        c
    }

    /// Run a command, asserting success; returns stdout.
    pub fn run(&self, args: &[&str]) -> String {
        let out = self.base().args(args).output().expect("spawn asp");
        if !out.status.success() {
            panic!(
                "[{}] asp {:?} failed: {}\nstdout: {}",
                self.name,
                args,
                String::from_utf8_lossy(&out.stderr),
                String::from_utf8_lossy(&out.stdout)
            );
        }
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    /// Run a command allowing failure; returns (success, stdout, stderr).
    pub fn try_run(&self, args: &[&str]) -> (bool, String, String) {
        let out = self.base().args(args).output().expect("spawn asp");
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    }

    pub fn init(&self) -> &Self {
        self.run(&["init"]);
        self
    }

    pub fn key(&self) -> String {
        self.run(&["key"]).trim().to_string()
    }

    pub fn write(&self, rel: &str, contents: &[u8]) {
        let p = self.dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, contents).unwrap();
    }

    pub fn rename(&self, from: &str, to: &str) {
        let fp = self.dir.join(from);
        let tp = self.dir.join(to);
        std::fs::create_dir_all(tp.parent().unwrap()).unwrap();
        std::fs::rename(fp, tp).unwrap();
    }

    pub fn remove(&self, rel: &str) {
        let _ = std::fs::remove_file(self.dir.join(rel));
    }

    pub fn read(&self, rel: &str) -> Option<Vec<u8>> {
        std::fs::read(self.dir.join(rel)).ok()
    }

    pub fn read_str(&self, rel: &str) -> Option<String> {
        self.read(rel).map(|b| String::from_utf8_lossy(&b).to_string())
    }

    pub fn exists(&self, rel: &str) -> bool {
        self.dir.join(rel).exists()
    }

    pub fn commit(&self) {
        self.run(&["commit"]);
    }

    /// One-shot sync against a listening peer (optionally presenting an auth key).
    pub fn sync(&self, url: &str, auth_key: Option<&str>) {
        let mut args = vec!["sync", url];
        if let Some(k) = auth_key {
            args.push("--auth-key");
            args.push(k);
        }
        self.run(&args);
    }

    pub fn try_sync(&self, url: &str, auth_key: Option<&str>) -> (bool, String, String) {
        let mut args = vec!["sync", url];
        if let Some(k) = auth_key {
            args.push("--auth-key");
            args.push(k);
        }
        self.try_run(&args)
    }

    /// Clone-bootstrap into this node's dir from a listening peer.
    pub fn clone_from(&self, url: &str, auth_key: Option<&str>) {
        let dir = self.dir.to_string_lossy().to_string();
        let mut args = vec!["clone", url, &dir];
        if let Some(k) = auth_key {
            args.push("--auth-key");
            args.push(k);
        }
        // clone takes its own positional dir; --dir global is harmless/ignored.
        self.run(&args);
    }

    pub fn try_clone_from(&self, url: &str, auth_key: Option<&str>) -> (bool, String, String) {
        let dir = self.dir.to_string_lossy().to_string();
        let mut args = vec!["clone", url, &dir];
        if let Some(k) = auth_key {
            args.push("--auth-key");
            args.push(k);
        }
        self.try_run(&args)
    }

    pub fn snapshot(&self, name: &str) {
        self.run(&["snapshot", name]);
    }

    pub fn restore(&self, target: &str) {
        self.run(&["restore", target]);
    }

    pub fn status_json(&self) -> serde_json::Value {
        let out = self.run(&["status", "--json"]);
        serde_json::from_str(&out).expect("status json")
    }

    pub fn authorize(&self, pubkey: &str, ttl: Option<&str>) {
        let mut args = vec!["authorize", pubkey];
        if let Some(t) = ttl {
            args.push("--ttl");
            args.push(t);
        }
        self.run(&args);
    }

    /// List live (non-deleted) materialized file paths from `status`.
    pub fn rows(&self) -> u64 {
        self.status_json()["rows"].as_u64().unwrap_or(0)
    }

    pub fn head(&self) -> String {
        self.status_json()["head"].as_str().unwrap_or("").to_string()
    }
}

/// A listening relay/hub: `asp watch --listen` in the background. Killed on drop.
pub struct Hub {
    child: Child,
    pub port: u16,
    pub dir: PathBuf,
    _home: PathBuf,
    _drain: std::thread::JoinHandle<()>,
}

impl Hub {
    pub fn start(root: &Path, name: &str, auth_key: Option<&str>, extra: &[&str]) -> Hub {
        let dir = root.join(name);
        let home = root.join(format!("home-{name}"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        let mut c = Command::new(asp_bin());
        c.env("ASP_HOME", &home)
            .env("ASP_LOG", "warn")
            .arg("--dir")
            .arg(&dir)
            .args(["watch", "--listen", "--no-tls", "--port", "0"]);
        if let Some(k) = auth_key {
            c.arg("--auth-key").arg(k);
        }
        c.args(extra);
        c.stdout(Stdio::piped()).stderr(Stdio::null());
        let mut child = c.spawn().expect("spawn hub");
        let stdout = child.stdout.take().unwrap();

        // Read stdout until the listen line; keep draining afterwards.
        let (tx, rx) = mpsc::channel::<u16>();
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
                            if let Some(p) = parse_port(&line) {
                                let _ = tx.send(p);
                                sent = true;
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        let port = rx.recv_timeout(Duration::from_secs(20)).expect("hub did not announce a port");
        Hub { child, port, dir, _home: home, _drain: drain }
    }

    pub fn url(&self) -> String {
        format!("ws://127.0.0.1:{}", self.port)
    }
}

impl Drop for Hub {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn parse_port(line: &str) -> Option<u16> {
    let marker = "ws://0.0.0.0:";
    let idx = line.find(marker)? + marker.len();
    let rest = &line[idx..];
    let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// A persistent `asp watch --peer` node (realtime). Killed on drop.
pub struct Watcher {
    child: Child,
    _drain: std::thread::JoinHandle<()>,
}

impl Watcher {
    pub fn start(node: &Node, peer_url: &str, auth_key: Option<&str>, listen: bool) -> Watcher {
        let mut c = Command::new(asp_bin());
        c.env("ASP_HOME", &node.home)
            .env("ASP_LOG", "warn")
            .arg("--dir")
            .arg(&node.dir)
            .args(["watch", "--no-tls", "--debounce", "120"]);
        if listen {
            c.args(["--listen", "--port", "0"]);
        }
        if !peer_url.is_empty() {
            c.arg("--peer").arg(peer_url);
        }
        if let Some(k) = auth_key {
            c.arg("--auth-key").arg(k);
        }
        c.stdout(Stdio::piped()).stderr(Stdio::null());
        let mut child = c.spawn().expect("spawn watcher");
        let stdout = child.stdout.take().unwrap();
        let drain = std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            let mut r = stdout;
            while let Ok(n) = r.read(&mut buf) {
                if n == 0 {
                    break;
                }
            }
        });
        // Give the watcher a moment to connect + reconcile.
        std::thread::sleep(Duration::from_millis(400));
        Watcher { child, _drain: drain }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Poll `cond` until true or `timeout` elapses (for realtime/watch tests).
pub fn wait_until<F: FnMut() -> bool>(timeout: Duration, mut cond: F) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    cond()
}

/// Convenience: make a temp root dir for a test.
pub fn temp_root() -> tempfile::TempDir {
    tempfile::tempdir().expect("temp dir")
}

/// Run a one-shot `asp` command against a named vault dir (e.g. to pre-authorize
/// a hub before it starts listening). Returns (success, stdout, stderr).
pub fn admin_cmd(root: &Path, name: &str, args: &[&str]) -> (bool, String, String) {
    let dir = root.join(name);
    let home = root.join(format!("home-{name}"));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let out = Command::new(asp_bin())
        .env("ASP_HOME", &home)
        .env("ASP_LOG", "warn")
        .arg("--dir")
        .arg(&dir)
        .args(args)
        .output()
        .expect("spawn asp");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}
