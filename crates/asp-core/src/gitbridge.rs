//! gitbridge — the native transport + local-object-store layer for the git bridge
//! (§2 module table, §2.1, §6.3, §8 of `specs/git-bridge.md`).
//!
//! **Bytes and refs, not model.** [`crate::gitwire`] owns the pure protocol framing
//! (pkt-line, protocol-v2 `ls-refs`/`fetch`, v0 `receive-pack`); the pure history
//! model + replay lives in `gitimport`. This module is the middle layer that turns
//! those bytes into actual network I/O and an on-disk object store:
//!
//! * **Transports** — [`HttpsTransport`] (reqwest, rustls) implementing the stateless
//!   [`GitTransport`] round-trips, and an SSH path that spawns the user's `ssh` binary
//!   (§8). Both speak the same [`gitwire`](crate::gitwire) bytes.
//! * **High-level ops** — [`ls_remote`], [`fetch_pack`], [`push_pack`]: compose a
//!   transport with `gitwire` builders/parsers into the three operations the
//!   clone/pull/push orchestration needs.
//! * **Local bare object store** — [`RemoteStore`] under `.asp/gitremote/<remote_id>/`
//!   (§6.3): fetched packs land here (via `gix-pack` index-pack), powering ancestry
//!   checks (force-push detection, §4.4) and push base selection.
//! * **Pack writer** — [`write_pack`], a minimal non-delta pack encoder the commit
//!   synthesis slice reuses to assemble the objects a push sends.
//!
//! Native-only (`cfg(not(target_arch = "wasm32"))`): reqwest/tokio/`ssh` and the
//! bare object store are all native. The browser reuses `gitwire` + `gitimport` with
//! a `fetch()`-backed transport in `asp-wasm`.
//!
//! ## SSH protocol decision
//!
//! `gitwire` implements **protocol v2 only** for the fetch/`upload-pack` path. Git
//! over SSH gets v2 by exporting `GIT_PROTOCOL=version=2` to the remote command; the
//! industry-standard mechanism (used by git itself and gix) is
//! `ssh -o SendEnv=GIT_PROTOCOL` with `GIT_PROTOCOL` set in the spawned child's
//! environment. GitHub / GitLab / Gitea all `AcceptEnv GIT_PROTOCOL`, so this yields
//! a real v2 capability advertisement over the pipe, which we parse with the same
//! [`parse_capability_advertisement`](crate::gitwire::parse_capability_advertisement)
//! used for HTTPS. If a server does not offer v2 over SSH (older/locked-down sshd),
//! we surface a clear typed error suggesting the HTTPS URL rather than falling back to
//! a v0 parser we do not own. `-o BatchMode=yes` keeps auth non-interactive; a missing
//! `ssh` binary is reported with a "use https://" hint. Host-key verification and key
//! selection stay entirely inside the user's `ssh` (`~/.ssh/config`, agents, hardware
//! keys) — we never parse private keys. The `ASP_GIT_SSH` env var overrides the `ssh`
//! binary (used by tests via a shim; also handy for a wrapper script).

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use base64::Engine as _;
use flate2::write::ZlibEncoder;
use flate2::Compression;
use futures_util::StreamExt;
use sha1::{Digest, Sha1};
use sha2::Sha256;

use crate::gitwire::{
    build_fetch, build_ls_refs, build_update_request, info_refs_url, parse_capability_advertisement,
    parse_ls_refs_response, parse_receive_pack_advertisement, parse_report_status,
    receive_pack_info_refs_url, receive_pack_url, upload_pack_url, FetchRequest,
    FetchResponseParser, GitUrl, GitWireError, Pkt, PktReader, RefInfo,
};
use crate::store::BlobStore;

/// `agent=` string for pushes (matches gitwire's private constant format).
const AGENT: &str = concat!("asp/", env!("CARGO_PKG_VERSION"));

/// Default response-body cap for a single round trip (§7.3 uses the same 1 GiB for
/// the proxy). Packs up to ~1 GiB are buffered in memory for v1; streaming is a
/// later optimization.
pub const DEFAULT_MAX_BODY: u64 = 1 << 30;

// ===========================================================================
// Errors
// ===========================================================================

/// Every fallible op in this module returns this typed error so the clone/pull/push
/// orchestration can branch on the cause — most importantly [`GitBridgeError::NonFastForward`],
/// which drives the §5.2 bounded push retry, and [`GitBridgeError::Auth`] for the
/// credential-rejected surface (§9).
#[derive(Debug)]
pub enum GitBridgeError {
    /// A `gitwire` framing/protocol error (advertisement, response, or report).
    Wire(GitWireError),
    /// The remote said no to our credentials (HTTP 401/403).
    Auth,
    /// The repo/endpoint was not found (HTTP 404).
    NotFound,
    /// A non-fast-forward ref update was rejected (a human pushed between our fetch
    /// and push, or the base is stale). The push-retry slice re-fetches on this.
    NonFastForward,
    /// The ref update was rejected for another reason (hook declined, locked, …).
    Rejected(String),
    /// The remote reported a fatal error (side-band band 3, `unpack` failure, …).
    Remote(String),
    /// A transport-level failure (HTTP status, connection, DNS, timeout).
    Http(String),
    /// The SSH subprocess path failed (spawn/exit/no-v2).
    Ssh(String),
    /// A local object-store / packfile-decode failure.
    Store(String),
    /// Local I/O.
    Io(String),
}

/// Alias matching the spec's naming for the push error surface; every variant of
/// [`GitBridgeError`] can arise from a push, but callers key off `NonFastForward`.
pub type PushError = GitBridgeError;

impl std::fmt::Display for GitBridgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GitBridgeError::Wire(e) => write!(f, "{e}"),
            GitBridgeError::Auth => write!(f, "git remote rejected credentials (401/403)"),
            GitBridgeError::NotFound => write!(f, "git remote or repository not found (404)"),
            GitBridgeError::NonFastForward => write!(f, "push rejected: non-fast-forward"),
            GitBridgeError::Rejected(s) => write!(f, "push rejected: {s}"),
            GitBridgeError::Remote(s) => write!(f, "git remote error: {s}"),
            GitBridgeError::Http(s) => write!(f, "git transport error: {s}"),
            GitBridgeError::Ssh(s) => write!(f, "git ssh error: {s}"),
            GitBridgeError::Store(s) => write!(f, "git object store error: {s}"),
            GitBridgeError::Io(s) => write!(f, "io error: {s}"),
        }
    }
}

impl std::error::Error for GitBridgeError {}

impl From<GitWireError> for GitBridgeError {
    fn from(e: GitWireError) -> Self {
        GitBridgeError::Wire(e)
    }
}
impl From<std::io::Error> for GitBridgeError {
    fn from(e: std::io::Error) -> Self {
        GitBridgeError::Io(e.to_string())
    }
}
impl From<GitBridgeError> for crate::error::AspError {
    fn from(e: GitBridgeError) -> Self {
        crate::error::AspError::Protocol(e.to_string())
    }
}

/// Result alias for the bridge layer.
pub type BridgeResult<T> = Result<T, GitBridgeError>;

// ===========================================================================
// Auth + remote spec + source detection
// ===========================================================================

/// Credentials for a remote (native; §8). Tokens are held only in memory here; where
/// they are *persisted* (keyring / `ASP_GIT_TOKEN`) is a surface concern above us.
#[derive(Debug, Clone)]
pub enum GitAuth {
    /// No credentials (public HTTPS clone).
    Anonymous,
    /// An HTTPS token (a GitHub-style PAT), sent as HTTP Basic `x-access-token:<token>`.
    Token(String),
    /// SSH — the spawned `ssh` binary owns auth (agent, `~/.ssh/config`, hardware keys).
    SshAgent,
}

/// A remote to talk to: a parsed URL plus the credentials to use.
#[derive(Debug, Clone)]
pub struct GitRemoteSpec {
    /// The parsed remote URL.
    pub url: GitUrl,
    /// The credentials.
    pub auth: GitAuth,
}

impl GitRemoteSpec {
    /// Construct a spec from an already-parsed URL and auth.
    pub fn new(url: GitUrl, auth: GitAuth) -> Self {
        Self { url, auth }
    }

    /// Parse `input` as a git URL and pair it with `auth`, or `None` if it is not a
    /// git URL (the caller then treats it as an ASP peer — see [`detect_source`]).
    pub fn parse(input: &str, auth: GitAuth) -> Option<Self> {
        crate::gitwire::parse_git_url(input).map(|url| Self { url, auth })
    }

    /// The `https://…` base for an HTTPS remote, if this is one.
    pub fn https_base(&self) -> Option<&str> {
        match &self.url {
            GitUrl::Https { base } => Some(base),
            GitUrl::Ssh { .. } => None,
        }
    }
}

/// What a pasted clone source is: a git remote or an ASP peer. Thin wrapper over
/// `gitwire`'s URL detection so the CLI/desktop can branch (git first, else peer —
/// git-URL syntax is unambiguous, ASP tickets/node-ids are not).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceKind {
    /// A git remote URL.
    GitUrl(GitUrl),
    /// Not a git URL — hand to `parse_peer` for an ASP ticket / node-id.
    Peer,
}

/// Classify a pasted clone source (§7.1 detection order: git URL first, else peer).
pub fn detect_source(input: &str) -> SourceKind {
    match crate::gitwire::parse_git_url(input) {
        Some(u) => SourceKind::GitUrl(u),
        None => SourceKind::Peer,
    }
}

// ===========================================================================
// Transport
// ===========================================================================

/// Which smart-HTTP service an `info/refs` probe targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Service {
    /// `git-upload-pack` — fetch (protocol v2).
    UploadPack,
    /// `git-receive-pack` — push (protocol v0).
    ReceivePack,
}

impl Service {
    fn remote_command(self) -> &'static str {
        match self {
            Service::UploadPack => "git-upload-pack",
            Service::ReceivePack => "git-receive-pack",
        }
    }
}

/// The stateless round-trip seam every HTTPS-shaped transport implements (browser
/// `fetch()` will implement the same shape in `asp-wasm`). Two response bodies are
/// buffered `Vec<u8>` for v1; SSH is stateful and does **not** go through this trait
/// (see [`ls_remote`]/[`fetch_pack`]).
#[allow(async_fn_in_trait)]
pub trait GitTransport {
    /// `GET .../info/refs?service=…` — returns the raw advertisement bytes.
    async fn info_refs(&self, service: Service) -> BridgeResult<Vec<u8>>;
    /// `POST .../git-upload-pack` — a v2 stateless round trip (ls-refs / fetch).
    async fn upload_pack(&self, request_body: Vec<u8>) -> BridgeResult<Vec<u8>>;
    /// `POST .../git-receive-pack` — a push round trip (update commands + pack).
    async fn receive_pack(&self, request_body: Vec<u8>) -> BridgeResult<Vec<u8>>;
}

/// HTTPS transport over `reqwest` (rustls; already configured in the dep). Follows
/// same-host redirects only (max 3 — GitHub redirects `…/foo` → `…/foo.git`), caps
/// the response body, and attaches the token as HTTP Basic per the GitHub PAT
/// convention.
pub struct HttpsTransport {
    base: String,
    auth: GitAuth,
    client: reqwest::Client,
    max_body: u64,
}

impl HttpsTransport {
    /// Build a transport for the `https://…` base with the given auth.
    pub fn new(base: impl Into<String>, auth: GitAuth) -> BridgeResult<Self> {
        Self::with_cap(base, auth, DEFAULT_MAX_BODY)
    }

    /// Like [`new`](Self::new) with an explicit response-body cap.
    pub fn with_cap(base: impl Into<String>, auth: GitAuth, max_body: u64) -> BridgeResult<Self> {
        let redirect = reqwest::redirect::Policy::custom(|attempt| {
            // Follow at most 3 hops, and only when the host does not change (GitHub's
            // `foo` → `foo.git` is same-host). Cross-host or over-limit → stop and let
            // the caller see the 3xx (which check_status turns into an error).
            if attempt.previous().len() > 3 {
                return attempt.stop();
            }
            let orig_host = attempt.previous().first().and_then(|u| u.host_str());
            if orig_host.is_some() && orig_host == attempt.url().host_str() {
                attempt.follow()
            } else {
                attempt.stop()
            }
        });
        let client = reqwest::Client::builder()
            .redirect(redirect)
            .connect_timeout(std::time::Duration::from_secs(30))
            .timeout(std::time::Duration::from_secs(600))
            .build()
            .map_err(|e| GitBridgeError::Http(format!("failed to build https client: {e}")))?;
        Ok(Self { base: base.into(), auth, client, max_body })
    }

    /// Build a transport from a spec whose URL is HTTPS.
    pub fn from_spec(spec: &GitRemoteSpec) -> BridgeResult<Self> {
        match &spec.url {
            GitUrl::Https { base } => Self::new(base.clone(), spec.auth.clone()),
            GitUrl::Ssh { .. } => Err(GitBridgeError::Http("not an https remote".into())),
        }
    }

    fn auth_header(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.auth {
            GitAuth::Token(t) => {
                // GitHub PAT convention: Basic base64("x-access-token:<token>").
                let raw = format!("x-access-token:{t}");
                let val = format!(
                    "Basic {}",
                    base64::engine::general_purpose::STANDARD.encode(raw.as_bytes())
                );
                rb.header(reqwest::header::AUTHORIZATION, val)
            }
            GitAuth::Anonymous | GitAuth::SshAgent => rb,
        }
    }
}

fn check_status(resp: &reqwest::Response) -> BridgeResult<()> {
    let s = resp.status();
    if s.is_success() {
        Ok(())
    } else if s == reqwest::StatusCode::UNAUTHORIZED || s == reqwest::StatusCode::FORBIDDEN {
        Err(GitBridgeError::Auth)
    } else if s == reqwest::StatusCode::NOT_FOUND {
        Err(GitBridgeError::NotFound)
    } else {
        Err(GitBridgeError::Http(format!("unexpected HTTP status {s}")))
    }
}

async fn read_capped(resp: reqwest::Response, max: u64) -> BridgeResult<Vec<u8>> {
    read_capped_progress(resp, max, |_, _| {}).await
}

/// Like [`read_capped`], but invokes `on_bytes(downloaded, total)` — cumulative bytes
/// so far and the response's `Content-Length` — after each streamed chunk lands. Drives
/// the clone "fetching" phase. `total` is `0` when the server omits Content-Length
/// (GitHub smart-HTTP is chunked, so this is the common case — the phase then shows a
/// live byte count under a segment shimmer rather than a determinate bar; the total is
/// never faked).
async fn read_capped_progress(
    resp: reqwest::Response,
    max: u64,
    mut on_bytes: impl FnMut(u64, u64),
) -> BridgeResult<Vec<u8>> {
    let content_len = resp.content_length();
    if let Some(len) = content_len {
        if len > max {
            return Err(GitBridgeError::Http("response exceeds size cap".into()));
        }
    }
    let announced = content_len.unwrap_or(0);
    let mut stream = resp.bytes_stream();
    let mut out = Vec::new();
    let mut total: u64 = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| GitBridgeError::Http(format!("stream error: {e}")))?;
        total += chunk.len() as u64;
        if total > max {
            return Err(GitBridgeError::Http("response body exceeded size cap".into()));
        }
        out.extend_from_slice(&chunk);
        on_bytes(total, announced);
    }
    Ok(out)
}

impl GitTransport for HttpsTransport {
    async fn info_refs(&self, service: Service) -> BridgeResult<Vec<u8>> {
        let url = match service {
            Service::UploadPack => info_refs_url(&self.base),
            Service::ReceivePack => receive_pack_info_refs_url(&self.base),
        };
        let mut rb = self.client.get(&url);
        // Protocol v2 is requested for upload-pack only; receive-pack advertisement
        // is v0 and needs no Git-Protocol header.
        if service == Service::UploadPack {
            rb = rb.header("Git-Protocol", "version=2");
        }
        rb = self.auth_header(rb);
        let resp = rb
            .send()
            .await
            .map_err(|e| GitBridgeError::Http(format!("info/refs request failed: {e}")))?;
        check_status(&resp)?;
        read_capped(resp, self.max_body).await
    }

    async fn upload_pack(&self, body: Vec<u8>) -> BridgeResult<Vec<u8>> {
        let url = upload_pack_url(&self.base);
        let rb = self
            .client
            .post(&url)
            .header("Git-Protocol", "version=2")
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-git-upload-pack-request",
            )
            .header(
                reqwest::header::ACCEPT,
                "application/x-git-upload-pack-result",
            )
            .body(body);
        let rb = self.auth_header(rb);
        let resp = rb
            .send()
            .await
            .map_err(|e| GitBridgeError::Http(format!("upload-pack request failed: {e}")))?;
        check_status(&resp)?;
        read_capped(resp, self.max_body).await
    }

    async fn receive_pack(&self, body: Vec<u8>) -> BridgeResult<Vec<u8>> {
        let url = receive_pack_url(&self.base);
        let rb = self
            .client
            .post(&url)
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-git-receive-pack-request",
            )
            .header(
                reqwest::header::ACCEPT,
                "application/x-git-receive-pack-result",
            )
            .body(body);
        let rb = self.auth_header(rb);
        let resp = rb
            .send()
            .await
            .map_err(|e| GitBridgeError::Http(format!("receive-pack request failed: {e}")))?;
        check_status(&resp)?;
        read_capped(resp, self.max_body).await
    }
}

impl HttpsTransport {
    /// Like the trait [`upload_pack`](GitTransport::upload_pack), but reports the
    /// cumulative downloaded byte count to `on_bytes` as the response body streams
    /// in (drives the clone "fetching" phase). Off the trait so the browser-shaped
    /// seam stays a plain buffered round-trip.
    async fn upload_pack_progress(
        &self,
        body: Vec<u8>,
        on_bytes: impl FnMut(u64, u64),
    ) -> BridgeResult<Vec<u8>> {
        let url = upload_pack_url(&self.base);
        let rb = self
            .client
            .post(&url)
            .header("Git-Protocol", "version=2")
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-git-upload-pack-request",
            )
            .header(
                reqwest::header::ACCEPT,
                "application/x-git-upload-pack-result",
            )
            .body(body);
        let rb = self.auth_header(rb);
        let resp = rb
            .send()
            .await
            .map_err(|e| GitBridgeError::Http(format!("upload-pack request failed: {e}")))?;
        check_status(&resp)?;
        read_capped_progress(resp, self.max_body, on_bytes).await
    }
}

// ===========================================================================
// SSH transport (stateful subprocess)
// ===========================================================================

/// The `ssh` binary to spawn — `ASP_GIT_SSH` overrides the default `ssh` (tests use
/// a shim; a wrapper script is a valid production use too).
fn ssh_bin() -> String {
    std::env::var("ASP_GIT_SSH").unwrap_or_else(|_| "ssh".to_string())
}

/// Build the argument vector for `ssh` to run `<service>` against `url`'s path
/// (pure, so it is unit-testable). The remote command is a single argument so the
/// login shell runs `git-upload-pack '<path>'`; `-o SendEnv=GIT_PROTOCOL` carries the
/// v2 request (paired with `GIT_PROTOCOL=version=2` in the child env).
pub fn ssh_args(url: &GitUrl, service: Service) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-o".into(),
        "BatchMode=yes".into(),
        "-o".into(),
        "SendEnv=GIT_PROTOCOL".into(),
    ];
    if let GitUrl::Ssh { user, host, port, path } = url {
        if let Some(p) = port {
            args.push("-p".into());
            args.push(p.to_string());
        }
        let dest = match user {
            Some(u) => format!("{u}@{host}"),
            None => host.clone(),
        };
        args.push(dest);
        args.push(format!("{} '{}'", service.remote_command(), path));
    }
    args
}

/// Spawn `ssh`, write `request` to stdin, close it, and return the whole stdout.
/// For v2 upload-pack the server sends its capability advertisement first (proactively)
/// then reads our command — full-duplex, and both are tiny, so a write-then-drain
/// exchange cannot deadlock. Errors classify a missing `ssh` binary and a non-zero
/// exit with empty output.
fn ssh_run(url: &GitUrl, service: Service, request: &[u8], v2: bool) -> BridgeResult<Vec<u8>> {
    let mut cmd = Command::new(ssh_bin());
    cmd.args(ssh_args(url, service));
    if v2 {
        cmd.env("GIT_PROTOCOL", "version=2");
    }
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            GitBridgeError::Ssh(
                "the `ssh` binary was not found; use the repository's https:// URL instead".into(),
            )
        } else {
            GitBridgeError::Ssh(format!("failed to spawn ssh: {e}"))
        }
    })?;

    {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| GitBridgeError::Ssh("ssh stdin unavailable".into()))?;
        stdin
            .write_all(request)
            .map_err(|e| GitBridgeError::Ssh(format!("writing to ssh stdin: {e}")))?;
        // drop → EOF so the remote command finishes after answering.
    }

    let out = child
        .wait_with_output()
        .map_err(|e| GitBridgeError::Ssh(format!("waiting on ssh: {e}")))?;
    if !out.status.success() && out.stdout.is_empty() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(GitBridgeError::Ssh(format!(
            "ssh {} exited with {}: {}",
            service.remote_command(),
            out.status,
            stderr.trim()
        )));
    }
    Ok(out.stdout)
}

/// Byte length of the leading protocol-v2 capability advertisement (pkt-lines up to
/// and including the first flush). Errors if the stream is not framed / has no flush.
fn advertisement_len(bytes: &[u8]) -> BridgeResult<usize> {
    let mut reader = PktReader::new(bytes);
    for item in reader.by_ref() {
        if matches!(item?, Pkt::Flush) {
            return Ok(reader.offset());
        }
    }
    Err(GitBridgeError::Ssh(
        "ssh remote sent no protocol-v2 advertisement (does it support GIT_PROTOCOL=version=2? try https)".into(),
    ))
}

// ===========================================================================
// High-level ops
// ===========================================================================

/// The refs a remote advertises, plus its default branch (from `HEAD`'s symref).
#[derive(Debug, Clone)]
pub struct RemoteRefs {
    /// The short default-branch name (e.g. `main`), from the `HEAD` symref target.
    pub default_branch: Option<String>,
    /// Every advertised ref.
    pub refs: Vec<RefInfo>,
}

impl RemoteRefs {
    /// The oid the default branch points at, if resolvable.
    pub fn default_branch_oid(&self) -> Option<&str> {
        let name = self.default_branch.as_deref()?;
        let full = format!("refs/heads/{name}");
        self.refs
            .iter()
            .find(|r| r.name == full)
            .map(|r| r.oid.as_str())
    }
}

fn default_branch_from(refs: &[RefInfo]) -> Option<String> {
    refs.iter()
        .find(|r| r.name == "HEAD")
        .and_then(|r| r.symref_target.as_deref())
        .and_then(|t| t.strip_prefix("refs/heads/"))
        .map(|s| s.to_string())
}

/// `ls-remote`: list the remote's refs and default branch. Dispatches HTTPS through
/// [`GitTransport`], SSH through the spawned-`ssh` path.
pub async fn ls_remote(spec: &GitRemoteSpec) -> BridgeResult<RemoteRefs> {
    match &spec.url {
        GitUrl::Https { base } => {
            let t = HttpsTransport::new(base.clone(), spec.auth.clone())?;
            ls_remote_over(&t).await
        }
        GitUrl::Ssh { .. } => {
            let url = spec.url.clone();
            let refs = tokio::task::spawn_blocking(move || ssh_ls_remote(&url))
                .await
                .map_err(|e| GitBridgeError::Ssh(format!("ssh task join: {e}")))??;
            Ok(refs)
        }
    }
}

async fn ls_remote_over<T: GitTransport>(t: &T) -> BridgeResult<RemoteRefs> {
    let advert = t.info_refs(Service::UploadPack).await?;
    let caps = parse_capability_advertisement(&advert)?;
    caps.object_format()?; // reject sha256 up front
    // Empty prefix list → all refs (with symrefs + peel, per build_ls_refs).
    let body = build_ls_refs(&[]);
    let resp = t.upload_pack(body).await?;
    let refs = parse_ls_refs_response(&resp)?;
    Ok(RemoteRefs { default_branch: default_branch_from(&refs), refs })
}

fn ssh_ls_remote(url: &GitUrl) -> BridgeResult<RemoteRefs> {
    let out = ssh_run(url, Service::UploadPack, &build_ls_refs(&[]), true)?;
    let adv = advertisement_len(&out)?;
    // Validate the advertisement is really v2 (rejects a v0/v1 server clearly).
    parse_capability_advertisement(&out[..adv])?.object_format()?;
    let refs = parse_ls_refs_response(&out[adv..])?;
    Ok(RemoteRefs { default_branch: default_branch_from(&refs), refs })
}

/// The result of a single-round fetch.
#[derive(Debug, Clone)]
pub struct FetchOutcome {
    /// The raw packfile bytes (band-1 payload reassembled).
    pub pack: Vec<u8>,
    /// `shallow <oid>` boundary lines, if a depth was requested.
    pub shallow: Vec<String>,
}

/// Fetch a packfile in a single round (we send `done`, so the server returns a pack
/// immediately — one HTTP round trip, no multi-round negotiation; see the module
/// docs on the v1 simplification). We do **not** request a thin pack, so the returned
/// pack is self-contained and decodable without a base store (at the cost of possibly
/// transferring a little more — GitHub handles this fine).
///
/// Errors if the server sends no packfile section (`saw_packfile == false`).
pub async fn fetch_pack(
    spec: &GitRemoteSpec,
    wants: &[String],
    haves: &[String],
    depth: Option<u32>,
    mut on_bytes: impl FnMut(u64, u64),
) -> BridgeResult<FetchOutcome> {
    let req = FetchRequest {
        wants: wants.to_vec(),
        haves: haves.to_vec(),
        done: true,
        thin_pack: false,
        no_progress: false,
        filter: None,
        deepen: depth,
        shallow: Vec::new(),
    };
    let body = build_fetch(&req);

    let resp = match &spec.url {
        GitUrl::Https { base } => {
            let t = HttpsTransport::new(base.clone(), spec.auth.clone())?;
            t.upload_pack_progress(body, &mut on_bytes).await?
        }
        GitUrl::Ssh { .. } => {
            let url = spec.url.clone();
            let r = tokio::task::spawn_blocking(move || ssh_upload_pack(&url, &body))
                .await
                .map_err(|e| GitBridgeError::Ssh(format!("ssh task join: {e}")))??;
            // SSH reads the whole response before returning, so byte streaming isn't
            // available — report the final size once (as both done and total, since the
            // full length is now known) so the phase still shows a completed count.
            on_bytes(r.len() as u64, r.len() as u64);
            r
        }
    };

    // Band-2 progress text is still collected into `FetchResponse::progress`; the
    // clone driver drives its "fetching" bar from downloaded bytes (above) instead.
    let parsed = FetchResponseParser::parse_with(&resp, |_| {})?;
    if !parsed.saw_packfile {
        return Err(GitBridgeError::Remote(
            "server returned no packfile section for a done=true fetch".into(),
        ));
    }
    Ok(FetchOutcome { pack: parsed.pack, shallow: parsed.shallow })
}

/// SSH upload-pack round: read the v2 advert, then the fetch response (which follows
/// on the same pipe). Returns the response bytes *after* the advertisement so the
/// caller can hand them straight to [`FetchResponseParser`].
fn ssh_upload_pack(url: &GitUrl, fetch_body: &[u8]) -> BridgeResult<Vec<u8>> {
    let out = ssh_run(url, Service::UploadPack, fetch_body, true)?;
    let adv = advertisement_len(&out)?;
    parse_capability_advertisement(&out[..adv])?.object_format()?;
    Ok(out[adv..].to_vec())
}

/// The result of a push.
#[derive(Debug, Clone)]
pub struct PushOutcome {
    /// The ref that was updated.
    pub ref_name: String,
    /// The new oid the ref now points at.
    pub new_oid: String,
    /// True if the server accepted the update (`ok <ref>`).
    pub updated: bool,
}

/// Push a prepared pack to `ref_name`, updating it from `old_oid` to `new_oid`.
/// Negotiates capabilities from the receive-pack advertisement (report-status-v2 if
/// advertised, else report-status; side-band-64k and ofs-delta if advertised), builds
/// the update command, appends `pack`, and parses the report.
///
/// A `non-fast-forward` / stale-info rejection maps to [`GitBridgeError::NonFastForward`]
/// so the §5.2 retry loop can re-fetch and re-synthesize.
pub async fn push_pack(
    spec: &GitRemoteSpec,
    ref_name: &str,
    old_oid: &str,
    new_oid: &str,
    pack: Vec<u8>,
) -> BridgeResult<PushOutcome> {
    match &spec.url {
        GitUrl::Https { base } => {
            let t = HttpsTransport::new(base.clone(), spec.auth.clone())?;
            push_over(&t, ref_name, old_oid, new_oid, pack).await
        }
        GitUrl::Ssh { .. } => {
            let url = spec.url.clone();
            let ref_name = ref_name.to_string();
            let old = old_oid.to_string();
            let new = new_oid.to_string();
            tokio::task::spawn_blocking(move || ssh_push(&url, &ref_name, &old, &new, pack))
                .await
                .map_err(|e| GitBridgeError::Ssh(format!("ssh task join: {e}")))?
        }
    }
}

/// Choose the capabilities to request from an advertised receive-pack cap set, and
/// whether we negotiated side-band-64k (which decides how the report is framed).
fn negotiate_push_caps(advertised: &[String]) -> (Vec<String>, bool) {
    let has = |c: &str| advertised.iter().any(|a| a == c);
    let mut caps = Vec::new();
    if has("report-status-v2") {
        caps.push("report-status-v2".to_string());
    } else if has("report-status") {
        caps.push("report-status".to_string());
    }
    let sideband = has("side-band-64k");
    if sideband {
        caps.push("side-band-64k".to_string());
    }
    if has("ofs-delta") {
        caps.push("ofs-delta".to_string());
    }
    caps.push(format!("agent={AGENT}"));
    (caps, sideband)
}

/// Interpret a parsed report into a [`PushOutcome`] or a typed error.
fn interpret_report(
    report: crate::gitwire::PushReport,
    ref_name: &str,
    new_oid: &str,
) -> BridgeResult<PushOutcome> {
    if !report.unpack_ok {
        return Err(GitBridgeError::Remote("remote failed to unpack our pack".into()));
    }
    for (name, status) in &report.ref_statuses {
        if name == ref_name {
            return match status {
                Ok(()) => Ok(PushOutcome {
                    ref_name: ref_name.to_string(),
                    new_oid: new_oid.to_string(),
                    updated: true,
                }),
                Err(reason) => Err(classify_ref_rejection(reason)),
            };
        }
    }
    Err(GitBridgeError::Remote(format!(
        "remote report did not mention {ref_name}"
    )))
}

fn classify_ref_rejection(reason: &str) -> GitBridgeError {
    let low = reason.to_ascii_lowercase();
    if low.contains("non-fast-forward")
        || low.contains("fetch first")
        || low.contains("not a fast forward")
        || low.contains("stale info")
    {
        GitBridgeError::NonFastForward
    } else {
        GitBridgeError::Rejected(reason.to_string())
    }
}

async fn push_over<T: GitTransport>(
    t: &T,
    ref_name: &str,
    old_oid: &str,
    new_oid: &str,
    pack: Vec<u8>,
) -> BridgeResult<PushOutcome> {
    let advert = t.info_refs(Service::ReceivePack).await?;
    let advert = parse_receive_pack_advertisement(&advert)?;
    let (caps, sideband) = negotiate_push_caps(&advert.capabilities);
    let cap_refs: Vec<&str> = caps.iter().map(String::as_str).collect();

    let mut body = build_update_request(old_oid, new_oid, ref_name, &cap_refs);
    body.extend_from_slice(&pack);

    let resp = t.receive_pack(body).await?;
    let report = parse_report_status(&resp, sideband)?;
    interpret_report(report, ref_name, new_oid)
}

/// SSH push: read the v0 receive-pack advertisement first (we need its caps + the
/// unborn/ref state), then write the update command + pack, then read the report.
fn ssh_push(
    url: &GitUrl,
    ref_name: &str,
    old_oid: &str,
    new_oid: &str,
    pack: Vec<u8>,
) -> BridgeResult<PushOutcome> {
    // receive-pack is v0; we must read the advertisement before building the request,
    // so this is an interleaved (not write-then-drain) exchange.
    let mut cmd = Command::new(ssh_bin());
    cmd.args(ssh_args(url, Service::ReceivePack));
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            GitBridgeError::Ssh(
                "the `ssh` binary was not found; use the repository's https:// URL instead".into(),
            )
        } else {
            GitBridgeError::Ssh(format!("failed to spawn ssh: {e}"))
        }
    })?;

    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| GitBridgeError::Ssh("ssh stdout unavailable".into()))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| GitBridgeError::Ssh("ssh stdin unavailable".into()))?;

    let advert_bytes = read_pkts_until_flush(&mut stdout)?;
    let advert = parse_receive_pack_advertisement(&advert_bytes)?;
    let (caps, sideband) = negotiate_push_caps(&advert.capabilities);
    let cap_refs: Vec<&str> = caps.iter().map(String::as_str).collect();

    let mut body = build_update_request(old_oid, new_oid, ref_name, &cap_refs);
    body.extend_from_slice(&pack);
    stdin
        .write_all(&body)
        .map_err(|e| GitBridgeError::Ssh(format!("writing push to ssh: {e}")))?;
    drop(stdin);

    let mut report = Vec::new();
    stdout
        .read_to_end(&mut report)
        .map_err(|e| GitBridgeError::Ssh(format!("reading push report: {e}")))?;
    let _ = child.wait();

    let report = parse_report_status(&report, sideband)?;
    interpret_report(report, ref_name, new_oid)
}

/// Read framed pkt-lines from a blocking reader up to and including the first flush,
/// returning the raw bytes (framing preserved for the parser).
fn read_pkts_until_flush<R: Read>(reader: &mut R) -> BridgeResult<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let mut len_hex = [0u8; 4];
        reader
            .read_exact(&mut len_hex)
            .map_err(|e| GitBridgeError::Ssh(format!("short pkt-line length: {e}")))?;
        out.extend_from_slice(&len_hex);
        let hex = std::str::from_utf8(&len_hex)
            .ok()
            .and_then(|s| usize::from_str_radix(s, 16).ok())
            .ok_or_else(|| GitBridgeError::Ssh("invalid pkt-line length".into()))?;
        match hex {
            0 => return Ok(out), // flush
            1..=3 => continue,
            n if n >= 4 => {
                let mut buf = vec![0u8; n - 4];
                reader
                    .read_exact(&mut buf)
                    .map_err(|e| GitBridgeError::Ssh(format!("short pkt-line body: {e}")))?;
                out.extend_from_slice(&buf);
            }
            _ => unreachable!(),
        }
    }
}

// ===========================================================================
// Pack writer (reused by commit synthesis)
// ===========================================================================

/// A git object kind, with its packfile type number and loose-header string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitObjectKind {
    /// A commit object.
    Commit,
    /// A tree object.
    Tree,
    /// A blob object.
    Blob,
    /// An (annotated) tag object.
    Tag,
}

impl GitObjectKind {
    /// The loose/type header string (`commit`/`tree`/`blob`/`tag`).
    pub fn type_str(self) -> &'static str {
        match self {
            GitObjectKind::Commit => "commit",
            GitObjectKind::Tree => "tree",
            GitObjectKind::Blob => "blob",
            GitObjectKind::Tag => "tag",
        }
    }
    /// The packfile object type number (1=commit, 2=tree, 3=blob, 4=tag).
    pub fn pack_type(self) -> u8 {
        match self {
            GitObjectKind::Commit => 1,
            GitObjectKind::Tree => 2,
            GitObjectKind::Blob => 3,
            GitObjectKind::Tag => 4,
        }
    }
}

/// Map a bridge object kind to the import layer's kind (for feeding a [`GitObjectDb`]).
///
/// [`GitObjectDb`]: crate::gitimport::GitObjectDb
fn import_obj_kind(k: GitObjectKind) -> crate::gitimport::GitObjKind {
    use crate::gitimport::GitObjKind;
    match k {
        GitObjectKind::Commit => GitObjKind::Commit,
        GitObjectKind::Tree => GitObjKind::Tree,
        GitObjectKind::Blob => GitObjKind::Blob,
        GitObjectKind::Tag => GitObjKind::Tag,
    }
}

/// The inverse of [`import_obj_kind`].
fn export_obj_kind(k: crate::gitimport::GitObjKind) -> GitObjectKind {
    use crate::gitimport::GitObjKind;
    match k {
        GitObjKind::Commit => GitObjectKind::Commit,
        GitObjKind::Tree => GitObjectKind::Tree,
        GitObjKind::Blob => GitObjectKind::Blob,
        GitObjKind::Tag => GitObjectKind::Tag,
    }
}

/// The framed bytes git hashes for an object: `<type> <len>\0<content>`.
fn frame_object(kind: GitObjectKind, content: &[u8]) -> Vec<u8> {
    let mut out = format!("{} {}\0", kind.type_str(), content.len()).into_bytes();
    out.extend_from_slice(content);
    out
}

/// The 20-byte git object id (SHA-1 of the framed object).
pub fn git_oid_bytes(kind: GitObjectKind, content: &[u8]) -> [u8; 20] {
    let mut h = Sha1::new();
    h.update(frame_object(kind, content));
    let d = h.finalize();
    let mut a = [0u8; 20];
    a.copy_from_slice(&d);
    a
}

/// The 40-hex git object id of `content` framed as `kind`. Handy for building
/// commits/trees that reference other objects by oid.
pub fn git_oid(kind: GitObjectKind, content: &[u8]) -> String {
    hex::encode(git_oid_bytes(kind, content))
}

/// Encode a **non-delta** packfile from `objects` (each the *uncompressed* object
/// content, not framed): `"PACK"` + version 2 + object count, then per object a
/// variable-length type/size header followed by its zlib stream, then a trailing
/// SHA-1 over all preceding bytes.
///
/// # Contract (the synthesis slice depends on this)
/// * Objects are stored verbatim — **no delta compression** — so the writer never
///   needs a base store and the output is always a self-contained (non-thin) pack.
/// * `content` is the raw object bytes (e.g. a commit body, a tree's entry table, a
///   blob's file bytes); do **not** pre-frame it with `<type> <len>\0` — the pack
///   header already carries type + size, and git re-derives the oid from the frame.
/// * Ordering is preserved as given; for a valid fetch/push a consumer only needs the
///   set to be closed under reference, not any particular order.
/// * The result round-trips through the `gix-pack` decoder (see [`RemoteStore::record_fetch`]).
pub fn write_pack(objects: &[(GitObjectKind, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"PACK");
    out.extend_from_slice(&2u32.to_be_bytes());
    out.extend_from_slice(&(objects.len() as u32).to_be_bytes());

    for (kind, content) in objects {
        // Variable-length header: first byte = (type << 4) | (size & 0x0f), then
        // 7-bit little-endian continuation groups for the rest of the size.
        let mut size = content.len();
        let mut byte = (kind.pack_type() << 4) | ((size & 0x0f) as u8);
        size >>= 4;
        while size > 0 {
            out.push(byte | 0x80);
            byte = (size & 0x7f) as u8;
            size >>= 7;
        }
        out.push(byte);

        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        enc.write_all(content).expect("zlib write to Vec is infallible");
        let compressed = enc.finish().expect("zlib finish to Vec is infallible");
        out.extend_from_slice(&compressed);
    }

    let mut h = Sha1::new();
    h.update(&out);
    out.extend_from_slice(&h.finalize());
    out
}

/// Number of objects a packfile claims in its header (`pack[8..12]`, big-endian).
fn pack_object_count(pack: &[u8]) -> Option<u32> {
    if pack.len() < 12 || &pack[0..4] != b"PACK" {
        return None;
    }
    Some(u32::from_be_bytes([pack[8], pack[9], pack[10], pack[11]]))
}

/// Cheaply verify a fetched packfile's structural integrity **before** it is persisted:
/// the `"PACK"` magic, a supported version, and — the load-bearing check — that the
/// trailing 20 bytes equal the SHA-1 of everything preceding them (git's pack trailer).
///
/// This is the only integrity gate a fetched pack gets before its refs are recorded, so
/// it must catch the two ways a pack arrives broken: a **truncated** transfer (the
/// pkt-line parser accepts a framed-but-short pack, returning `Ok` on a mid-stream flush)
/// and a **bit-flipped** body. Both fail the trailer comparison (a truncation loses the
/// real trailer; a flip changes the digest), so `record_fetch` can reject them and leave
/// `refs.json` + `packs/` untouched instead of wedging every future pull on a bad pack
/// that only fails much later, deep inside `build_db`.
fn verify_pack_integrity(pack: &[u8]) -> BridgeResult<()> {
    // 12-byte header + 20-byte SHA-1 trailer is the minimum a real pack can be.
    if pack.len() < 32 {
        return Err(GitBridgeError::Store(format!(
            "fetched pack is too short to be valid ({} bytes)",
            pack.len()
        )));
    }
    if &pack[0..4] != b"PACK" {
        return Err(GitBridgeError::Store(
            "fetched pack missing PACK magic (corrupt or truncated transfer)".into(),
        ));
    }
    let version = u32::from_be_bytes([pack[4], pack[5], pack[6], pack[7]]);
    if version != 2 && version != 3 {
        return Err(GitBridgeError::Store(format!(
            "fetched pack has unsupported version {version}"
        )));
    }
    let (body, trailer) = pack.split_at(pack.len() - 20);
    let mut h = Sha1::new();
    h.update(body);
    let digest = h.finalize();
    if digest.as_slice() != trailer {
        return Err(GitBridgeError::Store(
            "fetched pack checksum mismatch (truncated or corrupt transfer) — refusing to store".into(),
        ));
    }
    Ok(())
}

// ===========================================================================
// Local bare object store (.asp/gitremote/<remote_id>/)
// ===========================================================================


/// Derive the stable per-remote id: `hex(sha256("asp-git-remote/v1" || normalized_url))[..16]`.
/// Normalization lowercases and strips a trailing `/` and `.git` so `…/repo`,
/// `…/repo/`, and `…/repo.git` share one store.
pub fn remote_id(url: &str) -> String {
    let mut n = url.trim().to_ascii_lowercase();
    while let Some(s) = n.strip_suffix('/') {
        n = s.to_string();
    }
    if let Some(s) = n.strip_suffix(".git") {
        n = s.to_string();
    }
    let mut h = Sha256::new();
    h.update(b"asp-git-remote/v1");
    h.update(n.as_bytes());
    hex::encode(h.finalize())[..16].to_string()
}

/// The engine-owned local git object store under `.asp/gitremote/<remote_id>/` (§6.3).
///
/// ## On-disk layout
/// ```text
/// .asp/gitremote/<remote_id>/
///   packs/         raw fetched packs, named `<seq>-<sha>.pack` in fetch order —
///                  the durable form for every fetched commit/tree/blob/tag
///   objects/       loose objects (git `xx/yyy…`): objects push synthesis authors,
///                  plus any left over from an older (pre-pack) store (back-compat)
///   refs.json      { full_ref_name: oid } snapshot of the last-fetched ref state
/// ```
/// **Verbatim packs, decode-on-read.** The spec's preferred "store the pack verbatim
/// with a gix-pack index" path can't build an index here: the pinned `gix-pack`
/// feature set enables `wasm` (mandatory for the shared wasm build), which compiles
/// out gix-pack's on-disk index-pack (`bundle::write`, gated `not(wasm)`). But we
/// don't need an index. `record_fetch` writes each pack's raw bytes as one file (one
/// inode per fetch instead of one per object), and random access
/// ([`get_object`]/[`has`]/[`is_ancestor`]) lazily rebuilds an in-memory object db by
/// decoding the stored packs in fetch order (thin incremental packs resolve against
/// the accumulating db, then any loose objects). The clone critical path never touches
/// that lazy db (it decodes the fetched pack once for import directly); only
/// `pull`/`push` do, and they already hold the full DAG in memory. Back-compat: a store
/// written by the old exploding path (loose objects, no `packs/`) still reads correctly.
///
/// [`get_object`]: RemoteStore::get_object
/// [`has`]: RemoteStore::has
/// [`is_ancestor`]: RemoteStore::is_ancestor
pub struct RemoteStore {
    root: PathBuf,
    refs: BTreeMap<String, String>,
    /// Lazily-decoded object db over `packs/` + `objects/`, built on first random
    /// access and invalidated whenever a pack or loose object is written. `RefCell`
    /// (not a lock): a `RemoteStore` is used single-threaded; it is `Send` (so it may
    /// be held across an `.await` in the push driver) but never `Sync`.
    cache: std::cell::RefCell<Option<crate::gitimport::GitObjectDb>>,
    /// On-disk content-addressed scratch that [`build_db`](RemoteStore::build_db) spills
    /// blob bytes into (keyed by `content_hash`), so a full-history pack decode keeps only
    /// commits/trees + byte-free locators in RAM instead of every decompressed blob (the
    /// pull/push OOM). A temp dir **on the store's own filesystem** (real disk, not a
    /// possibly-tmpfs system temp — spilling to RAM would defeat the fix), auto-removed
    /// when this `RemoteStore` drops. Purely a per-access cache; the packs stay authoritative.
    _spill_dir: tempfile::TempDir,
    blob_spill: FsBlobStore,
}

/// A minimal on-disk, content-addressed [`BlobStore`](crate::store::BlobStore): each blob
/// is one file at `<root>/<hash[..2]>/<hash[2..]>` (sha256 `content_hash` key, sharded by
/// the first byte). Used only as the [`RemoteStore`] blob spill target — an idempotent,
/// dedup-by-existence sink the pack decode streams blobs into so they leave RAM.
struct FsBlobStore {
    root: PathBuf,
}

impl FsBlobStore {
    fn new(root: PathBuf) -> Self {
        FsBlobStore { root }
    }

    fn blob_path(&self, hash: &str) -> PathBuf {
        if hash.len() >= 2 {
            self.root.join(&hash[..2]).join(&hash[2..])
        } else {
            self.root.join(hash)
        }
    }
}

impl crate::store::BlobStore for FsBlobStore {
    fn put_blob(&self, bytes: &[u8]) -> crate::error::AspResult<String> {
        let h = crate::oid::content_hash(bytes);
        self.put_blob_with_hash(&h, bytes)?;
        Ok(h)
    }
    fn get_blob(&self, hash: &str) -> crate::error::AspResult<Option<Vec<u8>>> {
        match std::fs::read(self.blob_path(hash)) {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
    fn has_blob(&self, hash: &str) -> crate::error::AspResult<bool> {
        Ok(self.blob_path(hash).exists())
    }
    fn put_blob_with_hash(&self, hash: &str, bytes: &[u8]) -> crate::error::AspResult<()> {
        let path = self.blob_path(hash);
        if path.exists() {
            return Ok(()); // content-addressed: identical bytes, skip the rewrite
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, bytes)?;
        Ok(())
    }
}

impl RemoteStore {
    /// Open (creating the layout if absent) the store for `remote_id` under `asp_dir`
    /// (the `.asp` directory). Loads the ref snapshot; objects are read lazily.
    pub fn open(asp_dir: &Path, remote_id: &str) -> BridgeResult<Self> {
        let root = asp_dir.join("gitremote").join(remote_id);
        std::fs::create_dir_all(root.join("objects"))?;

        let refs = match std::fs::read(root.join("refs.json")) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
            Err(_) => BTreeMap::new(),
        };

        // Blob spill scratch on the store's own filesystem (see field docs). Created under
        // `root` so it shares the vault's real disk, and auto-removed on drop.
        let spill_dir = tempfile::Builder::new()
            .prefix("blobscratch-")
            .tempdir_in(&root)?;
        let blob_spill = FsBlobStore::new(spill_dir.path().to_path_buf());

        Ok(Self {
            root,
            refs,
            cache: std::cell::RefCell::new(None),
            _spill_dir: spill_dir,
            blob_spill,
        })
    }

    /// The blob spill store `build_db` streams decoded blob bytes into (keyed by
    /// `content_hash`). A caller that holds the built db can read a spilled blob's bytes
    /// back through this using [`GitObjectDb::spilled_content_hash`] — the pull driver's
    /// ingest blob source does exactly that.
    ///
    /// [`GitObjectDb::spilled_content_hash`]: crate::gitimport::GitObjectDb::spilled_content_hash
    pub fn spill_store(&self) -> &dyn crate::store::BlobStore {
        &self.blob_spill
    }

    /// The store's root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Store a freshly fetched pack verbatim and record the ref state it corresponds
    /// to. An empty pack (0 objects, e.g. a have-covers-everything fetch) is a no-op
    /// for objects; the refs are still updated. Invalidates the lazy object db so the
    /// next random access sees the new pack.
    pub fn record_fetch(&mut self, pack: &[u8], refs: &[(String, String)]) -> BridgeResult<()> {
        if pack_object_count(pack).unwrap_or(0) > 0 {
            // Integrity gate BEFORE any persistence: a truncated/corrupt pack errors here
            // so neither the pack nor the advanced refs are written — the fetch fails
            // cleanly and a retry re-fetches, rather than wedging every future pull on a
            // bad pack that would only blow up later inside `build_db`.
            verify_pack_integrity(pack)?;
            self.store_pack(pack)?;
        }
        for (name, oid) in refs {
            self.refs.insert(name.clone(), oid.clone());
        }
        self.persist_refs()?;
        *self.cache.borrow_mut() = None;
        Ok(())
    }

    fn packs_dir(&self) -> PathBuf {
        self.root.join("packs")
    }

    /// Write `pack`'s raw bytes as the next pack in fetch order (`<seq>-<sha>.pack`).
    /// `seq` (zero-padded) preserves fetch order for decode; `<sha>` is the pack's own
    /// trailing checksum, purely for human-legible uniqueness.
    fn store_pack(&self, pack: &[u8]) -> BridgeResult<()> {
        let dir = self.packs_dir();
        std::fs::create_dir_all(&dir)?;
        let seq = self.next_pack_seq()?;
        let id = if pack.len() >= 20 {
            hex::encode(&pack[pack.len() - 20..])
        } else {
            "short".to_string()
        };
        let name = format!("{seq:08}-{id}.pack");
        std::fs::write(dir.join(name), pack)?;
        Ok(())
    }

    /// The stored pack paths in fetch (decode) order.
    fn pack_paths(&self) -> Vec<PathBuf> {
        let mut packs: Vec<(u64, PathBuf)> = Vec::new();
        let Ok(entries) = std::fs::read_dir(self.packs_dir()) else {
            return Vec::new();
        };
        for e in entries.flatten() {
            let path = e.path();
            if path.extension().and_then(|s| s.to_str()) != Some("pack") {
                continue;
            }
            let seq = path
                .file_name()
                .and_then(|s| s.to_str())
                .and_then(|s| s.split('-').next())
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);
            packs.push((seq, path));
        }
        packs.sort_by_key(|(seq, _)| *seq);
        packs.into_iter().map(|(_, p)| p).collect()
    }

    /// One past the highest existing pack `seq` (0 for a fresh store).
    fn next_pack_seq(&self) -> BridgeResult<u64> {
        let next = self
            .pack_paths()
            .last()
            .and_then(|p| p.file_name())
            .and_then(|s| s.to_str())
            .and_then(|s| s.split('-').next())
            .and_then(|s| s.parse::<u64>().ok())
            .map(|s| s + 1)
            .unwrap_or(0);
        Ok(next)
    }

    /// Borrow the lazily-decoded object db (built from `packs/` in fetch order, then
    /// `objects/` loose objects), building it on first access. A decode error is
    /// surfaced; callers that return `Option`/`bool` degrade a build failure to
    /// "absent" (a torn store already has bigger problems).
    fn object_db(&self) -> BridgeResult<std::cell::Ref<'_, crate::gitimport::GitObjectDb>> {
        if self.cache.borrow().is_none() {
            let db = self.build_db()?;
            *self.cache.borrow_mut() = Some(db);
        }
        Ok(std::cell::Ref::map(self.cache.borrow(), |o| o.as_ref().expect("db just built")))
    }

    /// Decode every stored pack (fetch order) plus every loose object into a fresh
    /// [`GitObjectDb`]. Loose objects load first so an incremental pack that deltas
    /// against a push-authored (loose) base resolves; packs then accumulate into the
    /// same db so a thin pack resolves against earlier packs. This is the durable
    /// rebuild the pull driver uses (replacing per-object loose reads).
    ///
    /// [`GitObjectDb`]: crate::gitimport::GitObjectDb
    pub fn build_db(&self) -> BridgeResult<crate::gitimport::GitObjectDb> {
        use crate::gitimport::{no_base_lookup, GitObjectDb};
        let mut db = GitObjectDb::new();
        // Loose objects first (push-authored + legacy pre-pack stores).
        let objects = self.root.join("objects");
        if let Ok(shards) = std::fs::read_dir(&objects) {
            for shard in shards.flatten() {
                let prefix = shard.file_name().to_string_lossy().to_string();
                if prefix.len() != 2 {
                    continue;
                }
                let Ok(files) = std::fs::read_dir(shard.path()) else { continue };
                for f in files.flatten() {
                    let rest = f.file_name().to_string_lossy().to_string();
                    let sha = format!("{prefix}{rest}");
                    if sha.len() != 40 {
                        continue;
                    }
                    if let Some((kind, body)) = self.read_loose(&sha) {
                        db.insert_loose(import_obj_kind(kind), &body)
                            .map_err(|e| GitBridgeError::Store(format!("loose object {sha}: {e}")))?;
                    }
                }
            }
        }
        // Packs in fetch order (thin packs resolve against the accumulating db). Each
        // pack's bytes move straight into the decoder (no second full-pack copy), and its
        // blob bodies **spill onto disk** (into `blob_spill`) instead of piling into RAM —
        // so decoding a full-history store keeps only commits/trees + byte-free blob
        // locators resident. Consumers that need a blob back (pull's ingest, the unit
        // `get_object`) read it from `blob_spill` via the locator's `content_hash`.
        for path in self.pack_paths() {
            let bytes = std::fs::read(&path)
                .map_err(|e| GitBridgeError::Store(format!("reading pack {}: {e}", path.display())))?;
            if let Err(e) = db.absorb_pack_spilling(bytes, no_base_lookup, &self.blob_spill) {
                // Self-heal: a stored pack that passed the fetch-time trailer check but
                // still fails to decode (a rare semantic corruption a checksum can't
                // catch) would otherwise wedge every future pull AND push, since build_db
                // decodes all packs on every access. Quarantine it (rename `.pack` →
                // `.bad`, so `pack_paths` skips it) and surface an actionable error; a
                // subsequent pull re-fetches the missing objects instead of hitting the
                // same wall.
                let bad = path.with_extension("bad");
                let _ = std::fs::rename(&path, &bad);
                return Err(GitBridgeError::Store(format!(
                    "decoding stored pack {} failed ({e}); quarantined it as {} — re-run the pull to re-fetch",
                    path.display(),
                    bad.display()
                )));
            }
        }
        Ok(db)
    }

    fn persist_refs(&self) -> BridgeResult<()> {
        let bytes = serde_json::to_vec(&self.refs)
            .map_err(|e| GitBridgeError::Store(format!("serializing refs: {e}")))?;
        std::fs::write(self.root.join("refs.json"), bytes)?;
        Ok(())
    }

    /// The last-recorded ref snapshot (`full_ref_name` → oid).
    pub fn refs(&self) -> &BTreeMap<String, String> {
        &self.refs
    }

    /// Total number of objects held across all stored packs + loose objects (used by
    /// tests to check a fetch decoded the expected object set). Builds the lazy db.
    pub fn object_count(&self) -> u32 {
        // Spilled blobs live in `blob_meta` (their bytes are on disk), not `objects`, so
        // count both to report the full object set.
        self.object_db().map(|db| (db.len() + db.blob_count()) as u32).unwrap_or(0)
    }

    /// Whether object `sha` (40-hex) is present in any stored pack or as loose. Counts
    /// spilled blobs (a byte-free locator in the db, bytes on disk) — push dedup probes
    /// blob presence here, so a spilled blob must read as present.
    pub fn has(&self, sha: &str) -> bool {
        sha.len() == 40 && self.object_db().map(|db| db.contains(sha)).unwrap_or(false)
    }

    /// Fetch object `sha` (40-hex): its kind and *content* bytes (unframed). A spilled
    /// blob (bytes not in the db) is read back from the blob spill store via its locator.
    pub fn get_object(&self, sha: &str) -> Option<(GitObjectKind, Vec<u8>)> {
        let db = self.object_db().ok()?;
        if let Some((kind, body)) = db.get(sha) {
            return Some((export_obj_kind(kind), body.to_vec()));
        }
        // Spilled blob: recover its bytes from the on-disk spill store.
        let content_hash = db.spilled_content_hash(sha)?;
        let bytes = self.blob_spill.get_blob(content_hash).ok().flatten()?;
        Some((GitObjectKind::Blob, bytes))
    }

    /// Write a loose object (used by push synthesis to stage authored objects) and
    /// return its 40-hex oid. Invalidates the lazy db so a subsequent read sees it.
    pub fn write_loose_object(&self, kind: GitObjectKind, content: &[u8]) -> BridgeResult<String> {
        let framed = frame_object(kind, content);
        let sha = hex::encode(git_oid_bytes(kind, content));
        let path = self.loose_path(&sha);
        if !path.exists() {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
            enc.write_all(&framed)?;
            let compressed = enc.finish()?;
            std::fs::write(&path, compressed)?;
        }
        *self.cache.borrow_mut() = None;
        Ok(sha)
    }

    fn loose_path(&self, sha: &str) -> PathBuf {
        self.root.join("objects").join(&sha[..2]).join(&sha[2..])
    }

    fn read_loose(&self, sha: &str) -> Option<(GitObjectKind, Vec<u8>)> {
        if sha.len() != 40 {
            return None;
        }
        let bytes = std::fs::read(self.loose_path(sha)).ok()?;
        let mut dec = flate2::read::ZlibDecoder::new(&bytes[..]);
        let mut raw = Vec::new();
        dec.read_to_end(&mut raw).ok()?;
        let nul = raw.iter().position(|&b| b == 0)?;
        let header = std::str::from_utf8(&raw[..nul]).ok()?;
        let kind = match header.split(' ').next()? {
            "commit" => GitObjectKind::Commit,
            "tree" => GitObjectKind::Tree,
            "blob" => GitObjectKind::Blob,
            "tag" => GitObjectKind::Tag,
            _ => return None,
        };
        Some((kind, raw[nul + 1..].to_vec()))
    }

    /// Is `ancestor` an ancestor of (or equal to) `descendant`? Walks commit parents
    /// from `descendant` using stored objects. Powers force-push detection (§4.4) and
    /// push base selection. Returns `Ok(false)` if either commit is absent or a parent
    /// is missing (a shallow boundary), never an error for missing objects.
    pub fn is_ancestor(&self, ancestor: &str, descendant: &str) -> BridgeResult<bool> {
        if ancestor == descendant {
            return Ok(true);
        }
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut stack = vec![descendant.to_string()];
        while let Some(cur) = stack.pop() {
            if !seen.insert(cur.clone()) {
                continue;
            }
            let Some((kind, content)) = self.get_object(&cur) else {
                continue;
            };
            if kind != GitObjectKind::Commit {
                continue;
            }
            for parent in commit_parents(&content) {
                if parent == ancestor {
                    return Ok(true);
                }
                stack.push(parent);
            }
        }
        Ok(false)
    }
}

/// Parse the `parent <sha>` lines out of a commit object's content (the header block
/// before the first blank line). Shared by [`RemoteStore::is_ancestor`] and
/// `gitremote::db_is_ancestor` (force-push detection) so both walk parents identically.
pub(crate) fn commit_parents(content: &[u8]) -> Vec<String> {
    let mut parents = Vec::new();
    for line in content.split(|&b| b == b'\n') {
        if line.is_empty() {
            break; // end of header block
        }
        if let Some(rest) = line.strip_prefix(b"parent ") {
            if let Ok(sha) = std::str::from_utf8(rest) {
                parents.push(sha.trim().to_string());
            }
        }
    }
    parents
}

// ===========================================================================
// Unit tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gitwire::GitUrl;

    #[test]
    fn detect_source_git_vs_peer() {
        assert!(matches!(
            detect_source("https://github.com/o/r"),
            SourceKind::GitUrl(GitUrl::Https { .. })
        ));
        assert!(matches!(
            detect_source("git@github.com:o/r.git"),
            SourceKind::GitUrl(GitUrl::Ssh { .. })
        ));
        // A 64-hex node id and a bare path are peers, not git URLs.
        assert_eq!(
            detect_source("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"),
            SourceKind::Peer
        );
        assert_eq!(detect_source("/home/chris/repo"), SourceKind::Peer);
    }

    #[test]
    fn remote_id_normalizes_and_is_stable() {
        let a = remote_id("https://github.com/o/r");
        let b = remote_id("https://github.com/o/r.git");
        let c = remote_id("https://github.com/o/r/");
        let d = remote_id("https://GitHub.com/o/r");
        assert_eq!(a, b);
        assert_eq!(a, c);
        assert_eq!(a, d);
        assert_eq!(a.len(), 16);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, remote_id("https://github.com/o/other"));
    }

    #[test]
    fn ssh_args_https_forms() {
        let url = GitUrl::Ssh {
            user: Some("git".into()),
            host: "github.com".into(),
            port: None,
            path: "owner/repo.git".into(),
        };
        let args = ssh_args(&url, Service::UploadPack);
        assert_eq!(
            args,
            vec![
                "-o",
                "BatchMode=yes",
                "-o",
                "SendEnv=GIT_PROTOCOL",
                "git@github.com",
                "git-upload-pack 'owner/repo.git'",
            ]
        );

        let url_port = GitUrl::Ssh {
            user: None,
            host: "example.com".into(),
            port: Some(2222),
            path: "/srv/repo".into(),
        };
        let args = ssh_args(&url_port, Service::ReceivePack);
        assert_eq!(
            args,
            vec![
                "-o",
                "BatchMode=yes",
                "-o",
                "SendEnv=GIT_PROTOCOL",
                "-p",
                "2222",
                "example.com",
                "git-receive-pack '/srv/repo'",
            ]
        );
    }

    #[test]
    fn write_pack_roundtrips_through_gix_pack() {
        // Build a blob, a tree referencing it, and a commit referencing the tree.
        let blob = b"hello asp\n".to_vec();
        let blob_oid = git_oid_bytes(GitObjectKind::Blob, &blob);

        let mut tree = Vec::new();
        tree.extend_from_slice(b"100644 file.txt\0");
        tree.extend_from_slice(&blob_oid);
        let tree_oid = git_oid(GitObjectKind::Tree, &tree);

        let commit = format!(
            "tree {tree_oid}\nauthor A <a@x> 1700000000 +0000\ncommitter A <a@x> 1700000000 +0000\n\nmsg\n"
        )
        .into_bytes();
        let commit_oid = git_oid(GitObjectKind::Commit, &commit);

        let pack = write_pack(&[
            (GitObjectKind::Commit, commit.clone()),
            (GitObjectKind::Tree, tree.clone()),
            (GitObjectKind::Blob, blob.clone()),
        ]);
        assert_eq!(&pack[0..4], b"PACK");
        assert_eq!(pack_object_count(&pack), Some(3));

        // Decode it through the same path record_fetch uses and read every object back.
        let tmp = tempfile::tempdir().unwrap();
        let mut store = RemoteStore::open(tmp.path(), "unit").unwrap();
        store
            .record_fetch(&pack, &[("refs/heads/main".into(), commit_oid.clone())])
            .unwrap();
        assert_eq!(store.object_count(), 3);

        assert!(store.has(&commit_oid));
        assert!(store.has(&tree_oid));
        let (k, got) = store.get_object(&blob_oid_hex(&blob)).unwrap();
        assert_eq!(k, GitObjectKind::Blob);
        assert_eq!(got, blob);
        let (k, got) = store.get_object(&commit_oid).unwrap();
        assert_eq!(k, GitObjectKind::Commit);
        assert_eq!(got, commit);
    }

    fn blob_oid_hex(content: &[u8]) -> String {
        git_oid(GitObjectKind::Blob, content)
    }

    /// A minimal valid pack (one blob) plus the commit oid its refs would point at.
    fn sample_pack() -> (Vec<u8>, String) {
        let blob = b"hello asp\n".to_vec();
        let pack = write_pack(&[(GitObjectKind::Blob, blob.clone())]);
        (pack, blob_oid_hex(&blob))
    }

    #[test]
    fn truncated_pack_rejected_and_store_untouched() {
        let (pack, _) = sample_pack();
        // Drop the trailing bytes (incl. the SHA-1 trailer) — exactly what the pkt parser
        // hands back for a transfer that flushed mid-pack.
        let truncated = pack[..pack.len() - 8].to_vec();

        let tmp = tempfile::tempdir().unwrap();
        let mut store = RemoteStore::open(tmp.path(), "trunc").unwrap();
        let err = store
            .record_fetch(&truncated, &[("refs/heads/main".into(), "a".repeat(40))])
            .unwrap_err();
        assert!(matches!(err, GitBridgeError::Store(_)), "expected Store error, got {err:?}");

        // Nothing persisted: no refs.json, no packs, empty object db, and the in-memory
        // ref snapshot never advanced.
        assert!(!store.root().join("refs.json").exists(), "refs.json must not be written");
        assert!(store.refs().is_empty(), "refs must not advance");
        assert_eq!(store.object_count(), 0, "no objects should be stored");
        let pack_count = std::fs::read_dir(store.root().join("packs"))
            .map(|d| d.flatten().filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("pack")).count())
            .unwrap_or(0);
        assert_eq!(pack_count, 0, "no pack file should be written");
    }

    #[test]
    fn bitflipped_pack_rejected_and_store_untouched() {
        let (mut pack, _) = sample_pack();
        // Flip a bit in the compressed object body (not the trailer) — the trailer no
        // longer matches the recomputed SHA-1.
        let mid = pack.len() / 2;
        pack[mid] ^= 0x40;

        let tmp = tempfile::tempdir().unwrap();
        let mut store = RemoteStore::open(tmp.path(), "flip").unwrap();
        let err = store
            .record_fetch(&pack, &[("refs/heads/main".into(), "b".repeat(40))])
            .unwrap_err();
        assert!(matches!(err, GitBridgeError::Store(_)), "expected Store error, got {err:?}");

        assert!(!store.root().join("refs.json").exists(), "refs.json must not be written");
        assert!(store.refs().is_empty(), "refs must not advance");
        assert_eq!(store.object_count(), 0, "no objects should be stored");
    }

    #[test]
    fn valid_pack_passes_integrity_check() {
        let (pack, blob_oid) = sample_pack();
        let tmp = tempfile::tempdir().unwrap();
        let mut store = RemoteStore::open(tmp.path(), "ok").unwrap();
        store
            .record_fetch(&pack, &[("refs/heads/main".into(), blob_oid.clone())])
            .unwrap();
        assert!(store.has(&blob_oid));
        assert_eq!(store.refs().get("refs/heads/main"), Some(&blob_oid));
    }

    #[test]
    fn build_db_spills_blob_bytes_out_of_ram() {
        // Memory-boundedness (finding 1), structurally: after build_db, a blob's
        // decompressed bytes must NOT sit in the in-memory object map — only commits and
        // trees do; the blob is a byte-free locator whose bytes live on disk. get_object
        // and has() still recover / report it (read back from the spill).
        let blob = vec![b'x'; 1 << 16]; // 64 KiB — dwarfs the commit+tree
        let blob_oid = git_oid_bytes(GitObjectKind::Blob, &blob);
        let mut tree = Vec::new();
        tree.extend_from_slice(b"100644 file.bin\0");
        tree.extend_from_slice(&blob_oid);
        let tree_oid = git_oid(GitObjectKind::Tree, &tree);
        let commit = format!(
            "tree {tree_oid}\nauthor A <a@x> 1700000000 +0000\ncommitter A <a@x> 1700000000 +0000\n\nm\n"
        )
        .into_bytes();
        let commit_oid = git_oid(GitObjectKind::Commit, &commit);
        let blob_hex = git_oid(GitObjectKind::Blob, &blob);

        let pack = write_pack(&[
            (GitObjectKind::Commit, commit.clone()),
            (GitObjectKind::Tree, tree.clone()),
            (GitObjectKind::Blob, blob.clone()),
        ]);

        let tmp = tempfile::tempdir().unwrap();
        let mut store = RemoteStore::open(tmp.path(), "spill").unwrap();
        store
            .record_fetch(&pack, &[("refs/heads/main".into(), commit_oid.clone())])
            .unwrap();

        let db = store.build_db().unwrap();
        // Commits + trees stay resident; the blob does not.
        assert_eq!(db.len(), 2, "only commit + tree should be in the RAM object map");
        assert_eq!(db.blob_count(), 1, "the blob should be a spilled locator");
        assert!(db.get(&blob_hex).is_none(), "blob bytes must not be held in RAM");
        assert!(db.contains(&blob_hex), "blob must still register as present");
        // …but the bytes are recoverable on demand from the on-disk spill.
        let (k, got) = store.get_object(&blob_hex).unwrap();
        assert_eq!(k, GitObjectKind::Blob);
        assert_eq!(got, blob, "spilled blob round-trips byte-for-byte");
        assert!(store.has(&blob_hex) && store.has(&tree_oid) && store.has(&commit_oid));
        assert_eq!(store.object_count(), 3);
    }

    #[test]
    fn is_ancestor_walks_commit_parents() {
        // root <- c1 <- c2 (linear); build minimal commits with no trees needed for
        // ancestry (get_object only reads parent lines).
        let empty_tree = git_oid(GitObjectKind::Tree, &[]);
        let root = format!(
            "tree {empty_tree}\nauthor A <a@x> 1 +0000\ncommitter A <a@x> 1 +0000\n\nroot\n"
        )
        .into_bytes();
        let root_oid = git_oid(GitObjectKind::Commit, &root);
        let c1 = format!(
            "tree {empty_tree}\nparent {root_oid}\nauthor A <a@x> 2 +0000\ncommitter A <a@x> 2 +0000\n\nc1\n"
        )
        .into_bytes();
        let c1_oid = git_oid(GitObjectKind::Commit, &c1);
        let c2 = format!(
            "tree {empty_tree}\nparent {c1_oid}\nauthor A <a@x> 3 +0000\ncommitter A <a@x> 3 +0000\n\nc2\n"
        )
        .into_bytes();
        let c2_oid = git_oid(GitObjectKind::Commit, &c2);

        let pack = write_pack(&[
            (GitObjectKind::Commit, root.clone()),
            (GitObjectKind::Commit, c1.clone()),
            (GitObjectKind::Commit, c2.clone()),
            (GitObjectKind::Tree, Vec::new()),
        ]);
        let tmp = tempfile::tempdir().unwrap();
        let mut store = RemoteStore::open(tmp.path(), "anc").unwrap();
        store.record_fetch(&pack, &[]).unwrap();

        assert!(store.is_ancestor(&root_oid, &c2_oid).unwrap());
        assert!(store.is_ancestor(&c1_oid, &c2_oid).unwrap());
        assert!(store.is_ancestor(&c2_oid, &c2_oid).unwrap());
        assert!(!store.is_ancestor(&c2_oid, &root_oid).unwrap());
        // A commit not in the store → not an ancestor, no error.
        assert!(!store.is_ancestor(&"f".repeat(40), &c2_oid).unwrap());
    }

    #[test]
    fn negotiate_push_caps_prefers_v2_and_sideband() {
        let (caps, sb) = negotiate_push_caps(&[
            "report-status".into(),
            "report-status-v2".into(),
            "side-band-64k".into(),
            "ofs-delta".into(),
            "delete-refs".into(),
        ]);
        assert!(caps.iter().any(|c| c == "report-status-v2"));
        assert!(!caps.iter().any(|c| c == "report-status")); // v2 wins, plain dropped
        assert!(caps.iter().any(|c| c == "side-band-64k"));
        assert!(caps.iter().any(|c| c == "ofs-delta"));
        assert!(caps.iter().any(|c| c.starts_with("agent=asp/")));
        assert!(sb);

        let (caps, sb) = negotiate_push_caps(&["report-status".into()]);
        assert!(caps.iter().any(|c| c == "report-status"));
        assert!(!sb);
    }

    #[test]
    fn classify_ref_rejection_maps_non_ff() {
        assert!(matches!(
            classify_ref_rejection("non-fast-forward"),
            GitBridgeError::NonFastForward
        ));
        assert!(matches!(
            classify_ref_rejection("failed to update ref: fetch first"),
            GitBridgeError::NonFastForward
        ));
        assert!(matches!(
            classify_ref_rejection("hook declined"),
            GitBridgeError::Rejected(_)
        ));
    }

    #[test]
    fn advertisement_len_finds_flush() {
        use crate::gitwire::{flush_pkt, pkt_line};
        let mut body = Vec::new();
        body.extend(pkt_line(b"version 2\n"));
        body.extend(pkt_line(b"agent=git/2.40\n"));
        body.extend_from_slice(flush_pkt());
        body.extend_from_slice(b"TRAILING RESPONSE BYTES");
        let n = advertisement_len(&body).unwrap();
        assert_eq!(&body[n..], b"TRAILING RESPONSE BYTES");
        // No flush → typed error, no panic.
        assert!(advertisement_len(b"0009version 2").is_err());
    }
}
