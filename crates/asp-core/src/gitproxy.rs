//! Relay-co-hosted git CORS proxy (native-only; §7.3 of the git-bridge spec).
//!
//! Git hosts do not send CORS headers on their smart-HTTP endpoints, so a browser
//! `fetch()` to `https://github.com/owner/repo.git/info/refs?...` is blocked by the
//! same-origin policy — even for a read-only clone. This module is a **stateless**
//! HTTP proxy, co-hosted with the iroh relay (`asp relay --git-proxy`), that forwards
//! *exactly two* request shapes to an upstream git host and adds the CORS headers the
//! browser needs. It forwards nothing else.
//!
//! ## URL mapping (the contract the wasm fetch transport relies on)
//!
//! ```text
//!   proxy path   = "/git/" + <upstream host> + <upstream path>
//!   upstream URL = "https://" + <upstream host> + <upstream path>
//! ```
//!
//! The two — and only two — forwarded shapes:
//!
//! ```text
//!   GET  /git/<host>/<path...>/info/refs?service=git-upload-pack
//!   POST /git/<host>/<path...>/git-upload-pack
//! ```
//!
//! e.g. to clone `github.com/owner/repo.git` the browser requests
//! `GET  <proxy>/git/github.com/owner/repo.git/info/refs?service=git-upload-pack`
//! then `POST <proxy>/git/github.com/owner/repo.git/git-upload-pack`.
//!
//! `service=git-receive-pack` (push) is **rejected** — browser push is out of scope.
//! Any other method / path / query is rejected. `OPTIONS` is answered locally (204).
//!
//! ## Hard rules (all from §7.3)
//!
//! * **HTTPS-only upstream, port 443 only.** The host segment must carry no explicit
//!   port; we always resolve `host:443` and connect only there.
//! * **SSRF.** The resolved upstream IP is vetted with [`is_forbidden_ip`] and any
//!   private / loopback / link-local / CGNAT / multicast / unspecified / reserved
//!   address (IPv4 **and** IPv6, including v4-mapped v6) is refused.
//! * **DNS-rebinding defense.** We resolve the host **once**, vet the IP, then pin
//!   reqwest to that exact `SocketAddr` via [`reqwest::ClientBuilder::resolve_to_addrs`]
//!   so the actual TCP connect cannot re-resolve to a different (private) IP between
//!   our check and the request. Redirects are disabled for the same reason (a 3xx
//!   could point at a private host, bypassing the vet).
//! * **Header hygiene.** Only `Authorization`, `Content-Type`, `Accept` and
//!   `Git-Protocol` are forwarded upstream (defaulting `Git-Protocol: version=2`);
//!   cookies are dropped in both directions; the `Authorization` value is **never**
//!   logged.
//! * **Caps.** Configurable request- and response-body caps (default 1 GiB), connect
//!   and overall timeouts, and a simple in-memory per-IP token-bucket rate limit.
//! * **Optional allowlist.** `--git-proxy-allow <host>` (repeatable): when non-empty,
//!   only those **exact** hosts are proxied (no subdomain wildcarding).
//!
//! Unlike relayed ASP traffic (which stays end-to-end encrypted), git payloads are
//! **TLS-terminated at this proxy** — the operator's box sees the plaintext git bytes
//! and any `Authorization` token. Documented so deployers can decide accordingly.

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use bytes::Bytes;
use futures_util::StreamExt;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Empty, Full, Limited, StreamBody};
use hyper::body::{Frame, Incoming};
use hyper::header::{self, HeaderMap, HeaderValue};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::{lookup_host, TcpListener};
use tokio::task::JoinHandle;

/// The response body type produced by this proxy. Errors surface as `io::Error` (from
/// the capped upstream stream); `Full`/`Empty` bodies never error (`Infallible`).
type ProxyBody = BoxBody<Bytes, std::io::Error>;

// ---------------- configuration ----------------

/// Configuration for [`serve_git_proxy`] / [`spawn_git_proxy`].
#[derive(Clone, Debug)]
pub struct GitProxyConfig {
    /// Address the proxy's HTTP listener binds. Separate from the iroh relay port.
    pub bind: SocketAddr,
    /// Exact-match host allowlist. Empty = allow any (SSRF-vetted) host.
    pub allow_hosts: Vec<String>,
    /// Request- and response-body cap in bytes (default 1 GiB).
    pub max_body: u64,
    /// Token-bucket burst size, per client IP (default 60).
    pub rate_burst: u32,
    /// Token-bucket refill rate, tokens per second, per client IP (default 30).
    pub rate_per_sec: f64,
    /// TCP connect timeout to the upstream (default 10s).
    pub connect_timeout: Duration,
    /// Overall per-request timeout to the upstream (default 300s).
    pub overall_timeout: Duration,

    /// **Test-only** escape hatch: when `Some`, the SSRF/HTTPS rules are bypassed and
    /// every forward connects to this address over plain HTTP, letting an in-test
    /// loopback upstream stand in for a real git host. Private to the crate and never
    /// wired to any CLI flag, so it is impossible to enable from `asp relay`.
    test_upstream_addr: Option<SocketAddr>,
}

impl GitProxyConfig {
    /// A config with the documented defaults bound to `bind`.
    pub fn new(bind: SocketAddr) -> Self {
        Self {
            bind,
            allow_hosts: Vec::new(),
            max_body: 1 << 30, // 1 GiB
            rate_burst: 60,
            rate_per_sec: 30.0,
            connect_timeout: Duration::from_secs(10),
            overall_timeout: Duration::from_secs(300),
            test_upstream_addr: None,
        }
    }
}

// ---------------- server entry points ----------------

/// Run the git CORS proxy until the process is cancelled. Binds `cfg.bind`.
pub async fn serve_git_proxy(cfg: GitProxyConfig) -> Result<()> {
    let listener = TcpListener::bind(cfg.bind).await?;
    let addr = listener.local_addr()?;
    tracing::info!(%addr, "git CORS proxy up — forwards git-upload-pack only, adds CORS");
    let state = Arc::new(ProxyState::new(cfg));
    accept_loop(listener, state).await;
    Ok(())
}

/// Spawn the proxy on a background task and return its bound address plus the task
/// handle (aborting the task stops the proxy). Mirrors [`crate::iroh_net::spawn_relay`].
/// Bind `127.0.0.1:0` for a free localhost port (used by tests).
pub async fn spawn_git_proxy(cfg: GitProxyConfig) -> Result<(SocketAddr, JoinHandle<()>)> {
    let listener = TcpListener::bind(cfg.bind).await?;
    let addr = listener.local_addr()?;
    let state = Arc::new(ProxyState::new(cfg));
    tracing::info!(%addr, "git CORS proxy up (spawned)");
    let handle = tokio::spawn(async move {
        accept_loop(listener, state).await;
    });
    Ok((addr, handle))
}

async fn accept_loop(listener: TcpListener, state: Arc<ProxyState>) {
    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(v) => v,
            Err(e) => {
                tracing::debug!("git-proxy accept error: {e}");
                continue;
            }
        };
        let state = state.clone();
        tokio::spawn(async move {
            let io = TokioIo::new(stream);
            let svc = service_fn(move |req| handle_request(req, state.clone(), peer.ip()));
            if let Err(e) = http1::Builder::new().serve_connection(io, svc).await {
                tracing::debug!("git-proxy connection error: {e}");
            }
        });
    }
}

// ---------------- shared state ----------------

struct ProxyState {
    cfg: GitProxyConfig,
    limiter: RateLimiter,
}

impl ProxyState {
    fn new(cfg: GitProxyConfig) -> Self {
        let limiter = RateLimiter::new(cfg.rate_burst as f64, cfg.rate_per_sec);
        Self { cfg, limiter }
    }

    /// Exact-match allowlist (case-insensitive). Empty allowlist = allow all.
    fn host_allowed(&self, host: &str) -> bool {
        if self.cfg.allow_hosts.is_empty() {
            return true;
        }
        self.cfg
            .allow_hosts
            .iter()
            .any(|a| a.eq_ignore_ascii_case(host))
    }
}

// ---------------- request handling ----------------

async fn handle_request(
    req: Request<Incoming>,
    state: Arc<ProxyState>,
    client_ip: IpAddr,
) -> std::result::Result<Response<ProxyBody>, Infallible> {
    // Per-IP token bucket. Note: `client_ip` is the direct-connection peer; behind a
    // reverse proxy that would be the front box (documented — no X-Forwarded-For trust).
    if !state.limiter.allow(client_ip) {
        return Ok(text_resp(StatusCode::TOO_MANY_REQUESTS, "rate limit exceeded"));
    }

    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let query = req.uri().query().unwrap_or("").to_string();

    match parse_route(&method, &path, &query) {
        Routed::Preflight => Ok(preflight_resp()),
        Routed::Reject(status, why) => {
            // Never logs headers — only method + path (no Authorization can leak here).
            tracing::debug!(%method, path = %path, "git-proxy rejected: {why}");
            Ok(text_resp(status, why))
        }
        Routed::Forward {
            host,
            upstream_path,
            is_post,
        } => {
            if !state.host_allowed(&host) {
                return Ok(text_resp(StatusCode::FORBIDDEN, "host not in allowlist"));
            }
            match do_forward(&state, host, upstream_path, is_post, req).await {
                Ok(resp) => Ok(resp),
                Err(e) => {
                    tracing::warn!("git-proxy upstream error: {}", e.msg);
                    Ok(text_resp(e.status, e.msg))
                }
            }
        }
    }
}

/// Perform the actual upstream request and stream the (capped) response back.
async fn do_forward(
    state: &ProxyState,
    host: String,
    upstream_path: String,
    is_post: bool,
    req: Request<Incoming>,
) -> std::result::Result<Response<ProxyBody>, ProxyError> {
    let (parts, body) = req.into_parts();

    // Buffer + cap the request body (upload-pack "want" lists are small). Over-cap
    // bodies error out of `Limited` rather than being read forever.
    let body_bytes = if is_post {
        match Limited::new(body, state.cfg.max_body as usize).collect().await {
            Ok(collected) => collected.to_bytes(),
            Err(_) => {
                return Err(ProxyError::new(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "request body exceeds cap",
                ))
            }
        }
    } else {
        Bytes::new()
    };

    // Resolve + vet once, then pin reqwest to the vetted address (rebinding defense).
    let targets = resolve_targets(state, &host).await?;
    let scheme = if state.cfg.test_upstream_addr.is_some() {
        "http"
    } else {
        "https"
    };
    let url = format!("{scheme}://{host}{upstream_path}");

    let client = reqwest::Client::builder()
        .resolve_to_addrs(&host, &targets)
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(state.cfg.connect_timeout)
        .timeout(state.cfg.overall_timeout)
        .build()
        .map_err(|_| ProxyError::new(StatusCode::BAD_GATEWAY, "failed to build upstream client"))?;

    let mut rb = if is_post {
        client.post(&url)
    } else {
        client.get(&url)
    };

    // Forward ONLY the allowlisted request headers (cookies etc. are dropped).
    for name in [header::AUTHORIZATION, header::CONTENT_TYPE, header::ACCEPT] {
        if let Some(v) = parts.headers.get(&name) {
            rb = rb.header(name, v);
        }
    }
    // Git-Protocol passthrough, defaulting to protocol v2.
    let git_protocol = parts
        .headers
        .get("git-protocol")
        .cloned()
        .unwrap_or_else(|| HeaderValue::from_static("version=2"));
    rb = rb.header("Git-Protocol", git_protocol);

    if is_post {
        rb = rb.body(body_bytes);
    }

    let upstream = rb
        .send()
        .await
        .map_err(|_| ProxyError::new(StatusCode::BAD_GATEWAY, "upstream request failed"))?;

    let status = upstream.status();

    // If the upstream advertises a length over the cap, refuse before streaming.
    if let Some(len) = upstream.content_length() {
        if len > state.cfg.max_body {
            return Err(ProxyError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "upstream response exceeds cap",
            ));
        }
    }
    let upstream_ct = upstream.headers().get(header::CONTENT_TYPE).cloned();

    // Stream the body through, counting bytes; if an un-advertised (chunked) body
    // exceeds the cap, yield an error — hyper drops the connection rather than
    // buffering the whole thing.
    let max = state.cfg.max_body;
    let mut count: u64 = 0;
    let stream = upstream.bytes_stream().map(move |item| match item {
        Ok(chunk) => {
            count += chunk.len() as u64;
            if count > max {
                Err(io_err("upstream response body exceeded cap"))
            } else {
                Ok(Frame::data(chunk))
            }
        }
        Err(e) => Err(io_err(&format!("upstream stream error: {e}"))),
    });
    let out_body = BodyExt::boxed(StreamBody::new(stream));

    let mut builder = Response::builder().status(status);
    if let Some(ct) = upstream_ct {
        builder = builder.header(header::CONTENT_TYPE, ct);
    }
    let resp = builder
        .body(out_body)
        .map_err(|_| ProxyError::new(StatusCode::BAD_GATEWAY, "failed to build response"))?;
    Ok(add_cors(resp))
}

/// Resolve the upstream host to vetted `SocketAddr`s (port 443), rejecting any that
/// map to a forbidden range. In test mode, returns the fixed loopback upstream.
async fn resolve_targets(
    state: &ProxyState,
    host: &str,
) -> std::result::Result<Vec<SocketAddr>, ProxyError> {
    if let Some(addr) = state.cfg.test_upstream_addr {
        return Ok(vec![addr]);
    }
    let addrs: Vec<SocketAddr> = lookup_host((host, 443u16))
        .await
        .map_err(|_| ProxyError::new(StatusCode::BAD_GATEWAY, "dns resolution failed"))?
        .collect();
    if addrs.is_empty() {
        return Err(ProxyError::new(StatusCode::BAD_GATEWAY, "host did not resolve"));
    }
    for a in &addrs {
        if is_forbidden_ip(a.ip()) {
            return Err(ProxyError::new(
                StatusCode::FORBIDDEN,
                "host resolves to a disallowed (private/loopback/link-local) address",
            ));
        }
    }
    Ok(addrs)
}

// ---------------- routing (pure) ----------------

/// The outcome of routing a request. Pure over `(method, path, query)`.
#[derive(Debug, PartialEq, Eq)]
enum Routed {
    /// A CORS preflight — answer 204 locally.
    Preflight,
    /// Forward this exact shape upstream.
    Forward {
        host: String,
        /// The upstream path (leading `/`), including the query for the GET shape.
        upstream_path: String,
        is_post: bool,
    },
    /// Reject with this status + reason.
    Reject(StatusCode, &'static str),
}

/// Route a request to exactly one of the two allowed shapes, a preflight, or a
/// rejection. This is the single validation choke point — the server forwards
/// **nothing** that does not come back `Routed::Forward` here.
fn parse_route(method: &Method, path: &str, query: &str) -> Routed {
    if method == Method::OPTIONS {
        return Routed::Preflight;
    }

    let rest = match path.strip_prefix("/git/") {
        Some(r) => r,
        None => return Routed::Reject(StatusCode::NOT_FOUND, "path must start with /git/"),
    };
    let (host, tail) = match rest.split_once('/') {
        Some((h, t)) => (h, t),
        None => return Routed::Reject(StatusCode::NOT_FOUND, "missing repository path"),
    };
    if !is_valid_host(host) {
        return Routed::Reject(StatusCode::BAD_REQUEST, "invalid host segment");
    }
    let upstream_base = format!("/{tail}");
    if !is_safe_path(&upstream_base) {
        return Routed::Reject(StatusCode::BAD_REQUEST, "invalid path");
    }

    match *method {
        Method::GET => {
            if !upstream_base.ends_with("/info/refs") {
                return Routed::Reject(StatusCode::NOT_FOUND, "GET is only allowed for info/refs");
            }
            if query == "service=git-upload-pack" {
                Routed::Forward {
                    host: host.to_string(),
                    upstream_path: format!("{upstream_base}?{query}"),
                    is_post: false,
                }
            } else if query.contains("git-receive-pack") {
                Routed::Reject(StatusCode::FORBIDDEN, "git-receive-pack (push) is not allowed")
            } else {
                Routed::Reject(
                    StatusCode::BAD_REQUEST,
                    "info/refs requires exactly service=git-upload-pack",
                )
            }
        }
        Method::POST => {
            if !query.is_empty() {
                return Routed::Reject(StatusCode::BAD_REQUEST, "POST takes no query string");
            }
            if upstream_base.ends_with("/git-upload-pack") {
                Routed::Forward {
                    host: host.to_string(),
                    upstream_path: upstream_base,
                    is_post: true,
                }
            } else if upstream_base.ends_with("/git-receive-pack") {
                Routed::Reject(StatusCode::FORBIDDEN, "git-receive-pack (push) is not allowed")
            } else {
                Routed::Reject(StatusCode::NOT_FOUND, "POST is only allowed for git-upload-pack")
            }
        }
        _ => Routed::Reject(StatusCode::METHOD_NOT_ALLOWED, "method not allowed"),
    }
}

/// A host segment is a bare DNS name: non-empty, <=253 bytes, only
/// `[A-Za-z0-9.-]`. Crucially this rejects any explicit port (`:`), userinfo (`@`),
/// IPv6-literal brackets, and percent-encoding — so "port 443 only" is enforced and
/// IP-literal SSRF tricks in the host segment cannot slip through.
fn is_valid_host(host: &str) -> bool {
    if host.is_empty() || host.len() > 253 {
        return false;
    }
    host.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
}

/// The upstream path must be printable, start with `/`, and contain no whitespace,
/// control chars, backslashes, or `..` traversal segments.
fn is_safe_path(path: &str) -> bool {
    if !path.starts_with('/') {
        return false;
    }
    if path.contains("..") {
        return false;
    }
    path.chars()
        .all(|c| !c.is_control() && !c.is_whitespace() && c != '\\')
}

// ---------------- SSRF: IP vetting (pure) ----------------

/// Return `true` if connecting to `ip` should be **refused** as an SSRF risk. Covers
/// the loopback / private / link-local / CGNAT / multicast / unspecified / reserved
/// ranges for IPv4 and IPv6, including IPv4-mapped IPv6 (`::ffff:a.b.c.d`).
pub fn is_forbidden_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_forbidden_v4(v4),
        IpAddr::V6(v6) => is_forbidden_v6(v6),
    }
}

fn is_forbidden_v4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    ip.is_unspecified()          // 0.0.0.0
        || ip.is_loopback()      // 127.0.0.0/8
        || ip.is_private()       // 10/8, 172.16/12, 192.168/16
        || ip.is_link_local()    // 169.254.0.0/16
        || ip.is_broadcast()     // 255.255.255.255
        || ip.is_multicast()     // 224.0.0.0/4
        || ip.is_documentation() // 192.0.2/24, 198.51.100/24, 203.0.113/24
        || o[0] == 0                                       // 0.0.0.0/8 "this network"
        || (o[0] == 100 && (o[1] & 0xc0) == 64)            // 100.64.0.0/10 CGNAT
        || (o[0] == 192 && o[1] == 0 && o[2] == 0)         // 192.0.0.0/24 IETF protocol
        || (o[0] == 198 && (o[1] & 0xfe) == 18)            // 198.18.0.0/15 benchmarking
        || o[0] >= 240 // 240.0.0.0/4 reserved (incl. limited broadcast)
}

fn is_forbidden_v6(ip: Ipv6Addr) -> bool {
    // v4-mapped (::ffff:a.b.c.d) inherits the IPv4 rules.
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_forbidden_v4(v4);
    }
    let seg = ip.segments();
    ip.is_unspecified()                 // ::
        || ip.is_loopback()             // ::1
        || ip.is_multicast()            // ff00::/8
        || (seg[0] & 0xffc0) == 0xfe80  // fe80::/10 link-local
        || (seg[0] & 0xfe00) == 0xfc00 // fc00::/7 unique-local
}

// ---------------- rate limiting ----------------

/// A trivially simple per-IP token bucket. `burst` tokens max, refilled `per_sec`
/// tokens/second; each request costs one token, and an empty bucket → 429.
struct RateLimiter {
    burst: f64,
    per_sec: f64,
    buckets: Mutex<HashMap<IpAddr, (f64, Instant)>>,
}

impl RateLimiter {
    fn new(burst: f64, per_sec: f64) -> Self {
        Self {
            burst,
            per_sec,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    fn allow(&self, ip: IpAddr) -> bool {
        let now = Instant::now();
        let mut map = self.buckets.lock().unwrap();
        // Bound memory: if the table gets large, drop buckets idle > 5 min.
        if map.len() > 10_000 {
            map.retain(|_, (_, last)| now.duration_since(*last) < Duration::from_secs(300));
        }
        let entry = map.entry(ip).or_insert((self.burst, now));
        let elapsed = now.duration_since(entry.1).as_secs_f64();
        entry.1 = now;
        entry.0 = (entry.0 + elapsed * self.per_sec).min(self.burst);
        if entry.0 >= 1.0 {
            entry.0 -= 1.0;
            true
        } else {
            false
        }
    }
}

// ---------------- responses & helpers ----------------

struct ProxyError {
    status: StatusCode,
    msg: &'static str,
}

impl ProxyError {
    fn new(status: StatusCode, msg: &'static str) -> Self {
        Self { status, msg }
    }
}

fn io_err(msg: &str) -> std::io::Error {
    std::io::Error::other(msg.to_string())
}

fn empty_body() -> ProxyBody {
    Empty::<Bytes>::new().map_err(|never| match never {}).boxed()
}

fn full_body(bytes: Bytes) -> ProxyBody {
    Full::new(bytes).map_err(|never| match never {}).boxed()
}

/// Add the CORS headers every response carries, and strip any `Set-Cookie` the
/// upstream tried to send back (cookies are dropped in both directions).
fn add_cors(mut resp: Response<ProxyBody>) -> Response<ProxyBody> {
    let h = resp.headers_mut();
    h.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    h.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET, POST, OPTIONS"),
    );
    h.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("Authorization, Content-Type, Accept, Git-Protocol"),
    );
    h.insert(
        header::ACCESS_CONTROL_MAX_AGE,
        HeaderValue::from_static("86400"),
    );
    h.remove(header::SET_COOKIE);
    resp
}

fn preflight_resp() -> Response<ProxyBody> {
    let resp = Response::builder()
        .status(StatusCode::NO_CONTENT)
        .body(empty_body())
        .expect("static preflight response");
    add_cors(resp)
}

fn text_resp(status: StatusCode, msg: &str) -> Response<ProxyBody> {
    let resp = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(full_body(Bytes::from(msg.to_string())))
        .expect("static text response");
    add_cors(resp)
}

/// A log-safe rendering of request headers: every value is shown **except**
/// `Authorization`, which is redacted. Used so tracing can never leak a token.
#[allow(dead_code)]
fn redacted_headers(headers: &HeaderMap) -> String {
    let mut out = String::new();
    for (name, value) in headers {
        out.push_str(name.as_str());
        out.push_str(": ");
        if name == header::AUTHORIZATION {
            out.push_str("<redacted>");
        } else {
            out.push_str(value.to_str().unwrap_or("<binary>"));
        }
        out.push('\n');
    }
    out
}

// ---------------- tests ----------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    // -------- SSRF ip-vetting table --------

    #[test]
    fn ssrf_forbidden_ranges() {
        let forbidden: &[&str] = &[
            "127.0.0.1",
            "10.0.0.1",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "169.254.1.1",  // link-local
            "100.64.0.1",   // CGNAT
            "100.127.0.1",  // CGNAT upper
            "0.0.0.0",      // unspecified / this-network
            "224.0.0.1",    // multicast
            "255.255.255.255",
            "240.0.0.1",    // reserved
            "198.18.0.1",   // benchmarking
            "::1",          // v6 loopback
            "::",           // v6 unspecified
            "fe80::1",      // v6 link-local
            "fc00::1",      // v6 unique-local
            "fd12:3456::1", // v6 unique-local
            "ff02::1",      // v6 multicast
            "::ffff:10.0.0.1",   // v4-mapped private
            "::ffff:127.0.0.1",  // v4-mapped loopback
        ];
        for s in forbidden {
            let ip: IpAddr = s.parse().unwrap();
            assert!(is_forbidden_ip(ip), "{s} should be forbidden");
        }

        // Public / routable addresses must be allowed. (Note: 203.0.113.0/24 is
        // TEST-NET-3 documentation space and is intentionally NOT in this list — it is
        // covered by the forbidden set via `is_documentation`.)
        let public: &[&str] = &["8.8.8.8", "140.82.121.3", "1.1.1.1", "2606:4700:4700::1111"];
        for s in public {
            let ip: IpAddr = s.parse().unwrap();
            assert!(!is_forbidden_ip(ip), "{s} should be allowed");
        }
    }

    #[test]
    fn ssrf_v4_mapped_matches_v4() {
        // ::ffff:a.b.c.d must vet identically to a.b.c.d.
        for octet in [Ipv4Addr::new(10, 0, 0, 1), Ipv4Addr::new(8, 8, 8, 8)] {
            let mapped = octet.to_ipv6_mapped();
            assert_eq!(
                is_forbidden_ip(IpAddr::V6(mapped)),
                is_forbidden_ip(IpAddr::V4(octet)),
                "{octet} mapped mismatch"
            );
        }
        let _ = Ipv6Addr::LOCALHOST;
    }

    // -------- routing --------

    fn route(method: &str, path: &str, query: &str) -> Routed {
        parse_route(&Method::from_bytes(method.as_bytes()).unwrap(), path, query)
    }

    #[test]
    fn route_info_refs_ok() {
        let r = route("GET", "/git/github.com/owner/repo.git/info/refs", "service=git-upload-pack");
        assert_eq!(
            r,
            Routed::Forward {
                host: "github.com".into(),
                upstream_path: "/owner/repo.git/info/refs?service=git-upload-pack".into(),
                is_post: false,
            }
        );
    }

    #[test]
    fn route_upload_pack_ok() {
        let r = route("POST", "/git/github.com/owner/repo.git/git-upload-pack", "");
        assert_eq!(
            r,
            Routed::Forward {
                host: "github.com".into(),
                upstream_path: "/owner/repo.git/git-upload-pack".into(),
                is_post: true,
            }
        );
    }

    #[test]
    fn route_preflight() {
        assert_eq!(route("OPTIONS", "/git/github.com/x/info/refs", ""), Routed::Preflight);
    }

    #[test]
    fn route_rejections() {
        // receive-pack (push) is refused in both shapes
        assert!(matches!(
            route("GET", "/git/github.com/r.git/info/refs", "service=git-receive-pack"),
            Routed::Reject(StatusCode::FORBIDDEN, _)
        ));
        assert!(matches!(
            route("POST", "/git/github.com/r.git/git-receive-pack", ""),
            Routed::Reject(StatusCode::FORBIDDEN, _)
        ));
        // arbitrary paths
        assert!(matches!(route("GET", "/", ""), Routed::Reject(..)));
        assert!(matches!(route("GET", "/git/github.com", ""), Routed::Reject(..)));
        assert!(matches!(
            route("GET", "/git/github.com/r.git/objects/info/packs", ""),
            Routed::Reject(..)
        ));
        // GET info/refs must carry the exact service query
        assert!(matches!(
            route("GET", "/git/github.com/r.git/info/refs", ""),
            Routed::Reject(..)
        ));
        // POST must carry no query
        assert!(matches!(
            route("POST", "/git/github.com/r.git/git-upload-pack", "service=git-upload-pack"),
            Routed::Reject(..)
        ));
        // wrong methods
        assert!(matches!(
            route("PUT", "/git/github.com/r.git/git-upload-pack", ""),
            Routed::Reject(StatusCode::METHOD_NOT_ALLOWED, _)
        ));
        assert!(matches!(
            route("DELETE", "/git/github.com/r.git/info/refs", "service=git-upload-pack"),
            Routed::Reject(StatusCode::METHOD_NOT_ALLOWED, _)
        ));
        // host with an explicit port is refused (port 443 only)
        assert!(matches!(
            route("GET", "/git/github.com:22/r.git/info/refs", "service=git-upload-pack"),
            Routed::Reject(StatusCode::BAD_REQUEST, _)
        ));
        // path traversal
        assert!(matches!(
            route("POST", "/git/github.com/../../etc/git-upload-pack", ""),
            Routed::Reject(..)
        ));
    }

    /// Deterministic LCG fuzz (repo style, no proptest): hammer the router with random
    /// methods/paths/queries and assert it never panics and only ever returns
    /// `Forward` for the two exact shapes.
    #[test]
    fn route_fuzz_never_forwards_unexpected() {
        let mut state: u64 = 0x9E3779B97F4A7C15;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let methods = ["GET", "POST", "OPTIONS", "PUT", "HEAD", "DELETE", "PATCH"];
        let segs = [
            "git", "github.com", "owner", "repo.git", "info", "refs", "git-upload-pack",
            "git-receive-pack", "..", "objects", "", "a:b", "x", "%2e", "\n", " ",
        ];
        let queries = [
            "",
            "service=git-upload-pack",
            "service=git-receive-pack",
            "service=git-upload-pack&x=1",
            "foo=bar",
        ];
        for _ in 0..20_000 {
            let m = methods[(next() as usize) % methods.len()];
            let nseg = (next() as usize) % 6;
            let mut path = String::from("/");
            for _ in 0..nseg {
                path.push_str(segs[(next() as usize) % segs.len()]);
                path.push('/');
            }
            if path.len() > 1 {
                path.pop();
            }
            let q = queries[(next() as usize) % queries.len()];
            let method = Method::from_bytes(m.as_bytes()).unwrap();
            match parse_route(&method, &path, q) {
                Routed::Forward {
                    host,
                    upstream_path,
                    is_post,
                } => {
                    // Only the two exact shapes may forward.
                    assert!(is_valid_host(&host), "forwarded invalid host: {host}");
                    if is_post {
                        assert_eq!(method, Method::POST);
                        assert!(upstream_path.ends_with("/git-upload-pack"));
                        assert!(!upstream_path.contains('?'));
                        assert!(!upstream_path.contains("git-receive-pack"));
                    } else {
                        assert_eq!(method, Method::GET);
                        assert!(upstream_path.ends_with("/info/refs?service=git-upload-pack"));
                    }
                }
                Routed::Preflight => assert_eq!(method, Method::OPTIONS),
                Routed::Reject(..) => {}
            }
        }
    }

    // -------- redaction --------

    #[test]
    fn authorization_is_redacted() {
        let mut headers = HeaderMap::new();
        headers.insert(header::AUTHORIZATION, HeaderValue::from_static("Bearer supersecrettoken"));
        headers.insert(header::ACCEPT, HeaderValue::from_static("application/x-git-upload-pack-result"));
        let rendered = redacted_headers(&headers);
        assert!(!rendered.contains("supersecrettoken"), "token leaked: {rendered}");
        assert!(rendered.contains("<redacted>"));
        assert!(rendered.contains("application/x-git-upload-pack-result"));
    }

    // -------- end-to-end against a local fake upstream --------

    /// Spin a minimal plain-HTTP "upstream" that echoes request info and returns a
    /// configurable body, so the proxy (with the test hook) can be exercised without
    /// a real HTTPS git host.
    async fn spawn_fake_upstream() -> (SocketAddr, JoinHandle<()>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            loop {
                let (stream, _) = match listener.accept().await {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                tokio::spawn(async move {
                    let io = TokioIo::new(stream);
                    let svc = service_fn(fake_upstream_handler);
                    let _ = http1::Builder::new().serve_connection(io, svc).await;
                });
            }
        });
        (addr, handle)
    }

    async fn fake_upstream_handler(
        req: Request<Incoming>,
    ) -> std::result::Result<Response<Full<Bytes>>, Infallible> {
        let path = req.uri().path().to_string();
        let query = req.uri().query().unwrap_or("").to_string();
        // Report which headers arrived (so the test can assert filtering).
        let auth = req
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("<none>")
            .to_string();
        let cookie = req
            .headers()
            .get(header::COOKIE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("<none>")
            .to_string();
        let git_proto = req
            .headers()
            .get("git-protocol")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("<none>")
            .to_string();

        // "big" path returns an over-cap body with an advertised length.
        if path.contains("big") {
            let big = vec![b'x'; 100];
            let resp = Response::builder()
                .status(200)
                .header(header::CONTENT_TYPE, "application/octet-stream")
                .body(Full::new(Bytes::from(big)))
                .unwrap();
            return Ok(resp);
        }

        let body = format!(
            "path={path};query={query};auth={auth};cookie={cookie};git-protocol={git_proto}"
        );
        let resp = Response::builder()
            .status(200)
            .header(header::CONTENT_TYPE, "application/x-git-upload-pack-advertisement")
            .header(header::SET_COOKIE, "session=abc")
            .body(Full::new(Bytes::from(body)))
            .unwrap();
        Ok(resp)
    }

    async fn spawn_test_proxy(cfg_mut: impl FnOnce(&mut GitProxyConfig)) -> (SocketAddr, SocketAddr) {
        let (upstream_addr, _up) = spawn_fake_upstream().await;
        // Leak the upstream task so it lives for the test's duration.
        std::mem::forget(_up);
        let mut cfg = GitProxyConfig::new("127.0.0.1:0".parse().unwrap());
        cfg.test_upstream_addr = Some(upstream_addr);
        cfg_mut(&mut cfg);
        let (proxy_addr, handle) = spawn_git_proxy(cfg).await.unwrap();
        std::mem::forget(handle);
        (proxy_addr, upstream_addr)
    }

    fn plain_client() -> reqwest::Client {
        reqwest::Client::builder().build().unwrap()
    }

    #[tokio::test]
    async fn e2e_get_info_refs_proxies_and_filters() {
        let (proxy, _up) = spawn_test_proxy(|_| {}).await;
        let url = format!(
            "http://{proxy}/git/upstream.test/owner/repo.git/info/refs?service=git-upload-pack"
        );
        let resp = plain_client()
            .get(&url)
            .header(header::AUTHORIZATION, "Bearer tok123")
            .header(header::COOKIE, "should=not-forward")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        // CORS present
        assert_eq!(
            resp.headers().get(header::ACCESS_CONTROL_ALLOW_ORIGIN).unwrap(),
            "*"
        );
        assert!(resp.headers().get(header::ACCESS_CONTROL_ALLOW_METHODS).is_some());
        // upstream Set-Cookie stripped
        assert!(resp.headers().get(header::SET_COOKIE).is_none());
        // content-type passed through
        assert_eq!(
            resp.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/x-git-upload-pack-advertisement"
        );
        let body = resp.text().await.unwrap();
        // Authorization forwarded; Cookie NOT forwarded; Git-Protocol defaulted.
        assert!(body.contains("auth=Bearer tok123"), "{body}");
        assert!(body.contains("cookie=<none>"), "{body}");
        assert!(body.contains("git-protocol=version=2"), "{body}");
        assert!(body.contains("path=/owner/repo.git/info/refs"), "{body}");
    }

    #[tokio::test]
    async fn e2e_post_upload_pack_round_trips_body() {
        let (proxy, _up) = spawn_test_proxy(|_| {}).await;
        let url = format!("http://{proxy}/git/upstream.test/owner/repo.git/git-upload-pack");
        let resp = plain_client()
            .post(&url)
            .header(header::CONTENT_TYPE, "application/x-git-upload-pack-request")
            .header("Git-Protocol", "version=2")
            .body("0011command=fetch")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body = resp.text().await.unwrap();
        assert!(body.contains("path=/owner/repo.git/git-upload-pack"), "{body}");
        assert!(body.contains("git-protocol=version=2"), "{body}");
    }

    #[tokio::test]
    async fn e2e_preflight_204() {
        let (proxy, _up) = spawn_test_proxy(|_| {}).await;
        let url = format!("http://{proxy}/git/upstream.test/x/info/refs");
        let resp = plain_client()
            .request(Method::OPTIONS, &url)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 204);
        assert_eq!(
            resp.headers().get(header::ACCESS_CONTROL_ALLOW_HEADERS).unwrap(),
            "Authorization, Content-Type, Accept, Git-Protocol"
        );
    }

    #[tokio::test]
    async fn e2e_receive_pack_rejected() {
        let (proxy, _up) = spawn_test_proxy(|_| {}).await;
        let url = format!(
            "http://{proxy}/git/upstream.test/r.git/info/refs?service=git-receive-pack"
        );
        let resp = plain_client().get(&url).send().await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn e2e_allowlist_blocks_other_hosts() {
        let (proxy, _up) = spawn_test_proxy(|c| c.allow_hosts = vec!["allowed.test".into()]).await;
        // A disallowed host → 403 (routing succeeds, allowlist blocks).
        let blocked = format!(
            "http://{proxy}/git/upstream.test/r.git/info/refs?service=git-upload-pack"
        );
        let resp = plain_client().get(&blocked).send().await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        // The allowed host resolves to the same test upstream and succeeds.
        let ok = format!(
            "http://{proxy}/git/allowed.test/r.git/info/refs?service=git-upload-pack"
        );
        let resp = plain_client().get(&ok).send().await.unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn e2e_body_cap_returns_413() {
        let (proxy, _up) = spawn_test_proxy(|c| c.max_body = 10).await;
        // upstream "big" path returns a 100-byte body with Content-Length → over cap.
        let url = format!(
            "http://{proxy}/git/upstream.test/big.git/info/refs?service=git-upload-pack"
        );
        let resp = plain_client().get(&url).send().await.unwrap();
        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn e2e_rate_limit_returns_429() {
        let (proxy, _up) = spawn_test_proxy(|c| {
            c.rate_burst = 2;
            c.rate_per_sec = 0.0; // no refill during the burst
        })
        .await;
        let url = format!(
            "http://{proxy}/git/upstream.test/r.git/info/refs?service=git-upload-pack"
        );
        let client = plain_client();
        let mut saw_429 = false;
        for _ in 0..6 {
            let resp = client.get(&url).send().await.unwrap();
            if resp.status() == StatusCode::TOO_MANY_REQUESTS {
                saw_429 = true;
                break;
            }
        }
        assert!(saw_429, "expected a 429 after exhausting the burst");
    }

    #[test]
    fn empty_and_full_bodies_build() {
        // Guard: the body helpers construct the expected ProxyBody type.
        let _e = empty_body();
        let _f = full_body(Bytes::from_static(b"hi"));
    }
}
