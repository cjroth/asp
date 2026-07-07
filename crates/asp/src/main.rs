//! `asp` — the Agent Sync Protocol CLI (native full node, §Surfaces). A single
//! binary exposing the full engine: init/clone/watch, key & `authorized_keys`
//! management, status, snapshot/restore (PITR), read-only derived-git inspection,
//! scope, and one-shot sync/commit. Every deployment knob has a flag, an `ASP_*`
//! env var, and (where applicable) a config key, resolved flag > env > config.

mod gitcli;
mod idstore;
use asp_core::{iroh_net, net};

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
    /// Disable iroh relays/discovery — direct/LAN dialing only (no public relays).
    /// Useful on a trusted LAN or in hermetic tests. The env var accepts any
    /// boolish value (1/true/yes/on, 0/false/no/off).
    #[arg(
        long = "no-relay",
        global = true,
        env = "ASP_NO_RELAY",
        value_parser = clap::builder::BoolishValueParser::new(),
        num_args = 0..=1,
        default_value_t = false,
        default_missing_value = "true"
    )]
    no_relay: bool,
    /// Pin a specific iroh relay (a self-hosted `asp relay`, or a local relay in
    /// tests) instead of the public n0 relays. e.g. http://127.0.0.1:8080
    #[arg(long = "relay-url", global = true, env = "ASP_RELAY_URL")]
    relay_url: Option<String>,
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
    /// The env var accepts any boolish value (1/true/yes/on, 0/false/no/off).
    #[arg(
        long = "no-tofu",
        global = true,
        env = "ASP_NO_TOFU",
        value_parser = clap::builder::BoolishValueParser::new(),
        num_args = 0..=1,
        default_value_t = false,
        default_missing_value = "true"
    )]
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
    /// Don't use the shared device-global (home) key (`$ASP_HOME/id_ed25519`); keep
    /// this node's key self-contained in the vault at `<vault>/.asp/id_ed25519`.
    /// Minted on first use by any command that opens the vault (init/clone/watch/
    /// sync/…); afterwards it's detected automatically by presence. Lets several
    /// nodes run on one machine with distinct identities. The env var accepts any
    /// boolish value (1/true/yes/on, 0/false/no/off).
    #[arg(
        long = "no-home-key",
        global = true,
        env = "ASP_NO_HOME_KEY",
        value_parser = clap::builder::BoolishValueParser::new(),
        num_args = 0..=1,
        default_value_t = false,
        default_missing_value = "true"
    )]
    no_home_key: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create a new scoped vault and this node's identity.
    Init {
        path: Option<PathBuf>,
        /// Create a VERIFIED vault (scoped-sync §4.4): every mutating row is signed
        /// by its author and each node rejects unsigned/unauthorized rows, so
        /// read-only + subdir-read hold in a true P2P mesh (not just a star). The
        /// mode is genesis-set and inherited by every clone; it cannot be downgraded.
        #[arg(long)]
        verified: bool,
    },
    /// Bootstrap a new node from a listening peer OR clone a git remote.
    /// `<source>` is auto-detected: an iroh ticket / 64-hex node id, or a git URL
    /// (`https://…`, `ssh://…`, `git@host:path`, or a path ending `.git`).
    Clone {
        peer: String,
        into: Option<PathBuf>,
        #[arg(long)]
        watch: bool,
        /// git only: import only the last N first-parent commits (+ merged side
        /// ancestry), fronted by a snapshot (git-bridge §3.4).
        #[arg(long)]
        depth: Option<u32>,
        /// git only: clone into a fresh random vault id (two separate vaults).
        #[arg(long = "new-identity")]
        new_identity: bool,
        /// git only: also import every open (unmerged) branch as a live ASP branch
        /// (a snapshot of the open branches; git-open-branches §1/§5).
        #[arg(long = "all-branches")]
        all_branches: bool,
        /// git only: HTTPS token (PAT). Also read from `ASP_GIT_TOKEN`.
        #[arg(long, env = "ASP_GIT_TOKEN")]
        token: Option<String>,
        /// Clone only a subdirectory (scoped-sync §3): a pull-only PARTIAL replica
        /// that receives just this path prefix and hides everything else. Repeatable.
        /// The source must have granted this key the same subdir (`asp authorize
        /// <key> --subdir PATH`). A partial replica refuses to `--listen`, to add a
        /// second upstream, and to push to a git remote. NOTE: widening the scope
        /// later requires a fresh re-clone (the version vector can't backfill).
        #[arg(long = "subdir")]
        subdir: Vec<String>,
    },
    /// The primary long-running command: watch + sync. `--listen` also accepts peers.
    Watch {
        #[arg(long)]
        listen: bool,
        /// Co-host an iroh relay in this same process and pin it as this node's
        /// home relay, so the ticket it prints routes peers through it. Combine
        /// with `--listen` for an all-in-one box that serves its own vault AND
        /// relays (no separate relay box). Advertise its public URL with
        /// `--relay-url` (e.g. https://my-box.fly.dev); without one it advertises
        /// the local bind (LAN/loopback only).
        #[arg(long)]
        relay: bool,
        /// Bind address for the co-hosted relay (with `--relay`). Default 0.0.0.0:8080.
        #[arg(long = "relay-listen-addr")]
        relay_listen_addr: Option<String>,
        /// Peer(s) to connect to — iroh ticket or node id (repeatable).
        #[arg(long = "peer")]
        peers: Vec<String>,
    },
    /// One-shot: capture local changes, sync with a peer, exit. With no peer, uses
    /// the saved peer (from `clone`). `peer` is an iroh ticket or node id.
    Sync { peer: Option<String> },
    /// Run a pure iroh relay (forwards encrypted packets, stores/sees nothing).
    Relay {
        /// HTTP bind address for the relay (default 0.0.0.0:8080).
        #[arg(long = "listen-addr")]
        bind: Option<String>,
        /// Also run the git CORS proxy (lets browser clones reach git hosts). Unlike
        /// relayed ASP traffic, git payloads are TLS-terminated here (plaintext).
        #[arg(long = "git-proxy")]
        git_proxy: bool,
        /// Bind address for the git CORS proxy (default 0.0.0.0:8081). Separate
        /// listener from the iroh relay's own port.
        #[arg(long = "git-proxy-addr")]
        git_proxy_addr: Option<String>,
        /// Restrict the git proxy to these exact upstream hosts (repeatable). Empty =
        /// any host (still SSRF-vetted).
        #[arg(long = "git-proxy-allow")]
        git_proxy_allow: Vec<String>,
    },
    /// Print this node's connection ticket (and node id) as text + a QR code.
    Ticket,
    /// Capture on-disk changes into the log (no network).
    Commit,
    /// Generate / show the node's SSH public key.
    Key,
    /// Authorize a peer public key in the admission table. Scoped-sync grants
    /// (scoped-sync §3.7): `--subdir PATH` (repeatable) limits what the peer
    /// receives to those path prefixes; `--read-only` forbids it pushing changes.
    Authorize {
        pubkey: String,
        #[arg(long)]
        ttl: Option<String>,
        /// Restrict this peer to a subdirectory (path prefix). Repeatable; omit for
        /// the whole vault. A peer granted `--subdir` receives only in-scope files.
        #[arg(long = "subdir")]
        subdir: Vec<String>,
        /// Forbid this peer from pushing file changes to us (one-way / pull-only).
        #[arg(long = "read-only")]
        read_only: bool,
    },
    /// Revoke a peer (by OpenSSH line or hex node id).
    Revoke { pubkey: String },
    /// Inspect / extend the admission table.
    Auth {
        #[command(subcommand)]
        cmd: AuthCmd,
    },
    /// Branches: scoped views over the shared log (list / create / checkout /
    /// delete). Branch records sync to every peer like content does.
    Branch {
        #[command(subcommand)]
        cmd: BranchCmd,
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

#[derive(Subcommand)]
enum BranchCmd {
    /// List branches; the checked-out one is marked `*`.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Create a branch from HEAD at the current point (does not switch to it).
    Create {
        name: String,
        /// Also check out the new branch.
        #[arg(long)]
        checkout: bool,
    },
    /// Switch HEAD to a branch (by id or name) and re-materialize its state.
    Checkout { branch: String },
    /// Soft-delete a branch (by id or name); `main` cannot be deleted.
    Delete { branch: String },
}

fn now_unix() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Whether a listener should force trust-on-first-use OFF. A node that both
/// serves (`listen`) and is publicly reachable (`public`: relays/discovery on)
/// must not silently TOFU-enroll the first stranger to dial it — admission then
/// requires an auth key or a pre-authorized key. A non-listener, or a
/// hermetic/LAN listener (`--no-relay`), is left as-is so easy pairing still
/// works on a trusted network.
fn listener_hardens_tofu(listen: bool, public: bool) -> bool {
    listen && public
}

/// Ask a yes/no question on an interactive terminal. `y`/`yes` → true; any other
/// key → false. Non-interactive (piped/CI) → false, so a supplied `--peer` never
/// silently rewrites saved config.
fn prompt_yes(question: &str) -> bool {
    use std::io::{IsTerminal, Write};
    if !std::io::stdin().is_terminal() {
        return false;
    }
    print!("{question} [y/N] ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// The saved peer URLs for this vault (git's `origin` — local `peers` table).
fn saved_peer_urls(engine: &Engine) -> Vec<String> {
    engine.store.peers().map(|ps| ps.into_iter().map(|(u, _)| u).collect()).unwrap_or_default()
}

/// Resolve the peer URLs for `watch`/`sync`. With no `--peer`, use the saved
/// peers. With explicit URLs, use them — and offer to save any new one (clone
/// saves automatically; an explicit URL only persists with consent).
fn resolve_peers(engine: &Engine, supplied: &[String]) -> Vec<String> {
    let saved = saved_peer_urls(engine);
    if supplied.is_empty() {
        return saved;
    }
    for url in supplied {
        if !saved.contains(url) && prompt_yes(&format!("Save {url} as a peer for this vault?")) {
            let _ = engine.store.add_peer(url, "", now_unix());
            println!("saved peer {url}");
        }
    }
    supplied.to_vec()
}

fn vault_dir(cli: &Cli) -> PathBuf {
    cli.dir.clone().unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

fn open_engine(cli: &Cli) -> Result<Engine> {
    let dir = vault_dir(cli);
    // Honor --no-home-key on any command that opens a vault (watch/sync/commit/…),
    // not just init/clone: a vault-local key is minted on first use and detected by
    // presence thereafter.
    let id = idstore::load_or_generate(&dir, cli.no_home_key)?;
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
        Cmd::Init { path, verified } => {
            let dir = path.clone().or_else(|| cli.dir.clone()).unwrap_or_else(|| std::env::current_dir().unwrap());
            let id = idstore::load_or_generate(&dir, cli.no_home_key)?;
            let engine = Engine::init(&dir, id).map_err(|e| anyhow!("init: {e}"))?;
            // Verified-at-genesis (epoch 0): every mutating row must be signed.
            if *verified {
                engine.set_verified(0).map_err(|e| anyhow!("enable verified: {e}"))?;
            }
            seed_authorized_keys(&cli, &engine)?;
            let vid = VaultConfig::new(&engine.store).vault_id()?.unwrap_or_default();
            let mode = if *verified { " [verified]" } else { "" };
            println!("initialized vault at {} (vault {}){}", dir.display(), &vid[..8.min(vid.len())], mode);
            println!("device key: {}", engine.identity.to_ssh_string());
            Ok(())
        }
        Cmd::Key => {
            println!("{}", idstore::public_line(&vault_dir(&cli), cli.no_home_key)?);
            Ok(())
        }
        Cmd::Commit => {
            let engine = open_engine(&cli)?;
            let rows = engine.capture_rescan().map_err(|e| anyhow!("commit: {e}"))?;
            println!("captured {} change(s)", rows.len());
            Ok(())
        }
        Cmd::Authorize { pubkey, ttl, subdir, read_only } => {
            let engine = open_engine(&cli)?;
            let (expires_at, never) = resolve_ttl(ttl.as_deref(), &engine, &cli)?;
            // Normalize `--subdir` prefixes to forward-slash, strip leading/trailing
            // slashes; an empty list = the whole vault (allowed_paths=None).
            let allowed: Vec<String> = subdir
                .iter()
                .map(|s| s.replace('\\', "/").trim_matches('/').to_string())
                .filter(|s| !s.is_empty())
                .collect();
            let allowed_paths = if allowed.is_empty() { None } else { Some(allowed) };
            let node = engine
                .authorize_with_policy(pubkey, expires_at, never, "cli", allowed_paths.clone(), *read_only)
                .map_err(|e| anyhow!("authorize: {e}"))?;
            let scope = match (&allowed_paths, read_only) {
                (Some(p), true) => format!(" (read-only, subdir: {})", p.join(", ")),
                (Some(p), false) => format!(" (subdir: {})", p.join(", ")),
                (None, true) => " (read-only)".to_string(),
                (None, false) => String::new(),
            };
            println!("authorized {}{}", &node.to_hex()[..16], scope);
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
        Cmd::Branch { cmd } => branch_cmd(&cli, cmd),
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
            // New bridge verbs (pull/push/status/remote/rebaseline) are intercepted
            // BEFORE the read-only derived-repo allowlist; anything else falls through.
            match args.first().map(|s| s.as_str()) {
                Some(
                    "pull" | "push" | "status" | "remote" | "rebaseline" | "diff" | "plan"
                    | "policy",
                ) => git_bridge_cmd(&cli, args).await,
                _ => {
                    let engine = open_engine(&cli)?;
                    gitcli::run(&engine.git_dir, args)
                }
            }
        }
        Cmd::Scope => scope_cmd(&cli),
        Cmd::Completions { shell } => {
            let mut cmd = Cli::command();
            clap_complete::generate(*shell, &mut cmd, "asp", &mut std::io::stdout());
            Ok(())
        }
        Cmd::Sync { peer } => {
            let engine = open_engine(&cli)?;
            seed_authorized_keys(&cli, &engine)?;
            let auth = auth_opts(&cli, &engine);
            // No peer → use the saved peer (clone's `origin`); a supplied peer is
            // offered for saving, then used for this run.
            let supplied: Vec<String> = peer.clone().into_iter().collect();
            let spec = resolve_peers(&engine, &supplied)
                .into_iter()
                .next()
                .ok_or_else(|| anyhow!("no peer configured — pass a ticket/node id or `asp clone` first"))?;
            let addr = iroh_net::parse_peer(&spec)?;
            let ep = iroh_net::bind_endpoint_relay(&engine.identity.seed(), !cli.no_relay, cli.relay_url.as_deref()).await?;
            let r = iroh_net::sync_oneshot(Arc::new(Mutex::new(engine)), &ep, addr, &auth).await;
            ep.close().await;
            r
        }
        Cmd::Clone { peer, into, watch, depth, new_identity, all_branches, token, subdir } => {
            clone_cmd(&cli, peer, into.clone(), *watch, *depth, *new_identity, *all_branches, token.clone(), subdir.clone()).await
        }
        Cmd::Watch { listen, relay, relay_listen_addr, peers } => {
            watch_cmd(&cli, *listen, *relay, relay_listen_addr.clone(), peers.clone()).await
        }
        Cmd::Relay { bind, git_proxy, git_proxy_addr, git_proxy_allow } => {
            let bind = bind.clone().unwrap_or_else(|| "0.0.0.0:8080".into());
            let addr: std::net::SocketAddr = bind.parse().map_err(|_| anyhow!("bad relay bind address: {bind}"))?;
            if *git_proxy {
                let paddr = git_proxy_addr.clone().unwrap_or_else(|| "0.0.0.0:8081".into());
                let paddr: std::net::SocketAddr = paddr.parse().map_err(|_| anyhow!("bad git-proxy bind address: {paddr}"))?;
                let mut cfg = asp_core::gitproxy::GitProxyConfig::new(paddr);
                cfg.allow_hosts = git_proxy_allow.clone();
                let (proxy_addr, _handle) = asp_core::gitproxy::spawn_git_proxy(cfg).await?;
                println!("git CORS proxy on http://{proxy_addr} (TLS-terminated git traffic)");
            }
            println!("starting iroh relay on {addr} (forwards ciphertext, stores nothing)");
            iroh_net::run_relay(addr).await
        }
        Cmd::Ticket => {
            let engine = open_engine(&cli)?;
            let ep = iroh_net::bind_endpoint_relay(&engine.identity.seed(), !cli.no_relay, cli.relay_url.as_deref()).await?;
            let ticket = iroh_net::ticket(&ep, !cli.no_relay).await?;
            print_ticket(&ticket, &engine.site_id());
            ep.close().await;
            Ok(())
        }
    }
}

/// Print a connection ticket as copy-paste text, a node id, and a scannable QR
/// code in the terminal (phone-to-desktop pairing). QR is a render concern of
/// the surface, not the engine — the engine only emits the ticket string.
fn print_ticket(ticket: &str, node_id: &str) {
    println!("node:   {node_id}");
    println!("ticket: {ticket}");
    if let Ok(code) = qrcode::QrCode::new(ticket.as_bytes()) {
        let rendered = code
            .render::<qrcode::render::unicode::Dense1x2>()
            .quiet_zone(true)
            .build();
        println!("\n{rendered}");
    }
    println!("share the ticket (or scan the QR) on another device: `asp clone <ticket>`");
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

/// Resolve a branch CLI arg (an exact branch id, else a unique live name).
fn resolve_branch(engine: &Engine, arg: &str) -> Result<String> {
    let branches = engine.branches().map_err(|e| anyhow!("{e}"))?;
    if branches.iter().any(|b| b.branch_id == arg) {
        return Ok(arg.to_string());
    }
    let by_name: Vec<&asp_core::Branch> = branches.iter().filter(|b| b.name == arg).collect();
    match by_name.as_slice() {
        [b] => Ok(b.branch_id.clone()),
        [] => Err(anyhow!("no such branch: {arg}")),
        _ => Err(anyhow!("ambiguous branch name '{arg}'; pass the branch id")),
    }
}

fn branch_cmd(cli: &Cli, cmd: &BranchCmd) -> Result<()> {
    let engine = open_engine(cli)?;
    let head = engine.current_branch();
    match cmd {
        BranchCmd::List { json } => {
            let branches = engine.branches().map_err(|e| anyhow!("{e}"))?;
            if *json {
                let arr: Vec<_> = branches
                    .iter()
                    .map(|b| {
                        serde_json::json!({
                            "branch_id": b.branch_id,
                            "name": b.name,
                            "parent": b.parent,
                            "current": b.branch_id == head,
                        })
                    })
                    .collect();
                println!("{}", serde_json::to_string_pretty(&arr)?);
            } else {
                for b in &branches {
                    let mark = if b.branch_id == head { "*" } else { " " };
                    println!("{mark} {}  ({})", b.name, &b.branch_id[..8.min(b.branch_id.len())]);
                }
            }
            Ok(())
        }
        BranchCmd::Create { name, checkout } => {
            let id = engine.create_branch_here(name).map_err(|e| anyhow!("branch create: {e}"))?;
            if *checkout {
                engine.checkout(&id).map_err(|e| anyhow!("checkout: {e}"))?;
            }
            let short = &id[..8.min(id.len())];
            println!("created branch {name} ({short}){}", if *checkout { " — checked out" } else { "" });
            Ok(())
        }
        BranchCmd::Checkout { branch } => {
            let id = resolve_branch(&engine, branch)?;
            engine.checkout(&id).map_err(|e| anyhow!("checkout: {e}"))?;
            println!("switched to branch {branch}");
            Ok(())
        }
        BranchCmd::Delete { branch } => {
            let id = resolve_branch(&engine, branch)?;
            engine.delete_branch(&id).map_err(|e| anyhow!("branch delete: {e}"))?;
            println!("deleted branch {branch}");
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

#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_arguments)]
async fn clone_cmd(
    cli: &Cli,
    peer: &str,
    into: Option<PathBuf>,
    watch: bool,
    depth: Option<u32>,
    new_identity: bool,
    all_branches: bool,
    token: Option<String>,
    subdir: Vec<String>,
) -> Result<()> {
    // Auto-detect the source: a git URL clones over the bridge, anything else is an
    // ASP peer (iroh ticket / node id) — git-URL syntax is unambiguous (git-bridge §7.1).
    if let asp_core::gitbridge::SourceKind::GitUrl(url) =
        asp_core::gitbridge::detect_source(peer)
    {
        if !subdir.is_empty() {
            return Err(anyhow!("--subdir is an ASP partial clone; it does not apply to a git clone"));
        }
        return git_clone_cmd(cli, url, into, watch, depth, new_identity, all_branches, token).await;
    }
    // Normalize the partial-replica scope (scoped-sync §3.1): a pull-only leaf that
    // receives only these prefixes. Recorded locally BEFORE the catch-up so the
    // view filter hides any out-of-scope rows the moment they materialize.
    let allowed: Vec<String> = subdir
        .iter()
        .map(|s| s.replace('\\', "/").trim_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let dir = into.or_else(|| cli.dir.clone()).unwrap_or_else(|| PathBuf::from("asp-vault"));
    let id = idstore::load_or_generate(&dir, cli.no_home_key)?;
    let engine = Engine::open(&dir, id).map_err(|e| anyhow!("clone open: {e}"))?;
    if !allowed.is_empty() {
        engine.set_partial_scope(&allowed).map_err(|e| anyhow!("record partial scope: {e}"))?;
    }
    seed_authorized_keys(cli, &engine)?;
    let auth = auth_opts(cli, &engine);
    let seed = engine.identity.seed();
    let addr = iroh_net::parse_peer(peer)?;
    let ep = iroh_net::bind_endpoint_relay(&seed, !cli.no_relay, cli.relay_url.as_deref()).await?;
    let engine: EngineRef = Arc::new(Mutex::new(engine));
    let pinned = iroh_net::clone_bootstrap(engine.clone(), &ep, addr, &auth).await?;
    let vid = {
        let e = engine.lock().unwrap();
        // clone saves the source ticket as the default peer (`origin`), pinning
        // the listener's NodeId (verified by iroh) for re-dial.
        if saved_peer_urls(&e).is_empty() {
            let node_hex = pinned.map(|n| n.to_hex()).unwrap_or_default();
            let _ = e.store.add_peer(peer, &node_hex, now_unix());
        }
        VaultConfig::new(&e.store).vault_id()?.unwrap_or_default()
    };
    if allowed.is_empty() {
        println!("cloned vault {} into {}", &vid[..8.min(vid.len())], dir.display());
    } else {
        println!("cloned PARTIAL vault {} into {} (subdir: {}) — pull-only", &vid[..8.min(vid.len())], dir.display(), allowed.join(", "));
    }
    if watch {
        return run_watch_loop(cli, engine, ep, false, cli.relay_url.clone(), vec![peer.to_string()]).await;
    }
    ep.close().await;
    Ok(())
}

/// Clone a git remote into a fresh vault (git-bridge §3/§7.1).
#[allow(clippy::too_many_arguments)]
async fn git_clone_cmd(
    cli: &Cli,
    url: asp_core::gitwire::GitUrl,
    into: Option<PathBuf>,
    watch: bool,
    depth: Option<u32>,
    new_identity: bool,
    all_branches: bool,
    token: Option<String>,
) -> Result<()> {
    use asp_core::gitremote::{clone_from_git, resolve_git_auth, CloneOptions};

    let dir = into.or_else(|| cli.dir.clone()).unwrap_or_else(|| PathBuf::from("asp-vault"));
    let id = idstore::load_or_generate(&dir, cli.no_home_key)?;
    let engine = Engine::open(&dir, id).map_err(|e| anyhow!("clone open: {e}"))?;

    let auth = resolve_git_auth(&url, token.as_deref(), None);
    let spec = asp_core::gitbridge::GitRemoteSpec::new(url, auth);
    let last_phase = std::sync::Mutex::new(String::new());
    let on_progress = move |phase: &str, done: u64, total: u64| {
        let mut lp = last_phase.lock().unwrap();
        if *lp != phase {
            *lp = phase.to_string();
            match phase {
                "fetching" => eprintln!("fetching git objects…"),
                "replaying" => eprintln!("replaying commit history…"),
                "saving" => eprintln!("saving {total} rows…"),
                _ => {}
            }
        }
        let _ = done;
    };
    let opts = CloneOptions { depth, new_identity, all_branches, on_progress: Some(&on_progress) };
    let report = clone_from_git(&engine, &spec, &opts)
        .await
        .map_err(|e| anyhow!("git clone: {e}"))?;

    println!(
        "cloned git repo into {} — {} commit(s), vault {}, at {}",
        dir.display(),
        report.commits,
        &report.vault_id[..8.min(report.vault_id.len())],
        &report.tip_sha[..8.min(report.tip_sha.len())],
    );
    if !report.branches.is_empty() {
        println!("  branches: {}", report.branches.join(", "));
    }
    if all_branches {
        println!(
            "  {} open branch(es) imported, {} ref(s) skipped",
            report.open_branches, report.refs_skipped
        );
    }
    for w in &report.warnings {
        eprintln!("  warning: {w}");
    }

    if watch {
        let seed = engine.identity.seed();
        let ep = iroh_net::bind_endpoint_relay(&seed, !cli.no_relay, cli.relay_url.as_deref()).await?;
        return run_watch_loop(cli, Arc::new(Mutex::new(engine)), ep, false, cli.relay_url.clone(), vec![]).await;
    }
    Ok(())
}

/// The single configured git remote for this vault, or a clear error. v1 assumes one
/// remote per vault (the clone target).
fn only_git_remote(engine: &Engine) -> Result<asp_core::GitRemoteRow> {
    let mut remotes = engine.store.git_remote_list().map_err(|e| anyhow!("{e}"))?;
    match remotes.len() {
        0 => Err(anyhow!("no git remote configured — `asp clone <git-url>` or `asp git remote add <url>` first")),
        1 => Ok(remotes.remove(0)),
        _ => Err(anyhow!("multiple git remotes configured; ambiguous")),
    }
}

/// Validate a rollup-policy string (git-bridge §5.3). `llm` is not an engine policy —
/// it is `manual` plus the external `asp git diff` / `asp git plan` primitives — so
/// only `manual` and `interval` are accepted here.
fn validate_policy(p: &str) -> Result<&str> {
    match p {
        "manual" | "interval" => Ok(p),
        other => Err(anyhow!(
            "unknown policy '{other}' (use `manual` or `interval`; \
             for LLM-driven rollup keep `manual` and drive `asp git diff`/`asp git plan`)"
        )),
    }
}

/// Open `$EDITOR`/`$VISUAL` on a temp file pre-filled with `seed` and the pending
/// `diff` as `#`-commented context (git-bridge §5.3 manual policy). Returns the edited
/// message (comment lines stripped), or `None` when there is no editor / it aborted /
/// the message ended up empty — the caller then falls back to the generated summary.
fn edit_commit_message(seed: &str, diff: &str) -> Option<String> {
    let editor = std::env::var("EDITOR").ok().or_else(|| std::env::var("VISUAL").ok())?;
    if editor.trim().is_empty() {
        return None;
    }
    let path = std::env::temp_dir().join(format!("asp-commit-{}.txt", std::process::id()));
    let mut body = String::new();
    body.push_str(seed);
    body.push_str("\n\n# Please enter the commit message for your changes. Lines starting\n# with '#' are ignored. Below is the pending diff.\n#\n");
    for line in diff.lines() {
        body.push_str("# ");
        body.push_str(line);
        body.push('\n');
    }
    std::fs::write(&path, &body).ok()?;
    let status = std::process::Command::new(editor).arg(&path).status().ok()?;
    let out = if status.success() {
        std::fs::read_to_string(&path).ok()
    } else {
        None
    };
    let _ = std::fs::remove_file(&path);
    let edited = out?;
    let msg: String = edited
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    let msg = msg.trim().to_string();
    if msg.is_empty() {
        None
    } else {
        Some(msg)
    }
}

/// `asp git <pull|push|status|remote|rebaseline>` — the bridge verbs (git-bridge §7.1).
async fn git_bridge_cmd(cli: &Cli, args: &[String]) -> Result<()> {
    use asp_core::gitremote::{git_status, pull_once, rebaseline, store_git_token, PullReport};

    let engine = open_engine(cli)?;
    let verb = args.first().map(|s| s.as_str()).unwrap_or("");
    let has_flag = |name: &str| args.iter().any(|a| a == name);
    let flag_val = |name: &str| -> Option<String> {
        args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
    };

    // A partial (`--subdir`) replica must NEVER push to a git remote or author a
    // push plan (scoped-sync §3.4): its fold is a partial tree, so a push would
    // DELETE every out-of-scope file on the remote, and a plan authored here would
    // sync to full nodes that then push that partial tree. Hard-disabled.
    if matches!(verb, "push" | "plan") && engine.is_partial() {
        return Err(anyhow!(
            "git {verb} is disabled on a partial (--subdir) replica — its fold is a partial tree, \
             so pushing it would delete every out-of-scope file on the remote. Use a full replica to push."
        ));
    }

    match verb {
        "status" => {
            let remote = only_git_remote(&engine)?;
            let st = git_status(&engine, &remote.remote_id).map_err(|e| anyhow!("{e}"))?;
            if has_flag("--json") {
                println!("{}", serde_json::to_string_pretty(&st)?);
            } else {
                let at = st.at_sha.as_deref().map(|s| &s[..8.min(s.len())]).unwrap_or("-");
                println!("remote:  {}", st.remote_url);
                println!("at:      {at}");
                println!("ahead:   {}  behind: {}", st.ahead, st.behind);
                println!("policy:  {}", st.policy);
                if st.frozen {
                    println!("state:   FROZEN — upstream history was rewritten; run `asp git rebaseline`");
                }
            }
            Ok(())
        }
        "pull" => {
            let remote = only_git_remote(&engine)?;
            match pull_once(&engine, &remote.remote_id, None).await.map_err(|e| anyhow!("{e}"))? {
                PullReport::UpToDate => println!("already up to date"),
                PullReport::Frozen => println!(
                    "FROZEN — upstream history was rewritten; run `asp git rebaseline` to recover"
                ),
                PullReport::Updated { new_commits, branches_added } => {
                    println!("pulled {new_commits} new commit(s)");
                    if !branches_added.is_empty() {
                        println!("  branches: {}", branches_added.join(", "));
                    }
                }
            }
            Ok(())
        }
        "push" => {
            use asp_core::gitpush::{author_plan, pending_git_diff, push, PushReport};
            let remote = only_git_remote(&engine)?;
            let author = flag_val("--author");
            let explicit_msg = flag_val("-m").or_else(|| flag_val("--message"));

            // Manual policy: pre-fill the message from the pending diff summary, then
            // open $EDITOR (unless `-m` was given, or no editor is available).
            let pending = pending_git_diff(&engine, &remote.remote_id).map_err(|e| anyhow!("{e}"))?;
            let message = match explicit_msg {
                Some(m) => m,
                None => {
                    if pending.files_changed == 0 {
                        println!("nothing to push (no pending changes)");
                        return Ok(());
                    }
                    let seed = format!(
                        "asp: {} file(s) changed ({})",
                        pending.files_changed,
                        pending.paths.join(", ")
                    );
                    edit_commit_message(&seed, &pending.unified).unwrap_or(seed)
                }
            };

            author_plan(&engine, &remote.remote_id, &message, author.as_deref())
                .map_err(|e| anyhow!("{e}"))?;
            match push(&engine, &remote.remote_id, |_phase| {}).await.map_err(|e| anyhow!("{e}"))? {
                PushReport::Nothing => println!("nothing to push"),
                PushReport::Pushed { pushed_sha, commits_pushed, .. } => {
                    println!(
                        "pushed {commits_pushed} commit(s) to {} — now at {}",
                        remote.url,
                        &pushed_sha[..8.min(pushed_sha.len())]
                    );
                }
            }
            Ok(())
        }
        "diff" => {
            // The `llm` primitive (git-bridge §5.3): the pending diff since the last
            // plan/ingest frontier, for an external agent (or a human) to inspect
            // before deciding a commit boundary + message.
            use asp_core::gitpush::pending_git_diff;
            let remote = only_git_remote(&engine)?;
            let pd = pending_git_diff(&engine, &remote.remote_id).map_err(|e| anyhow!("{e}"))?;
            if has_flag("--json") {
                let out = serde_json::json!({
                    "files_changed": pd.files_changed,
                    "paths": pd.paths,
                    "unified": pd.unified,
                });
                println!("{}", serde_json::to_string_pretty(&out)?);
            } else {
                print!("{}", pd.unified);
                println!("# {} file(s) changed ({})", pd.files_changed, pd.paths.join(", "));
            }
            Ok(())
        }
        "plan" => {
            // The `llm` primitive (git-bridge §5.3): author a plan (a commit boundary +
            // message) WITHOUT pushing. An external agent calls `asp git diff` +
            // `asp git plan -m …` on its own cadence; `asp git push` (or the interval
            // tick) then synthesizes + pushes. The engine never calls a model.
            use asp_core::gitpush::author_plan;
            let remote = only_git_remote(&engine)?;
            let author = flag_val("--author");
            let message = flag_val("-m")
                .or_else(|| flag_val("--message"))
                .ok_or_else(|| anyhow!("usage: asp git plan -m <message> [--author <a>]"))?;
            let row = author_plan(&engine, &remote.remote_id, &message, author.as_deref())
                .map_err(|e| anyhow!("{e}"))?;
            println!(
                "authored plan {} — run `asp git push` (or let the interval policy) to synthesize + push",
                &row.id[..8.min(row.id.len())]
            );
            Ok(())
        }
        "policy" => {
            // Show or set the per-remote rollup policy (git-bridge §5.3).
            let remote = only_git_remote(&engine)?;
            match args.get(1).map(|s| s.as_str()) {
                None | Some("") | Some("show") => {
                    println!("policy: {}", remote.policy);
                    Ok(())
                }
                Some(p) => {
                    let p = validate_policy(p)?;
                    let mut row = remote;
                    row.policy = p.to_string();
                    engine.store.git_remote_upsert(&row).map_err(|e| anyhow!("{e}"))?;
                    println!("policy set to {p}");
                    Ok(())
                }
            }
        }
        "rebaseline" => {
            let remote = only_git_remote(&engine)?;
            if !has_flag("--yes") {
                eprintln!(
                    "rebaseline re-imports the rewritten upstream tip as one snapshot batch.\n\
                     Re-run with --yes to confirm."
                );
                return Ok(());
            }
            match rebaseline(&engine, &remote.remote_id).await.map_err(|e| anyhow!("{e}"))? {
                PullReport::Updated { .. } => println!("rebaselined — freeze cleared, vault snapped to the new tip"),
                other => println!("rebaseline: {other:?}"),
            }
            Ok(())
        }
        "remote" => {
            let sub = args.get(1).map(|s| s.as_str()).unwrap_or("");
            match sub {
                "add" => {
                    let url = args.get(2).ok_or_else(|| anyhow!("usage: asp git remote add <url> [--push-ref R] [--policy P] [--token T]"))?;
                    let giturl = asp_core::gitwire::parse_git_url(url)
                        .ok_or_else(|| anyhow!("not a git url: {url}"))?;
                    let rid = asp_core::gitbridge::remote_id(url);
                    let _ = &giturl; // validated above; auth resolves lazily at pull time
                    let token = flag_val("--token");
                    let auth_ref = token.as_deref().and_then(|t| store_git_token(&rid, t));
                    let policy = match flag_val("--policy") {
                        Some(p) => validate_policy(&p)?.to_string(),
                        None => "manual".to_string(),
                    };
                    let row = asp_core::GitRemoteRow {
                        remote_id: rid.clone(),
                        url: url.clone(),
                        push_ref: flag_val("--push-ref"),
                        policy,
                        auth_ref,
                        default_branch: None,
                        last_ingested_sha: None,
                        remote_ref: None,
                        root_sha: None,
                        frozen: false,
                        last_pushed_sha: None,
                        last_pushed_frontier: None,
                    };
                    engine.store.git_remote_upsert(&row).map_err(|e| anyhow!("{e}"))?;
                    println!("added git remote {rid} ({url})");
                    Ok(())
                }
                "remove" => {
                    let remote = only_git_remote(&engine)?;
                    engine.store.git_remote_remove(&remote.remote_id).map_err(|e| anyhow!("{e}"))?;
                    println!("removed git remote {}", remote.remote_id);
                    Ok(())
                }
                "show" | "" => {
                    for r in engine.store.git_remote_list().map_err(|e| anyhow!("{e}"))? {
                        let at = r.last_ingested_sha.as_deref().map(|s| &s[..8.min(s.len())]).unwrap_or("-");
                        println!(
                            "{}  {}  ({}{})  at {at}",
                            r.remote_id,
                            r.url,
                            r.policy,
                            if r.frozen { ", frozen" } else { "" },
                        );
                    }
                    Ok(())
                }
                other => Err(anyhow!("unknown `asp git remote` subcommand: {other}")),
            }
        }
        other => Err(anyhow!("unknown git bridge verb: {other}")),
    }
}

async fn watch_cmd(
    cli: &Cli,
    listen: bool,
    relay: bool,
    relay_listen_addr: Option<String>,
    peers: Vec<String>,
) -> Result<()> {
    let engine = open_engine(cli)?;
    // Partial (`--subdir`) replicas are pull-only LEAVES (scoped-sync §3.1): their
    // sparse version vector would hand a third peer a permanent hole, so they must
    // never serve (`--listen`) and must pin exactly ONE upstream. Enforced here,
    // structurally.
    if engine.is_partial() && listen {
        return Err(anyhow!(
            "this is a partial (--subdir) replica — it is pull-only and cannot --listen \
             (its scoped version vector would strand a downstream peer). Re-clone the full vault to serve peers."
        ));
    }
    seed_authorized_keys(cli, &engine)?;
    let ttl_days = default_ttl_days(cli, &engine);
    let filled = engine.migrate_keys(ttl_days).map_err(|e| anyhow!("{e}"))?;
    if filled > 0 {
        tracing::info!(filled, "authorized_keys expiry migration");
    }
    // No --peer → connect to the saved peer(s) (clone's `origin`); a supplied
    // --peer is offered for saving (consent), then used.
    let resolved = resolve_peers(&engine, &peers);
    if engine.is_partial() {
        let distinct: std::collections::HashSet<&String> = resolved.iter().collect();
        if distinct.len() > 1 {
            return Err(anyhow!(
                "a partial (--subdir) replica pins exactly one upstream, but {} were given \
                 (a second upstream breaks the scoped version vector). Re-clone the full vault for a mesh.",
                distinct.len()
            ));
        }
    }

    // `--relay`: co-host an iroh relay in this same process and pin it as this
    // node's home relay so the ticket routes peers through it (all-in-one box).
    // The endpoint advertises the operator's public `--relay-url` if given, else
    // the local bind (LAN/loopback only). Pinning a relay url overrides the
    // public-vs-no-relay choice (bind_endpoint_relay uses RelayMode::Custom).
    let home_relay = if relay {
        let bind = relay_listen_addr.unwrap_or_else(|| "0.0.0.0:8080".into());
        let addr: std::net::SocketAddr =
            bind.parse().map_err(|_| anyhow!("bad relay bind address: {bind}"))?;
        tokio::spawn(async move {
            if let Err(e) = iroh_net::run_relay(addr).await {
                tracing::error!("co-hosted relay stopped: {e}");
            }
        });
        let url = cli
            .relay_url
            .clone()
            .unwrap_or_else(|| format!("http://127.0.0.1:{}", addr.port()));
        println!("co-hosting iroh relay on {addr} — advertising {url} as home relay");
        Some(url)
    } else {
        cli.relay_url.clone()
    };

    let ep =
        iroh_net::bind_endpoint_relay(&engine.identity.seed(), !cli.no_relay, home_relay.as_deref()).await?;
    run_watch_loop(cli, Arc::new(Mutex::new(engine)), ep, listen, home_relay, resolved).await
}

async fn run_watch_loop(
    cli: &Cli,
    engine: EngineRef,
    ep: iroh_net::Endpoint,
    listen: bool,
    relay_url: Option<String>,
    peers: Vec<String>,
) -> Result<()> {
    let conns = iroh_net::new_conns();
    let (auth, _vid, root, site, debounce) = {
        let e = engine.lock().unwrap();
        let mut auth = auth_opts(cli, &e);
        // Secure-by-default for a publicly-reachable listener: TOFU silently
        // enrolls the *first stranger* to dial, which is a land-grab risk once
        // the node is reachable over public relays/discovery. So a public
        // listener never falls back to open TOFU — admission requires an auth key
        // or a pre-authorized key. Hermetic/LAN listeners (`--no-relay`) keep
        // TOFU for easy pairing. An explicit `--no-tofu` is honored either way.
        let public_listener = listen && !cli.no_relay;
        if listener_hardens_tofu(listen, !cli.no_relay) {
            auth.no_tofu = true;
        }
        if public_listener && auth.auth_keys.is_empty() && e.store.authkeys_empty().unwrap_or(true) {
            eprintln!(
                "warning: public listener has no auth key and an empty authorized set — \
                 no new peer can enroll (TOFU is disabled when reachable over public relays).\n\
                 Set --auth-key <secret> (or ASP_AUTH_KEY), or pre-authorize a key with `asp authorize`."
            );
        }
        let vid = VaultConfig::new(&e.store).vault_id()?.unwrap_or_default();
        let debounce = cli.debounce.unwrap_or_else(|| VaultConfig::new(&e.store).debounce_ms().unwrap_or(400));
        e.capture_rescan().map_err(|err| anyhow!("reconcile: {err}"))?;
        (auth, vid, e.root.clone(), e.site_id(), debounce)
    };

    if listen {
        // A listening node is a hub: print its ticket + QR so peers can pair.
        match iroh_net::ticket_with_relay(&ep, !cli.no_relay, relay_url.as_deref()).await {
            Ok(ticket) => {
                println!("listening as a hub — share this ticket with peers:");
                print_ticket(&ticket, &site);
            }
            Err(e) => tracing::warn!("could not mint ticket: {e}"),
        }
        let (e, a, c, server_ep) = (engine.clone(), auth.clone(), conns.clone(), ep.clone());
        tokio::spawn(async move {
            if let Err(e) = iroh_net::serve(e, server_ep, a, c).await {
                tracing::error!("listener stopped: {e}");
            }
        });
    }

    for spec in peers {
        let (e, a, c, dial_ep) = (engine.clone(), auth.clone(), conns.clone(), ep.clone());
        tokio::spawn(async move {
            // Reconnect with exponential backoff + full jitter. A flapping or
            // rejected peer (auth fail, listener overload, network blip) must not
            // turn into a tight redial storm: every reconnect drives a full
            // version-vector catch-up on the listener, so a 2s fixed retry from
            // many peers can saturate the hub and become self-sustaining
            // (overload → drops → synchronized redials → more overload). Jitter
            // de-synchronizes peers; the delay resets once a connection has
            // stayed up long enough to count as healthy.
            const BASE: std::time::Duration = std::time::Duration::from_secs(1);
            const MAX: std::time::Duration = std::time::Duration::from_secs(60);
            const HEALTHY: std::time::Duration = std::time::Duration::from_secs(30);
            let mut backoff = BASE;
            // Resolve the peer spec (ticket / node id) once; iroh re-resolves the
            // live address on each dial via the embedded hints + discovery.
            let addr = match iroh_net::parse_peer(&spec) {
                Ok(a) => a,
                Err(err) => {
                    eprintln!("error: bad peer {spec}: {err}");
                    return;
                }
            };
            loop {
                let started = std::time::Instant::now();
                if let Err(err) = iroh_net::connect(e.clone(), &dial_ep, addr.clone(), &a, false, c.clone(), None).await {
                    let msg = err.to_string();
                    // A vault mismatch is permanent — retrying is futile. Tell the
                    // operator exactly how to fix it and stop hammering the peer.
                    if msg.contains("different vault") {
                        eprintln!(
                            "error: peer {spec} is a DIFFERENT vault — they were created separately and won't merge.\n\
                             To follow it, clone instead of init: `asp clone {spec} <dir>` (this folder's local edits would be replaced)."
                        );
                        break;
                    }
                    tracing::debug!("peer {spec} disconnected: {err}");
                }
                // A connection that lasted a while was healthy — restart from BASE.
                // One that dropped almost immediately escalates the delay.
                if started.elapsed() >= HEALTHY {
                    backoff = BASE;
                }
                // Full jitter: sleep a uniform-random duration in [0, backoff].
                let delay = backoff.mul_f64(rand::random::<f64>());
                tokio::time::sleep(delay).await;
                backoff = (backoff * 2).min(MAX);
            }
        });
    }

    let _watcher = net::spawn_watcher(engine.clone(), conns.clone(), debounce).context("watcher")?;

    println!("watching {} (node {})", root.display(), &site[..12.min(site.len())]);

    // When a git remote is configured, arm a periodic fetch/ingest tick alongside the
    // fs watcher + peer reconnects (git-bridge §4/§7.1). Kept minimal: a plain interval
    // branch in the same select as ctrl-c. The pull runs inline (not spawned) so it may
    // borrow the engine across its awaits without a Send bound; it briefly serializes
    // with the fs watcher, which is fine at a 5-minute cadence.
    let has_git_remote = {
        let e = engine.lock().unwrap();
        !e.store.git_remote_list().unwrap_or_default().is_empty()
    };
    if has_git_remote {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(300));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // The rollup-policy tick (git-bridge §5.3): a faster cadence than the pull so an
        // `interval`-policy remote authors + pushes near its quiescence/window bound. A
        // no-op for `manual`/`llm` remotes (they never author automatically).
        let mut policy_tick = tokio::time::interval(std::time::Duration::from_secs(60));
        policy_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => break,
                _ = tick.tick() => {
                    // Small jitter so multiple bridges don't fetch in lockstep (§4.3).
                    let jitter = std::time::Duration::from_millis(rand::random::<u64>() % 30_000);
                    tokio::time::sleep(jitter).await;
                    git_pull_tick(&engine).await;
                }
                _ = policy_tick.tick() => {
                    git_policy_tick(&engine).await;
                }
            }
        }
    } else {
        tokio::signal::ctrl_c().await.ok();
    }
    Ok(())
}

/// One periodic-pull pass over every configured git remote. Errors/frozen states are
/// logged, never fatal — the watch loop keeps running.
///
/// The engine lock is intentionally held across the pull's awaits: the whole engine
/// (SQLite) sits behind one `std::sync::Mutex`, `pull_once` interleaves async fetches
/// with sync store writes, and pulls are infrequent (5-min cadence) + short — so
/// briefly serializing with the fs watcher is the pragmatic v1 choice. The future is
/// only awaited inline (never spawned), so no `Send` bound is violated.
#[allow(clippy::await_holding_lock)]
async fn git_pull_tick(engine: &EngineRef) {
    use asp_core::gitremote::{pull_once, PullReport};
    let remotes = {
        let e = engine.lock().unwrap();
        e.store.git_remote_list().unwrap_or_default()
    };
    for r in remotes {
        // Hold the lock across the pull's awaits: pulls are infrequent (5-min cadence)
        // and short, so briefly serializing with the fs watcher is acceptable for v1.
        let result = {
            let e = engine.lock().unwrap();
            pull_once(&e, &r.remote_id, None).await
        };
        match result {
            Ok(PullReport::Updated { new_commits, .. }) if new_commits > 0 => {
                tracing::info!(remote = %r.remote_id, new_commits, "git pull: ingested new commits");
            }
            Ok(PullReport::Frozen) => {
                tracing::warn!(remote = %r.remote_id, "git pull: upstream history rewritten — frozen (run `asp git rebaseline`)");
            }
            Ok(_) => {}
            Err(e) => tracing::warn!(remote = %r.remote_id, "git pull failed: {e}"),
        }
    }
}

/// One `interval`-policy pass over every configured git remote (git-bridge §5.3).
/// A no-op for `manual`/`llm` remotes. Errors are logged, never fatal.
///
/// Like [`git_pull_tick`], the engine lock is held across the tick's awaits: the whole
/// engine sits behind one `std::sync::Mutex`, `interval_tick` interleaves an async push
/// (and a short dedup jitter sleep) with sync store writes, and it only actually
/// authors/pushes at the quiescence/window boundary — so briefly serializing with the
/// fs watcher is the pragmatic v1 choice. Awaited inline (never spawned) so no `Send`
/// bound is violated.
#[allow(clippy::await_holding_lock)]
async fn git_policy_tick(engine: &EngineRef) {
    use asp_core::gitpolicy::{interval_tick, IntervalParams, PolicyAction, POLICY_INTERVAL};
    // A partial (`--subdir`) replica never authors an auto-push plan (scoped-sync
    // §3.4): its partial-tree plan would sync to full nodes and delete out-of-scope
    // files on the remote. Suppress the whole interval policy on such a node.
    if engine.lock().unwrap().is_partial() {
        return;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let remotes = {
        let e = engine.lock().unwrap();
        e.store.git_remote_list().unwrap_or_default()
    };
    for r in remotes {
        if r.policy != POLICY_INTERVAL {
            continue;
        }
        let result = {
            let e = engine.lock().unwrap();
            let params =
                IntervalParams::from_config(&asp_core::config::VaultConfig::new(&e.store))
                    .unwrap_or_default();
            interval_tick(&e, &r.remote_id, now, params).await
        };
        match result {
            Ok(PolicyAction::Pushed { sha }) => {
                tracing::info!(remote = %r.remote_id, sha = %&sha[..8.min(sha.len())], "git interval policy: authored + pushed");
            }
            Ok(PolicyAction::Authored { plans }) => {
                tracing::info!(remote = %r.remote_id, plans, "git interval policy: authored plan (nothing new to push)");
            }
            Ok(PolicyAction::Skip) => {}
            Err(e) => tracing::warn!(remote = %r.remote_id, "git interval policy failed: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::listener_hardens_tofu;

    #[test]
    fn tofu_hardening_only_for_public_listeners() {
        // A publicly-reachable listener never silently TOFU-enrolls a stranger.
        assert!(listener_hardens_tofu(true, true), "public listener hardens TOFU");
        // A hermetic/LAN listener (--no-relay) keeps TOFU for easy pairing.
        assert!(!listener_hardens_tofu(true, false), "LAN listener keeps TOFU");
        // A pure connector (no --listen) is never a TOFU surface either way.
        assert!(!listener_hardens_tofu(false, true), "connector unaffected");
        assert!(!listener_hardens_tofu(false, false), "connector unaffected");
    }
}
