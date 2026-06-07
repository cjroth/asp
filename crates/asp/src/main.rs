//! `asp` — the Agent Sync Protocol CLI (native full node, §Surfaces). A single
//! binary exposing the full engine: init/clone/watch, key & `authorized_keys`
//! management, status, snapshot/restore (PITR), read-only derived-git inspection,
//! scope, and one-shot sync/commit. Every deployment knob has a flag, an `ASP_*`
//! env var, and (where applicable) a config key, resolved flag > env > config.

mod gitcli;
mod idstore;
mod net;

use anyhow::{anyhow, Context, Result};
use asp_core::authkeys::{expiry_from_ttl_days, format_date_ymd_utc, parse_ttl, TtlSpec};
use asp_core::config::VaultConfig;
use asp_core::Engine;
use clap::{CommandFactory, Parser, Subcommand};
use net::{AuthOpts, EngineRef};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Parser)]
#[command(name = "asp", version, about = "Agent Sync Protocol — automatic, real-time, P2P context sync")]
struct Cli {
    /// Vault directory (locates the config). Flag+env only.
    #[arg(short = 'C', long = "dir", global = true, env = "ASP_DIR")]
    dir: Option<PathBuf>,
    /// Serve plaintext ws:// instead of wss:// (behind a TLS-terminating proxy).
    #[arg(long = "no-tls", global = true, env = "ASP_NO_TLS")]
    no_tls: bool,
    /// AUTH_KEY enrollment secret(s), comma-separated (listener accepts / connector presents).
    #[arg(long = "auth-key", global = true, env = "ASP_AUTH_KEY")]
    auth_key: Option<String>,
    /// Seed the admission set with these OpenSSH key lines (newline/`;`-separated).
    #[arg(long = "authorized-keys", global = true, env = "ASP_AUTHORIZED_KEYS")]
    authorized_keys: Option<String>,
    /// Default key TTL for enrollment/migration (e.g. 90d, 1y, never).
    #[arg(long = "default-key-ttl", global = true, env = "ASP_DEFAULT_KEY_TTL")]
    default_key_ttl: Option<String>,
    /// Disable trust-on-first-use entirely (hardened/internet-exposed listeners).
    #[arg(long = "no-tofu", global = true, env = "ASP_NO_TOFU")]
    no_tofu: bool,
    /// Debounce window in milliseconds for the watcher.
    #[arg(long = "debounce", global = true, env = "ASP_DEBOUNCE")]
    debounce: Option<u64>,
    /// Log filter (e.g. info, debug).
    #[arg(long = "log", global = true, env = "ASP_LOG")]
    log: Option<String>,
    /// Opt-in debug log target (§Testing). Off by default; a side channel that
    /// never affects convergence. v1 enables verbose row-level local logging (the
    /// local source of the debug stream); shipping to a central collector URL is
    /// post-v1.
    #[arg(long = "debug", global = true, env = "ASP_DEBUG")]
    debug: Option<String>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create a new scoped vault and this node's identity.
    Init { path: Option<PathBuf> },
    /// Bootstrap a new node from a listening peer (authenticate, catch-up, pin).
    Clone { url: String, into: Option<PathBuf>, #[arg(long)] watch: bool },
    /// The primary long-running command: watch + sync. `--listen` also accepts peers.
    Watch {
        #[arg(long)]
        listen: bool,
        #[arg(long, env = "PORT")]
        port: Option<u16>,
        /// Peer URL(s) to connect to (repeatable).
        #[arg(long = "peer")]
        peers: Vec<String>,
    },
    /// One-shot: capture local changes, sync with a peer, exit.
    Sync { url: String },
    /// Capture on-disk changes into the log (no network).
    Commit,
    /// Generate / show the node's SSH public key.
    Key,
    /// Authorize a peer public key in the admission table.
    Authorize { pubkey: String, #[arg(long)] ttl: Option<String> },
    /// Revoke a peer (by OpenSSH line or hex node id).
    Revoke { pubkey: String },
    /// Inspect / extend the admission table.
    Auth {
        #[command(subcommand)]
        cmd: AuthCmd,
    },
    /// Node identity, peers, sync state, head SHA.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Pin an immutable, content-addressed snapshot.
    Snapshot { name: String },
    /// Restore the working tree to a snapshot name or a time (unix secs / YYYY-MM-DD).
    Restore { target: String },
    /// History (wraps the derived git history).
    Log {
        #[arg(long)]
        json: bool,
    },
    /// Read-only git inspection of the engine-owned repo (deny-by-default allowlist).
    Git {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Show the synced scope and ignore rules.
    Scope,
    /// Shell completions.
    Completions { shell: clap_complete::Shell },
}

#[derive(Subcommand)]
enum AuthCmd {
    List {
        #[arg(long)]
        json: bool,
    },
    Extend { peer: String, ttl: String },
}

fn now_unix() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn vault_dir(cli: &Cli) -> PathBuf {
    cli.dir.clone().unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

fn open_engine(cli: &Cli) -> Result<Engine> {
    let dir = vault_dir(cli);
    let id = idstore::load_or_generate()?;
    Engine::open(&dir, id).map_err(|e| anyhow!("opening vault at {}: {e}", dir.display()))
}

fn default_ttl_days(cli: &Cli, engine: &Engine) -> u64 {
    let spec = cli
        .default_key_ttl
        .clone()
        .or_else(|| VaultConfig::new(&engine.store).default_key_ttl().ok())
        .unwrap_or_else(|| "90d".into());
    match parse_ttl(&spec) {
        Some(TtlSpec::Days(d)) => d,
        _ => 36500, // 'never' → effectively a century
    }
}

fn auth_opts(cli: &Cli, engine: &Engine) -> AuthOpts {
    let auth_keys = cli
        .auth_key
        .clone()
        .map(|s| s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect())
        .unwrap_or_default();
    AuthOpts { auth_keys, no_tofu: cli.no_tofu, default_ttl_days: default_ttl_days(cli, engine) }
}

/// Seed the admission table from --authorized-keys / ASP_AUTHORIZED_KEYS.
fn seed_authorized_keys(cli: &Cli, engine: &Engine) -> Result<()> {
    if let Some(blob) = &cli.authorized_keys {
        for line in blob.split([';', '\n']) {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let _ = engine.authorize(line, None, false, "env");
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    // `--debug` opts into verbose row-level logging (the local source of the
    // opt-in debug log); it is a side channel that never affects convergence.
    let filter = if cli.debug.is_some() {
        "asp=debug,asp_core=debug".to_string()
    } else {
        cli.log.clone().unwrap_or_else(|| "info".into())
    };
    if let Some(target) = &cli.debug {
        eprintln!("debug log enabled (target: {target}) — side channel, off the sync path");
    }
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(filter))
        .with_target(false)
        .without_time()
        .try_init();

    if let Err(e) = run(cli).await {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<()> {
    match &cli.cmd {
        Cmd::Init { path } => {
            let dir = path.clone().or_else(|| cli.dir.clone()).unwrap_or_else(|| std::env::current_dir().unwrap());
            let id = idstore::load_or_generate()?;
            let engine = Engine::init(&dir, id).map_err(|e| anyhow!("init: {e}"))?;
            seed_authorized_keys(&cli, &engine)?;
            let vid = VaultConfig::new(&engine.store).vault_id()?.unwrap_or_default();
            println!("initialized vault at {} (vault {})", dir.display(), &vid[..8.min(vid.len())]);
            println!("device key: {}", engine.identity.to_ssh_string());
            Ok(())
        }
        Cmd::Key => {
            println!("{}", idstore::public_line()?);
            Ok(())
        }
        Cmd::Commit => {
            let engine = open_engine(&cli)?;
            let rows = engine.capture_rescan().map_err(|e| anyhow!("commit: {e}"))?;
            println!("captured {} change(s)", rows.len());
            Ok(())
        }
        Cmd::Authorize { pubkey, ttl } => {
            let engine = open_engine(&cli)?;
            let (expires_at, never) = resolve_ttl(ttl.as_deref(), &engine, &cli)?;
            let node = engine.authorize(pubkey, expires_at, never, "cli").map_err(|e| anyhow!("authorize: {e}"))?;
            println!("authorized {}", &node.to_hex()[..16]);
            Ok(())
        }
        Cmd::Revoke { pubkey } => {
            let engine = open_engine(&cli)?;
            let node_hex = node_hex_from_arg(pubkey)?;
            if engine.revoke(&node_hex).map_err(|e| anyhow!("revoke: {e}"))? {
                println!("revoked {}", &node_hex[..16]);
            } else {
                println!("no such key");
            }
            Ok(())
        }
        Cmd::Auth { cmd } => auth_cmd(&cli, cmd),
        Cmd::Status { json } => status_cmd(&cli, *json),
        Cmd::Snapshot { name } => {
            let engine = open_engine(&cli)?;
            let id = engine.snapshot(name).map_err(|e| anyhow!("snapshot: {e}"))?;
            println!("snapshot {name} ({})", &id[..8]);
            Ok(())
        }
        Cmd::Restore { target } => {
            let engine = open_engine(&cli)?;
            let rows = engine.restore(target).map_err(|e| anyhow!("restore: {e}"))?;
            println!("restored to {target} ({} change(s))", rows.len());
            Ok(())
        }
        Cmd::Log { json } => log_cmd(&cli, *json),
        Cmd::Git { args } => {
            let engine = open_engine(&cli)?;
            gitcli::run(&engine.git_dir, args)
        }
        Cmd::Scope => scope_cmd(&cli),
        Cmd::Completions { shell } => {
            let mut cmd = Cli::command();
            clap_complete::generate(*shell, &mut cmd, "asp", &mut std::io::stdout());
            Ok(())
        }
        Cmd::Sync { url } => {
            let engine = open_engine(&cli)?;
            seed_authorized_keys(&cli, &engine)?;
            let auth = auth_opts(&cli, &engine);
            let vid = VaultConfig::new(&engine.store).vault_id()?.unwrap_or_default();
            let _ = vid;
            net::sync_oneshot(Arc::new(Mutex::new(engine)), url, &auth).await
        }
        Cmd::Clone { url, into, watch } => clone_cmd(&cli, url, into.clone(), *watch).await,
        Cmd::Watch { listen, port, peers } => watch_cmd(&cli, *listen, *port, peers.clone()).await,
    }
}

fn resolve_ttl(ttl: Option<&str>, engine: &Engine, cli: &Cli) -> Result<(Option<u64>, bool)> {
    let spec = ttl.map(|s| s.to_string());
    match spec.as_deref().map(parse_ttl) {
        Some(Some(TtlSpec::Never)) => Ok((None, true)),
        Some(Some(TtlSpec::Days(d))) => Ok((Some(expiry_from_ttl_days(now_unix(), d)), false)),
        Some(None) => Err(anyhow!("bad ttl")),
        None => {
            let d = default_ttl_days(cli, engine);
            Ok((Some(expiry_from_ttl_days(now_unix(), d)), false))
        }
    }
}

fn node_hex_from_arg(arg: &str) -> Result<String> {
    if let Some(node) = asp_core::identity::parse_ssh_pubkey(arg) {
        return Ok(node.to_hex());
    }
    if asp_core::NodeId::from_hex(arg).is_some() {
        return Ok(arg.to_string());
    }
    Err(anyhow!("not an ssh-ed25519 key or hex node id"))
}

fn auth_cmd(cli: &Cli, cmd: &AuthCmd) -> Result<()> {
    let engine = open_engine(cli)?;
    match cmd {
        AuthCmd::List { json } => {
            let keys = engine.store.authkeys().map_err(|e| anyhow!("{e}"))?;
            if *json {
                let arr: Vec<_> = keys
                    .iter()
                    .map(|k| {
                        serde_json::json!({
                            "node_id": k.node_id,
                            "expires_at": k.expires_at,
                            "never": k.never,
                            "source": k.source,
                        })
                    })
                    .collect();
                println!("{}", serde_json::to_string_pretty(&arr)?);
            } else if keys.is_empty() {
                println!("(no authorized keys)");
            } else {
                for k in keys {
                    let exp = if k.never {
                        "never".to_string()
                    } else {
                        k.expires_at.map(format_date_ymd_utc).unwrap_or_else(|| "unset".into())
                    };
                    println!("{}  expires={}  ({})", &k.node_id[..16], exp, k.source);
                }
            }
            Ok(())
        }
        AuthCmd::Extend { peer, ttl } => {
            let node_hex = node_hex_from_arg(peer)?;
            let (expires_at, never) = resolve_ttl(Some(ttl), &engine, cli)?;
            if engine.store.set_authkey_expiry(&node_hex, expires_at, never).map_err(|e| anyhow!("{e}"))? {
                println!("extended {}", &node_hex[..16]);
            } else {
                println!("no such key");
            }
            Ok(())
        }
    }
}

fn status_cmd(cli: &Cli, json: bool) -> Result<()> {
    let engine = open_engine(cli)?;
    let cfg = VaultConfig::new(&engine.store);
    let vault_id = cfg.vault_id()?.unwrap_or_default();
    let rows = engine.store.row_count().map_err(|e| anyhow!("{e}"))?;
    let files = engine.store.live_files().map_err(|e| anyhow!("{e}"))?;
    let live = files.iter().filter(|f| !f.deleted).count();
    let peers = engine.store.peers().map_err(|e| anyhow!("{e}"))?;
    let head = std::fs::read_to_string(engine.git_dir.join("refs/heads/main"))
        .ok()
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if json {
        let obj = serde_json::json!({
            "node_id": engine.site_id(),
            "vault_id": vault_id,
            "rows": rows,
            "files": live,
            "peers": peers.iter().map(|(u,n)| serde_json::json!({"url":u,"node_id":n})).collect::<Vec<_>>(),
            "head": head,
            "tiebreak_key": cfg.tiebreak_key()?,
        });
        println!("{}", serde_json::to_string_pretty(&obj)?);
    } else {
        println!("node:    {}", engine.site_id());
        println!("vault:   {}", vault_id);
        println!("rows:    {rows}");
        println!("files:   {live}");
        println!("head:    {head}");
        println!("peers:   {}", peers.len());
        for (u, n) in peers {
            println!("  - {u} ({})", &n[..16.min(n.len())]);
        }
    }
    Ok(())
}

fn log_cmd(cli: &Cli, json: bool) -> Result<()> {
    let engine = open_engine(cli)?;
    if json {
        let rows = engine.store.all_rows().map_err(|e| anyhow!("{e}"))?;
        let arr: Vec<_> = rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.id, "site_id": r.site_id, "lamport": r.lamport, "seq": r.seq,
                    "kind": r.kind.as_str(), "file_id": r.file_id, "path": r.path,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&arr)?);
    } else {
        print!("{}", gitcli::log_oneline(&engine.git_dir)?);
    }
    Ok(())
}

fn scope_cmd(cli: &Cli) -> Result<()> {
    let engine = open_engine(cli)?;
    let ignore = std::fs::read_to_string(engine.root.join(".aspignore")).unwrap_or_default();
    println!("# scope root: {}", engine.root.display());
    if ignore.trim().is_empty() {
        println!("# .aspignore: (none) — .asp/ is always excluded");
    } else {
        println!("# .aspignore:");
        print!("{ignore}");
    }
    Ok(())
}

async fn clone_cmd(cli: &Cli, url: &str, into: Option<PathBuf>, watch: bool) -> Result<()> {
    let dir = into.or_else(|| cli.dir.clone()).unwrap_or_else(|| PathBuf::from("asp-vault"));
    let id = idstore::load_or_generate()?;
    let engine = Engine::open(&dir, id).map_err(|e| anyhow!("clone open: {e}"))?;
    seed_authorized_keys(cli, &engine)?;
    let auth = auth_opts(cli, &engine);
    let engine: EngineRef = Arc::new(Mutex::new(engine));
    net::clone_bootstrap(engine.clone(), url, &auth).await?;
    let vid = {
        let e = engine.lock().unwrap();
        VaultConfig::new(&e.store).vault_id()?.unwrap_or_default()
    };
    println!("cloned vault {} into {}", &vid[..8.min(vid.len())], dir.display());
    if watch {
        return run_watch_loop(cli, engine, false, None, vec![url.to_string()]).await;
    }
    Ok(())
}

async fn watch_cmd(cli: &Cli, listen: bool, port: Option<u16>, peers: Vec<String>) -> Result<()> {
    let engine = open_engine(cli)?;
    seed_authorized_keys(cli, &engine)?;
    let ttl_days = default_ttl_days(cli, &engine);
    let filled = engine.migrate_keys(ttl_days).map_err(|e| anyhow!("{e}"))?;
    if filled > 0 {
        tracing::info!(filled, "authorized_keys expiry migration");
    }
    run_watch_loop(cli, Arc::new(Mutex::new(engine)), listen, port, peers).await
}

async fn run_watch_loop(
    cli: &Cli,
    engine: EngineRef,
    listen: bool,
    port: Option<u16>,
    peers: Vec<String>,
) -> Result<()> {
    use std::collections::HashMap;
    let conns = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    let (auth, _vid, root, site, debounce) = {
        let e = engine.lock().unwrap();
        let auth = auth_opts(cli, &e);
        let vid = VaultConfig::new(&e.store).vault_id()?.unwrap_or_default();
        let debounce = cli.debounce.unwrap_or_else(|| VaultConfig::new(&e.store).debounce_ms().unwrap_or(400));
        e.capture_rescan().map_err(|err| anyhow!("reconcile: {err}"))?;
        (auth, vid, e.root.clone(), e.site_id(), debounce)
    };

    if listen {
        let bind = format!("0.0.0.0:{}", port.unwrap_or(9000));
        let (ptx, prx) = tokio::sync::oneshot::channel();
        let (e, a, c) = (engine.clone(), auth.clone(), conns.clone());
        tokio::spawn(async move {
            if let Err(e) = net::serve(e, &bind, a, c, Some(ptx)).await {
                tracing::error!("listener stopped: {e}");
            }
        });
        if let Ok(p) = prx.await {
            println!("listening on ws://0.0.0.0:{p}");
        }
    }

    for url in peers {
        let (e, a, c) = (engine.clone(), auth.clone(), conns.clone());
        tokio::spawn(async move {
            loop {
                if let Err(err) = net::connect(e.clone(), &url, &a, c.clone(), false, None).await {
                    tracing::debug!("peer {url} disconnected: {err}");
                }
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        });
    }

    let _watcher = net::spawn_watcher(engine.clone(), conns.clone(), debounce).context("watcher")?;

    println!("watching {} (node {})", root.display(), &site[..12.min(site.len())]);
    tokio::signal::ctrl_c().await.ok();
    Ok(())
}
