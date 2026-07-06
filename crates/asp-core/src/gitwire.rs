//! gitwire — the pure, sans-IO git smart-HTTP protocol layer (§2 git-bridge).
//!
//! **Bytes in, bytes out.** This module owns pkt-line framing and the
//! git-protocol-v2 request/response shapes (`ls-refs`, `fetch`) spoken over HTTPS
//! to GitHub/GitLab/Gitea, plus the v0/v1 `receive-pack`/`send-pack` framing used
//! for push. It never touches the network, the filesystem, tokio, or `reqwest`;
//! the transport (native `reqwest`, browser `fetch()` through the relay proxy, or
//! the SSH-spawn path) lives above it, exactly the way [`crate::session`] owns the
//! ASP protocol while [`crate::iroh_net`]/`iroh_wasm` own the byte transport.
//!
//! Everything here compiles to `wasm32` unchanged: no `cfg(not(wasm32))`
//! dependency is referenced, only `core`/`alloc`/`std` string+byte machinery.
//!
//! ## Layers
//! * **pkt-line** — [`pkt_line`], [`flush_pkt`], [`delim_pkt`], [`response_end_pkt`],
//!   and the incremental [`PktReader`] parser yielding [`Pkt`].
//! * **protocol v2 (upload-pack)** — [`parse_capability_advertisement`],
//!   [`build_ls_refs`]/[`parse_ls_refs_response`], [`FetchRequest`]/[`build_fetch`]
//!   and [`FetchResponseParser`] with side-band-64k demux.
//! * **v0/v1 (receive-pack / push)** — [`parse_receive_pack_advertisement`],
//!   [`build_update_request`], [`parse_report_status`].
//! * **URLs** — [`parse_git_url`], [`normalize_https_remote`], and the endpoint
//!   builders ([`info_refs_url`], [`upload_pack_url`], …).

use crate::error::AspError;

/// git's `agent=` string for this build — read from the crate version at compile
/// time so a wire capture is attributable to an asp release.
const AGENT: &str = concat!("asp/", env!("CARGO_PKG_VERSION"));

/// Maximum pkt-line *payload* (data after the 4-byte length prefix).
pub const MAX_PKT_PAYLOAD: usize = 65516;
/// Maximum pkt-line total length including the 4-byte prefix (`0xfff0`).
pub const MAX_PKT_LINE: usize = 65520;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Every parser here returns this typed error rather than panicking or returning
/// a stringly-typed `AspError`, so the fetch/push driver can branch on the cause
/// (e.g. distinguish a remote fatal from a framing bug). `Display` is hand-rolled
/// to match the rest of `asp-core`'s conventions and to stay dependency-light.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitWireError {
    /// Input ended in the middle of a pkt-line (short length prefix or short body).
    Truncated,
    /// The 4-byte length prefix was not valid hex, or was the reserved value `0003`.
    InvalidPktLen(String),
    /// A pkt-line claimed a length above [`MAX_PKT_LINE`].
    Oversize(usize),
    /// A text pkt-line held bytes that are not valid UTF-8.
    Utf8,
    /// A structural protocol violation (unexpected section, missing header, …).
    Protocol(String),
    /// The advertisement was git protocol v0/v1, not v2 (names what was seen).
    UnsupportedVersion(String),
    /// The repo uses an object format we do not support (e.g. `sha256`).
    UnsupportedObjectFormat(String),
    /// The remote sent a fatal error — side-band band 3, or a `unpack`/`ng` failure
    /// surfaced as an error by the caller.
    RemoteError(String),
}

impl std::fmt::Display for GitWireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GitWireError::Truncated => write!(f, "gitwire: truncated pkt-line input"),
            GitWireError::InvalidPktLen(s) => write!(f, "gitwire: invalid pkt-line length: {s}"),
            GitWireError::Oversize(n) => {
                write!(f, "gitwire: pkt-line length {n} exceeds maximum {MAX_PKT_LINE}")
            }
            GitWireError::Utf8 => write!(f, "gitwire: invalid utf-8 in text pkt-line"),
            GitWireError::Protocol(s) => write!(f, "gitwire: protocol error: {s}"),
            GitWireError::UnsupportedVersion(s) => {
                write!(f, "gitwire: unsupported git protocol: {s}")
            }
            GitWireError::UnsupportedObjectFormat(s) => {
                write!(f, "gitwire: unsupported object-format '{s}' (only sha1)")
            }
            GitWireError::RemoteError(s) => write!(f, "gitwire: remote error: {s}"),
        }
    }
}

impl std::error::Error for GitWireError {}

impl From<GitWireError> for AspError {
    fn from(e: GitWireError) -> Self {
        AspError::Protocol(e.to_string())
    }
}

/// Result alias for the gitwire layer.
pub type GitResult<T> = Result<T, GitWireError>;

fn utf8str(b: &[u8]) -> GitResult<&str> {
    std::str::from_utf8(b).map_err(|_| GitWireError::Utf8)
}

// ---------------------------------------------------------------------------
// pkt-line encoding
// ---------------------------------------------------------------------------

/// Encode one data pkt-line: a 4-hex length prefix (length *includes* the 4 bytes)
/// followed by `payload`. Panics if `payload` exceeds [`MAX_PKT_PAYLOAD`] — that is
/// a programmer error; all wire callers here emit short command/oid lines.
pub fn pkt_line(payload: &[u8]) -> Vec<u8> {
    assert!(
        payload.len() <= MAX_PKT_PAYLOAD,
        "pkt-line payload {} exceeds maximum {}",
        payload.len(),
        MAX_PKT_PAYLOAD
    );
    let len = payload.len() + 4;
    let mut v = Vec::with_capacity(len);
    v.extend_from_slice(format!("{len:04x}").as_bytes());
    v.extend_from_slice(payload);
    v
}

/// The flush-pkt `0000` — ends a section / request.
pub fn flush_pkt() -> &'static [u8] {
    b"0000"
}
/// The delim-pkt `0001` — separates command+capabilities from arguments (proto v2).
pub fn delim_pkt() -> &'static [u8] {
    b"0001"
}
/// The response-end-pkt `0002` — terminates a stateless-RPC response (proto v2).
pub fn response_end_pkt() -> &'static [u8] {
    b"0002"
}

/// Strip a single trailing `\n` from a data pkt-line's payload. Git text lines
/// conventionally end in a newline that may or may not be present; this normalizes
/// them for comparison without discarding a newline the payload actually needs.
pub fn pkt_text(data: &[u8]) -> &[u8] {
    match data.last() {
        Some(b'\n') => &data[..data.len() - 1],
        _ => data,
    }
}

fn pkt_str_line(s: &str) -> Vec<u8> {
    pkt_line(s.as_bytes())
}

// ---------------------------------------------------------------------------
// pkt-line decoding
// ---------------------------------------------------------------------------

/// One decoded pkt-line. `Data` borrows the payload directly from the input slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pkt<'a> {
    /// A normal data pkt-line (length ≥ 4). The slice is the payload after the prefix.
    Data(&'a [u8]),
    /// flush-pkt `0000`.
    Flush,
    /// delim-pkt `0001`.
    Delim,
    /// response-end-pkt `0002`.
    ResponseEnd,
}

/// Incremental pkt-line parser over a byte slice. Yields `Result<Pkt, _>` items;
/// on the first error it reports the error once and then stops (`None`), so a caller
/// can `for item in reader { let pkt = item?; … }` without looping on bad input.
/// A clean end (offset == len) yields `None`; a partial trailing pkt yields one
/// [`GitWireError::Truncated`].
pub struct PktReader<'a> {
    buf: &'a [u8],
    pos: usize,
    done: bool,
}

impl<'a> PktReader<'a> {
    /// Start a reader at the beginning of `buf`.
    pub fn new(buf: &'a [u8]) -> Self {
        PktReader { buf, pos: 0, done: false }
    }
    /// Byte offset consumed so far (useful to hand the packfile tail to a decoder).
    pub fn offset(&self) -> usize {
        self.pos
    }
}

impl<'a> Iterator for PktReader<'a> {
    type Item = GitResult<Pkt<'a>>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done || self.pos >= self.buf.len() {
            return None;
        }
        let rest = &self.buf[self.pos..];
        if rest.len() < 4 {
            self.done = true;
            return Some(Err(GitWireError::Truncated));
        }
        let n = match parse_hex4(&rest[..4]) {
            Ok(n) => n,
            Err(e) => {
                self.done = true;
                return Some(Err(e));
            }
        };
        match n {
            0 => {
                self.pos += 4;
                Some(Ok(Pkt::Flush))
            }
            1 => {
                self.pos += 4;
                Some(Ok(Pkt::Delim))
            }
            2 => {
                self.pos += 4;
                Some(Ok(Pkt::ResponseEnd))
            }
            3 => {
                self.done = true;
                Some(Err(GitWireError::InvalidPktLen("0003 is reserved".into())))
            }
            n => {
                if n > MAX_PKT_LINE {
                    self.done = true;
                    return Some(Err(GitWireError::Oversize(n)));
                }
                if rest.len() < n {
                    self.done = true;
                    return Some(Err(GitWireError::Truncated));
                }
                let data = &rest[4..n];
                self.pos += n;
                Some(Ok(Pkt::Data(data)))
            }
        }
    }
}

fn parse_hex4(b: &[u8]) -> GitResult<usize> {
    if b.len() != 4 {
        return Err(GitWireError::Truncated);
    }
    if !b.iter().all(u8::is_ascii_hexdigit) {
        return Err(GitWireError::InvalidPktLen(format!(
            "non-hex '{}'",
            String::from_utf8_lossy(b)
        )));
    }
    // Safe: all four bytes are ascii hex digits.
    let s = std::str::from_utf8(b).unwrap();
    usize::from_str_radix(s, 16).map_err(|_| GitWireError::InvalidPktLen("overflow".into()))
}

/// Decode a whole (small) pkt-line stream into a vector. Used by the advertisement
/// and report parsers where the body fits comfortably in memory; the packfile path
/// streams via [`FetchResponseParser`] instead.
fn collect_pkts(body: &[u8]) -> GitResult<Vec<Pkt<'_>>> {
    PktReader::new(body).collect()
}

// ---------------------------------------------------------------------------
// URL endpoints & parsing
// ---------------------------------------------------------------------------

/// `<base>/info/refs?service=git-upload-pack` — the smart-HTTP capability probe.
/// Send it with the header `Git-Protocol: version=2` to request protocol v2.
pub fn info_refs_url(base: &str) -> String {
    format!("{}/info/refs?service=git-upload-pack", base.trim_end_matches('/'))
}
/// `<base>/git-upload-pack` — the fetch (ls-refs / fetch) POST endpoint.
pub fn upload_pack_url(base: &str) -> String {
    format!("{}/git-upload-pack", base.trim_end_matches('/'))
}
/// `<base>/git-receive-pack` — the push POST endpoint (v0/v1).
pub fn receive_pack_url(base: &str) -> String {
    format!("{}/git-receive-pack", base.trim_end_matches('/'))
}
/// `<base>/info/refs?service=git-receive-pack` — the push ref advertisement probe.
pub fn receive_pack_info_refs_url(base: &str) -> String {
    format!("{}/info/refs?service=git-receive-pack", base.trim_end_matches('/'))
}

/// Normalize an HTTPS git remote for storage: require the `https://` scheme, strip
/// a trailing `/`, and reject credentials embedded in the URL. Deliberately does
/// **not** add `.git` — GitHub accepts both, and rewriting the user's URL is
/// surprising.
pub fn normalize_https_remote(url: &str) -> GitResult<String> {
    let s = url.trim();
    let rest = s
        .strip_prefix("https://")
        .ok_or_else(|| GitWireError::Protocol(format!("remote must be https://, got '{s}'")))?;
    let authority = rest.split('/').next().unwrap_or("");
    if authority.is_empty() {
        return Err(GitWireError::Protocol("remote is missing a host".into()));
    }
    if authority.contains('@') {
        return Err(GitWireError::Protocol(
            "credentials must not be embedded in the URL; pass a token separately".into(),
        ));
    }
    Ok(s.trim_end_matches('/').to_string())
}

/// A parsed git remote source, used for CLI auto-detection of the clone source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitUrl {
    /// `https://host[:port]/path[.git]` — no userinfo permitted.
    Https { base: String },
    /// `ssh://[user@]host[:port]/path` or scp-like `[user@]host:path`.
    Ssh { user: Option<String>, host: String, port: Option<u16>, path: String },
}

/// True iff `input` parses as a git URL (the CLI tries this before `parse_peer`,
/// since git-URL syntax is unambiguous while iroh tickets / node-ids are not).
pub fn looks_like_git_url(input: &str) -> bool {
    parse_git_url(input).is_some()
}

/// Parse `input` as an HTTPS, `ssh://`, or scp-like git URL, or return `None`.
///
/// Intentionally strict so CLI source auto-detection does not mistake a local path
/// or an iroh ticket / 64-hex node-id for a git URL:
/// * `https://` requires a host and rejects embedded userinfo (credentials).
/// * `http://` / `git://` / `file://` and other schemes → `None`.
/// * scp-like `host:path` is recognized only when the colon precedes any slash
///   **and** the host looks like a real host (has a dot, or a `user@`, or is
///   `localhost`) — so `C:\dir`, `word:word`, plain paths, and hex strings all fail.
pub fn parse_git_url(input: &str) -> Option<GitUrl> {
    let s = input.trim();
    if s.is_empty() {
        return None;
    }

    if let Some(rest) = s.strip_prefix("https://") {
        let authority = rest.split('/').next().unwrap_or("");
        if authority.is_empty() || authority.contains('@') {
            return None;
        }
        let host = authority.split(':').next().unwrap_or("");
        if !is_hostish(host) {
            return None;
        }
        return Some(GitUrl::Https { base: s.trim_end_matches('/').to_string() });
    }

    if let Some(rest) = s.strip_prefix("ssh://") {
        return parse_ssh_authority(rest);
    }

    // Any other explicit scheme (http, git, file, …) is rejected.
    if s.contains("://") {
        return None;
    }

    // scp-like: [user@]host:path — colon must precede any slash.
    if let Some(colon) = s.find(':') {
        let before = &s[..colon];
        let after = &s[colon + 1..];
        if before.contains('/') || before.contains('\\') || after.is_empty() {
            return None;
        }
        let (user, host) = match before.split_once('@') {
            Some((u, h)) => (Some(u.to_string()), h.to_string()),
            None => (None, before.to_string()),
        };
        if !is_hostish(&host) {
            return None;
        }
        // Guard against `word:word`: a bare host must look like a real remote.
        if user.is_none() && !host.contains('.') && host != "localhost" {
            return None;
        }
        return Some(GitUrl::Ssh { user, host, port: None, path: after.to_string() });
    }

    None
}

fn parse_ssh_authority(rest: &str) -> Option<GitUrl> {
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    if authority.is_empty() || path.is_empty() {
        return None;
    }
    let (user, hostport) = match authority.split_once('@') {
        Some((u, h)) => (Some(u.to_string()), h),
        None => (None, authority),
    };
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => {
            (h.to_string(), p.parse::<u16>().ok())
        }
        _ => (hostport.to_string(), None),
    };
    if !is_hostish(&host) {
        return None;
    }
    Some(GitUrl::Ssh { user, host, port, path: path.to_string() })
}

fn is_hostish(h: &str) -> bool {
    !h.is_empty()
        && h.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-' || b == b'_')
        && h.bytes().any(|b| b.is_ascii_alphanumeric())
}

// ---------------------------------------------------------------------------
// protocol v2: capability advertisement
// ---------------------------------------------------------------------------

/// The object hash format a repo uses. Only sha1 is supported; sha256 repos are
/// rejected at advertisement parse time with [`GitWireError::UnsupportedObjectFormat`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectFormat {
    /// 40-hex-digit SHA-1 object ids (the near-universal default).
    Sha1,
}

/// Parsed protocol-v2 capability advertisement: the `version 2` line's following
/// capability pkt-lines, each `name` or `name=value`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CapabilityAdvert {
    /// Capabilities in advertised order, `(name, Some(value))` or `(name, None)`.
    pub caps: Vec<(String, Option<String>)>,
}

impl CapabilityAdvert {
    /// Whether capability `name` was advertised (with or without a value).
    pub fn supports(&self, name: &str) -> bool {
        self.caps.iter().any(|(k, _)| k == name)
    }
    /// The value advertised for `name`, if it carried one (`name=value`).
    pub fn value(&self, name: &str) -> Option<&str> {
        self.caps
            .iter()
            .find(|(k, _)| k == name)
            .and_then(|(_, v)| v.as_deref())
    }
    /// The repo's object format, defaulting to sha1 when unadvertised. Errors on
    /// sha256 (unsupported) and any unrecognized value.
    pub fn object_format(&self) -> GitResult<ObjectFormat> {
        match self.value("object-format") {
            None | Some("sha1") => Ok(ObjectFormat::Sha1),
            Some(other) => Err(GitWireError::UnsupportedObjectFormat(other.to_string())),
        }
    }
}

/// Parse the body of `GET /info/refs?service=git-upload-pack` (with
/// `Git-Protocol: version=2`). Handles the smart-HTTP service-announcement prelude
/// (`# service=git-upload-pack` + flush) that precedes the `version 2` line, and
/// rejects a v0/v1 (dumb ref) advertisement with a message naming what was seen.
pub fn parse_capability_advertisement(body: &[u8]) -> GitResult<CapabilityAdvert> {
    let pkts = collect_pkts(body)?;
    let mut i = 0;

    // Optional smart-HTTP service prelude: "# service=git-upload-pack" then flush.
    if let Some(Pkt::Data(d)) = pkts.get(i) {
        if pkt_text(d).starts_with(b"# service=") {
            i += 1;
            match pkts.get(i) {
                Some(Pkt::Flush) => i += 1,
                _ => {
                    return Err(GitWireError::Protocol(
                        "expected flush after service header".into(),
                    ))
                }
            }
        }
    }

    // The version line. Anything but "version 2" is a v0/v1 server we can't speak.
    let vline = match pkts.get(i) {
        Some(Pkt::Data(d)) => pkt_text(d),
        _ => {
            return Err(GitWireError::UnsupportedVersion(
                "expected 'version 2' line, got flush/delim/eof".into(),
            ))
        }
    };
    if vline != b"version 2" {
        let seen = String::from_utf8_lossy(vline);
        return Err(GitWireError::UnsupportedVersion(format!(
            "expected 'version 2', saw '{seen}'"
        )));
    }
    i += 1;

    let mut caps = Vec::new();
    while let Some(p) = pkts.get(i) {
        match p {
            Pkt::Data(d) => {
                let s = utf8str(pkt_text(d))?;
                let (name, val) = match s.split_once('=') {
                    Some((k, v)) => (k.to_string(), Some(v.to_string())),
                    None => (s.to_string(), None),
                };
                caps.push((name, val));
                i += 1;
            }
            Pkt::Flush => break,
            _ => break,
        }
    }
    Ok(CapabilityAdvert { caps })
}

// ---------------------------------------------------------------------------
// protocol v2: ls-refs
// ---------------------------------------------------------------------------

/// Build an `ls-refs` request body (POST to `git-upload-pack`). Emits the command
/// and `agent` capability, a delim, then `peel`, `symrefs`, one `ref-prefix <p>`
/// per requested prefix, and a flush. Empty `refs_prefixes` lists all refs.
pub fn build_ls_refs(refs_prefixes: &[&str]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend(pkt_str_line("command=ls-refs\n"));
    out.extend(pkt_str_line(&format!("agent={AGENT}\n")));
    out.extend_from_slice(delim_pkt());
    out.extend(pkt_str_line("peel\n"));
    out.extend(pkt_str_line("symrefs\n"));
    for p in refs_prefixes {
        out.extend(pkt_str_line(&format!("ref-prefix {p}\n")));
    }
    out.extend_from_slice(flush_pkt());
    out
}

/// One ref from an `ls-refs` response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefInfo {
    /// The ref's object id (40-hex sha1).
    pub oid: String,
    /// The full ref name, e.g. `HEAD` or `refs/heads/main`.
    pub name: String,
    /// For a symbolic ref (`symrefs` requested): the ref it points at.
    pub symref_target: Option<String>,
    /// For an annotated tag (`peel` requested): the peeled commit oid.
    pub peeled: Option<String>,
}

/// Parse an `ls-refs` response: `<oid> <name>[ symref-target:<t>][ peeled:<oid>]`
/// lines terminated by a flush.
pub fn parse_ls_refs_response(body: &[u8]) -> GitResult<Vec<RefInfo>> {
    let mut refs = Vec::new();
    for item in PktReader::new(body) {
        match item? {
            Pkt::Data(d) => {
                let line = utf8str(pkt_text(d))?;
                let mut parts = line.split(' ');
                let oid = parts
                    .next()
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| GitWireError::Protocol("empty ref line".into()))?
                    .to_string();
                let name = parts
                    .next()
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| GitWireError::Protocol("ref line missing name".into()))?
                    .to_string();
                let mut symref_target = None;
                let mut peeled = None;
                for attr in parts {
                    if let Some(t) = attr.strip_prefix("symref-target:") {
                        symref_target = Some(t.to_string());
                    } else if let Some(t) = attr.strip_prefix("peeled:") {
                        peeled = Some(t.to_string());
                    }
                }
                refs.push(RefInfo { oid, name, symref_target, peeled });
            }
            Pkt::Flush => break,
            _ => {}
        }
    }
    Ok(refs)
}

// ---------------------------------------------------------------------------
// protocol v2: fetch
// ---------------------------------------------------------------------------

/// A protocol-v2 `fetch` request. All fields are optional negotiation inputs; the
/// driver fills `wants`/`haves` from the ref advert and the ingest ledger.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FetchRequest {
    /// `want <oid>` lines — the tips to fetch.
    pub wants: Vec<String>,
    /// `have <oid>` lines — objects we already hold (last-ingested sha on web).
    pub haves: Vec<String>,
    /// Append `done` to end negotiation and demand a packfile this round.
    pub done: bool,
    /// Request a thin pack (deltas may reference bases we already have).
    pub thin_pack: bool,
    /// Suppress band-2 progress.
    pub no_progress: bool,
    /// A partial-clone `filter` spec, e.g. `blob:none`.
    pub filter: Option<String>,
    /// `deepen <n>` for a shallow fetch.
    pub deepen: Option<u32>,
    /// `shallow <oid>` lines describing our shallow boundary.
    pub shallow: Vec<String>,
}

impl FetchRequest {
    /// Encode this request to pkt-lines (see [`build_fetch`]).
    pub fn build(&self) -> Vec<u8> {
        build_fetch(self)
    }
}

/// Build a protocol-v2 `fetch` request body: command + `agent`, delim, `ofs-delta`
/// (and `thin-pack`/`no-progress` if set), the want/have/shallow/deepen/filter
/// argument lines, an optional `done`, and a flush.
pub fn build_fetch(req: &FetchRequest) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend(pkt_str_line("command=fetch\n"));
    out.extend(pkt_str_line(&format!("agent={AGENT}\n")));
    out.extend_from_slice(delim_pkt());
    out.extend(pkt_str_line("ofs-delta\n"));
    if req.thin_pack {
        out.extend(pkt_str_line("thin-pack\n"));
    }
    if req.no_progress {
        out.extend(pkt_str_line("no-progress\n"));
    }
    for w in &req.wants {
        out.extend(pkt_str_line(&format!("want {w}\n")));
    }
    for h in &req.haves {
        out.extend(pkt_str_line(&format!("have {h}\n")));
    }
    for s in &req.shallow {
        out.extend(pkt_str_line(&format!("shallow {s}\n")));
    }
    if let Some(d) = req.deepen {
        out.extend(pkt_str_line(&format!("deepen {d}\n")));
    }
    if let Some(f) = &req.filter {
        out.extend(pkt_str_line(&format!("filter {f}\n")));
    }
    if req.done {
        out.extend(pkt_str_line("done\n"));
    }
    out.extend_from_slice(flush_pkt());
    out
}

/// The parsed result of a protocol-v2 `fetch` response.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FetchResponse {
    /// Object ids the server ACKed as common.
    pub acks: Vec<String>,
    /// Whether a `NAK` line appeared (no common objects).
    pub nak: bool,
    /// Whether the server said `ready` (it will send a packfile this round).
    pub ready: bool,
    /// `shallow <oid>` lines from a shallow-info section.
    pub shallow: Vec<String>,
    /// `unshallow <oid>` lines from a shallow-info section.
    pub unshallow: Vec<String>,
    /// The reassembled raw packfile bytes (band-1 payloads concatenated).
    pub pack: Vec<u8>,
    /// Collected band-2 progress bytes.
    pub progress: Vec<u8>,
    /// Whether a `packfile` section header was seen (false for a negotiation-only
    /// `acknowledgments`+flush round where the client must send `done` next).
    pub saw_packfile: bool,
}

/// Walks a protocol-v2 `fetch` response section by section, demultiplexing the
/// side-band-64k packfile. See [`FetchResponseParser::parse`].
pub struct FetchResponseParser;

impl FetchResponseParser {
    /// Parse a whole fetch-response body, discarding progress output.
    pub fn parse(body: &[u8]) -> GitResult<FetchResponse> {
        Self::parse_with(body, |_| {})
    }

    /// Parse a whole fetch-response body, streaming band-2 progress to `on_progress`
    /// (progress is also collected into [`FetchResponse::progress`]).
    ///
    /// Section order per the v2 grammar: an optional `acknowledgments` section
    /// (ACK/NAK/`ready`, ended by delim when `ready`, or by flush when negotiation
    /// must continue), an optional `shallow-info` section, then the `packfile`
    /// section whose data pkts are band-tagged (1=pack, 2=progress, 3=fatal). A
    /// band-3 frame becomes [`GitWireError::RemoteError`]. The stream ends at the
    /// packfile's flush, a response-end pkt, or EOF.
    pub fn parse_with<F: FnMut(&[u8])>(body: &[u8], mut on_progress: F) -> GitResult<FetchResponse> {
        #[derive(PartialEq, Eq, Clone, Copy)]
        enum Sec {
            None,
            Ack,
            Shallow,
            Wanted,
            PackUris,
            Pack,
        }

        let mut resp = FetchResponse::default();
        let mut sec = Sec::None;

        for item in PktReader::new(body) {
            match item? {
                Pkt::Flush => {
                    // Packfile flush → done. acknowledgments+flush → negotiation
                    // round with no packfile (client must send `done`).
                    if sec == Sec::Pack || sec == Sec::Ack {
                        break;
                    }
                    sec = Sec::None;
                }
                Pkt::Delim => sec = Sec::None,
                Pkt::ResponseEnd => break,
                Pkt::Data(d) => match sec {
                    Sec::None => {
                        let header = pkt_text(d);
                        sec = match header {
                            b"acknowledgments" => Sec::Ack,
                            b"shallow-info" => Sec::Shallow,
                            b"wanted-refs" => Sec::Wanted,
                            b"packfile-uris" => Sec::PackUris,
                            b"packfile" => {
                                resp.saw_packfile = true;
                                Sec::Pack
                            }
                            other => {
                                return Err(GitWireError::Protocol(format!(
                                    "unknown fetch section header '{}'",
                                    String::from_utf8_lossy(other)
                                )))
                            }
                        };
                    }
                    Sec::Ack => {
                        let line = utf8str(pkt_text(d))?;
                        if line == "NAK" {
                            resp.nak = true;
                        } else if let Some(oid) = line.strip_prefix("ACK ") {
                            resp.acks.push(oid.to_string());
                        } else if line == "ready" {
                            resp.ready = true;
                        } else {
                            return Err(GitWireError::Protocol(format!(
                                "unexpected acknowledgments line '{line}'"
                            )));
                        }
                    }
                    Sec::Shallow => {
                        let line = utf8str(pkt_text(d))?;
                        if let Some(oid) = line.strip_prefix("shallow ") {
                            resp.shallow.push(oid.to_string());
                        } else if let Some(oid) = line.strip_prefix("unshallow ") {
                            resp.unshallow.push(oid.to_string());
                        } else {
                            return Err(GitWireError::Protocol(format!(
                                "unexpected shallow-info line '{line}'"
                            )));
                        }
                    }
                    // Not needed by the bridge; consumed but ignored.
                    Sec::Wanted | Sec::PackUris => {}
                    Sec::Pack => {
                        if d.is_empty() {
                            return Err(GitWireError::Protocol("empty packfile pkt-line".into()));
                        }
                        let band = d[0];
                        let payload = &d[1..];
                        match band {
                            1 => resp.pack.extend_from_slice(payload),
                            2 => {
                                resp.progress.extend_from_slice(payload);
                                on_progress(payload);
                            }
                            3 => {
                                return Err(GitWireError::RemoteError(
                                    String::from_utf8_lossy(payload).into_owned(),
                                ))
                            }
                            other => {
                                return Err(GitWireError::Protocol(format!(
                                    "invalid side-band band {other}"
                                )))
                            }
                        }
                    }
                },
            }
        }
        Ok(resp)
    }
}

// ---------------------------------------------------------------------------
// v0/v1: receive-pack (push)
// ---------------------------------------------------------------------------

/// Parsed `git-receive-pack` ref advertisement (protocol v0/v1 — receive-pack does
/// not speak v2 for push in practice).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReceivePackAdvert {
    /// `(oid, refname)` for each advertised ref. Empty for a fresh/unborn repo.
    pub refs: Vec<(String, String)>,
    /// The server's capability list (from after the NUL on the first ref line).
    pub capabilities: Vec<String>,
}

/// Parse the body of `GET /info/refs?service=git-receive-pack`. Handles the
/// `# service=git-receive-pack` prelude, the first-ref `\0`-capability convention,
/// and the empty-repo `0{40} capabilities^{}\0<caps>` (unborn) form.
pub fn parse_receive_pack_advertisement(body: &[u8]) -> GitResult<ReceivePackAdvert> {
    let pkts = collect_pkts(body)?;
    let mut i = 0;

    if let Some(Pkt::Data(d)) = pkts.get(i) {
        if pkt_text(d).starts_with(b"# service=") {
            i += 1;
            if let Some(Pkt::Flush) = pkts.get(i) {
                i += 1;
            }
        }
    }

    let mut advert = ReceivePackAdvert::default();
    let mut first = true;
    while let Some(p) = pkts.get(i) {
        match p {
            Pkt::Data(d) => {
                let line = pkt_text(d);
                if first {
                    first = false;
                    // Capabilities ride after a NUL on the very first ref line.
                    let (refpart, cappart) = match line.iter().position(|&b| b == 0) {
                        Some(z) => (&line[..z], Some(&line[z + 1..])),
                        None => (line, None),
                    };
                    if let Some(c) = cappart {
                        advert.capabilities = String::from_utf8_lossy(c)
                            .split(' ')
                            .filter(|s| !s.is_empty())
                            .map(|s| s.to_string())
                            .collect();
                    }
                    parse_ref_line(refpart, &mut advert.refs)?;
                } else {
                    parse_ref_line(line, &mut advert.refs)?;
                }
                i += 1;
            }
            Pkt::Flush => break,
            _ => i += 1,
        }
    }
    Ok(advert)
}

fn parse_ref_line(line: &[u8], refs: &mut Vec<(String, String)>) -> GitResult<()> {
    let s = utf8str(line)?;
    let mut it = s.splitn(2, ' ');
    let oid = it.next().unwrap_or("");
    let name = it.next().unwrap_or("");
    // Unborn / empty-repo sentinel: no real ref, only capabilities.
    if name == "capabilities^{}" {
        return Ok(());
    }
    if oid.is_empty() || name.is_empty() {
        return Err(GitWireError::Protocol(format!("malformed ref line '{s}'")));
    }
    refs.push((oid.to_string(), name.to_string()));
    Ok(())
}

/// Build a single-ref update command list for `git-receive-pack`. Capabilities
/// ride after a NUL on the (first and only) command line. The caller appends the
/// packfile bytes *after* the returned framing.
pub fn build_update_request(
    old_oid: &str,
    new_oid: &str,
    refname: &str,
    caps: &[&str],
) -> Vec<u8> {
    let line = if caps.is_empty() {
        format!("{old_oid} {new_oid} {refname}\n")
    } else {
        format!("{old_oid} {new_oid} {refname}\0{}\n", caps.join(" "))
    };
    let mut out = pkt_line(line.as_bytes());
    out.extend_from_slice(flush_pkt());
    out
}

/// The outcome of a push, from the `report-status` / `report-status-v2` response.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PushReport {
    /// Whether the server unpacked our pack successfully (`unpack ok`).
    pub unpack_ok: bool,
    /// Per-ref result: `Ok(())` for `ok <ref>`, `Err(reason)` for `ng <ref> <reason>`.
    pub ref_statuses: Vec<(String, Result<(), String>)>,
}

/// Parse a `report-status` or `report-status-v2` push response. When `sideband`,
/// the report is wrapped in side-band-64k (band 1 = report, 2 = progress,
/// 3 = fatal); it is demuxed first. `option` lines (report-status-v2) are ignored.
pub fn parse_report_status(body: &[u8], sideband: bool) -> GitResult<PushReport> {
    let demuxed;
    let bytes: &[u8] = if sideband {
        demuxed = demux_sideband(body)?;
        &demuxed
    } else {
        body
    };

    let mut report = PushReport::default();
    let mut seen_unpack = false;
    for item in PktReader::new(bytes) {
        match item? {
            Pkt::Data(d) => {
                let line = utf8str(pkt_text(d))?;
                if let Some(rest) = line.strip_prefix("unpack ") {
                    seen_unpack = true;
                    report.unpack_ok = rest == "ok";
                } else if let Some(rest) = line.strip_prefix("ok ") {
                    report.ref_statuses.push((rest.to_string(), Ok(())));
                } else if let Some(rest) = line.strip_prefix("ng ") {
                    let mut it = rest.splitn(2, ' ');
                    let name = it.next().unwrap_or("").to_string();
                    let reason = it.next().unwrap_or("").to_string();
                    report.ref_statuses.push((name, Err(reason)));
                } else if line.starts_with("option ") {
                    // report-status-v2 option lines carry extra metadata; ignore.
                }
                // Unknown lines are ignored leniently.
            }
            Pkt::Flush => break,
            _ => {}
        }
    }
    if !seen_unpack {
        return Err(GitWireError::Protocol("report-status missing unpack line".into()));
    }
    Ok(report)
}

/// Reassemble the band-1 payload of a side-band-64k stream, erroring on a band-3
/// fatal. Band-2 progress is discarded.
fn demux_sideband(body: &[u8]) -> GitResult<Vec<u8>> {
    let mut out = Vec::new();
    for item in PktReader::new(body) {
        if let Pkt::Data(d) = item? {
            if d.is_empty() {
                continue;
            }
            match d[0] {
                1 => out.extend_from_slice(&d[1..]),
                2 => {}
                3 => {
                    return Err(GitWireError::RemoteError(
                        String::from_utf8_lossy(&d[1..]).into_owned(),
                    ))
                }
                other => {
                    return Err(GitWireError::Protocol(format!(
                        "invalid side-band band {other}"
                    )))
                }
            }
        }
    }
    Ok(out)
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // --- test helpers: build higher-level fixtures from the (separately-verified)
    // pkt_line encoder ---------------------------------------------------------

    fn dl(s: &str) -> Vec<u8> {
        pkt_line(s.as_bytes())
    }
    fn band(n: u8, payload: &[u8]) -> Vec<u8> {
        let mut v = vec![n];
        v.extend_from_slice(payload);
        pkt_line(&v)
    }
    /// Concatenate framing fragments into one body.
    fn body(parts: &[&[u8]]) -> Vec<u8> {
        let mut v = Vec::new();
        for p in parts {
            v.extend_from_slice(p);
        }
        v
    }

    const OID_A: &str = "1111111111111111111111111111111111111111";
    const OID_B: &str = "2222222222222222222222222222222222222222";
    const OID_TAG: &str = "3333333333333333333333333333333333333333";
    const OID_ZERO: &str = "0000000000000000000000000000000000000000";

    // --- pkt-line encode: hardcoded vectors (independent of the decoder) -------

    #[test]
    fn pkt_line_encode_vectors() {
        assert_eq!(pkt_line(b"a"), b"0005a");
        assert_eq!(pkt_line(b"hello\n"), b"000ahello\n");
        assert_eq!(pkt_line(b""), b"0004");
        assert_eq!(flush_pkt(), b"0000");
        assert_eq!(delim_pkt(), b"0001");
        assert_eq!(response_end_pkt(), b"0002");
    }

    #[test]
    fn pkt_line_max_boundary_roundtrip() {
        let payload = vec![b'x'; MAX_PKT_PAYLOAD];
        let framed = pkt_line(&payload);
        assert_eq!(&framed[..4], b"fff0"); // 65516 + 4 = 65520 = 0xfff0
        let pkts: Vec<_> = PktReader::new(&framed).collect::<GitResult<_>>().unwrap();
        assert_eq!(pkts, vec![Pkt::Data(&payload)]);
    }

    #[test]
    #[should_panic]
    fn pkt_line_oversize_payload_panics() {
        let _ = pkt_line(&vec![0u8; MAX_PKT_PAYLOAD + 1]);
    }

    // --- pkt-line decode -------------------------------------------------------

    #[test]
    fn pkt_reader_basic_stream() {
        let input = body(&[b"0005a", flush_pkt(), delim_pkt(), response_end_pkt()]);
        let pkts: Vec<_> = PktReader::new(&input).collect::<GitResult<_>>().unwrap();
        assert_eq!(
            pkts,
            vec![Pkt::Data(b"a"), Pkt::Flush, Pkt::Delim, Pkt::ResponseEnd]
        );
    }

    #[test]
    fn pkt_reader_clean_eof_is_none() {
        assert_eq!(PktReader::new(b"").next(), None);
        assert_eq!(PktReader::new(flush_pkt()).count(), 1);
    }

    #[test]
    fn pkt_reader_truncated_short_prefix() {
        // len prefix itself is short
        let err = PktReader::new(b"005").next().unwrap().unwrap_err();
        assert_eq!(err, GitWireError::Truncated);
    }

    #[test]
    fn pkt_reader_truncated_short_body() {
        // claims 5 bytes total but only 4 present
        let err = PktReader::new(b"0005").next().unwrap().unwrap_err();
        assert_eq!(err, GitWireError::Truncated);
        // claims 16 but only 6 present
        let err = PktReader::new(b"0010aa").next().unwrap().unwrap_err();
        assert_eq!(err, GitWireError::Truncated);
    }

    #[test]
    fn pkt_reader_bad_hex() {
        let err = PktReader::new(b"zzzzdata").next().unwrap().unwrap_err();
        assert!(matches!(err, GitWireError::InvalidPktLen(_)));
    }

    #[test]
    fn pkt_reader_reserved_0003() {
        let err = PktReader::new(b"0003").next().unwrap().unwrap_err();
        assert!(matches!(err, GitWireError::InvalidPktLen(_)));
    }

    #[test]
    fn pkt_reader_oversize() {
        // 0xfff1 = 65521 > MAX_PKT_LINE; reported before any truncation check.
        let err = PktReader::new(b"fff1").next().unwrap().unwrap_err();
        assert_eq!(err, GitWireError::Oversize(0xfff1));
    }

    #[test]
    fn pkt_reader_stops_after_error() {
        // After an error the iterator yields None (does not loop).
        let mut r = PktReader::new(b"0003");
        assert!(r.next().unwrap().is_err());
        assert_eq!(r.next(), None);
    }

    #[test]
    fn pkt_text_strips_one_newline() {
        assert_eq!(pkt_text(b"line\n"), b"line");
        assert_eq!(pkt_text(b"line\n\n"), b"line\n");
        assert_eq!(pkt_text(b"line"), b"line");
        assert_eq!(pkt_text(b""), b"");
    }

    // --- capability advertisement ---------------------------------------------

    fn github_upload_pack_advert() -> Vec<u8> {
        body(&[
            &dl("# service=git-upload-pack\n"),
            flush_pkt(),
            &dl("version 2\n"),
            &dl("agent=git/2.40.1\n"),
            &dl("ls-refs=unborn\n"),
            &dl("fetch=shallow wait-for-done filter\n"),
            &dl("object-format=sha1\n"),
            &dl("0001\n"), // a literal cap value containing a digit; not a delim
            flush_pkt(),
        ])
    }

    #[test]
    fn parse_capability_advert_github_shape() {
        let a = parse_capability_advertisement(&github_upload_pack_advert()).unwrap();
        assert!(a.supports("ls-refs"));
        assert!(a.supports("fetch"));
        assert_eq!(a.value("agent"), Some("git/2.40.1"));
        assert_eq!(a.value("fetch"), Some("shallow wait-for-done filter"));
        assert_eq!(a.object_format().unwrap(), ObjectFormat::Sha1);
    }

    #[test]
    fn parse_capability_advert_no_prelude() {
        // Some transports omit the service prelude; the direct v2 form must parse.
        let b = body(&[&dl("version 2\n"), &dl("agent=x\n"), flush_pkt()]);
        let a = parse_capability_advertisement(&b).unwrap();
        assert_eq!(a.value("agent"), Some("x"));
    }

    #[test]
    fn parse_capability_advert_rejects_v0_v1() {
        // A v0/v1 dumb ref advertisement starts with an oid+ref, not "version 2".
        let b = body(&[
            &dl("# service=git-upload-pack\n"),
            flush_pkt(),
            &dl(&format!("{OID_A} refs/heads/main\0multi_ack agent=git/2.40\n")),
            flush_pkt(),
        ]);
        let err = parse_capability_advertisement(&b).unwrap_err();
        assert!(matches!(err, GitWireError::UnsupportedVersion(_)));

        let b1 = body(&[&dl("version 1\n"), flush_pkt()]);
        assert!(matches!(
            parse_capability_advertisement(&b1).unwrap_err(),
            GitWireError::UnsupportedVersion(_)
        ));
    }

    #[test]
    fn object_format_sha256_rejected() {
        let b = body(&[
            &dl("version 2\n"),
            &dl("object-format=sha256\n"),
            flush_pkt(),
        ]);
        let a = parse_capability_advertisement(&b).unwrap();
        assert!(matches!(
            a.object_format().unwrap_err(),
            GitWireError::UnsupportedObjectFormat(_)
        ));
    }

    // --- ls-refs ---------------------------------------------------------------

    #[test]
    fn build_ls_refs_shape() {
        let req = build_ls_refs(&["refs/heads/", "refs/tags/"]);
        let pkts: Vec<_> = PktReader::new(&req).collect::<GitResult<_>>().unwrap();
        assert_eq!(pkts[0], Pkt::Data(b"command=ls-refs\n"));
        assert!(matches!(pkts[1], Pkt::Data(d) if d.starts_with(b"agent=asp/")));
        assert_eq!(pkts[2], Pkt::Delim);
        assert_eq!(pkts[3], Pkt::Data(b"peel\n"));
        assert_eq!(pkts[4], Pkt::Data(b"symrefs\n"));
        assert_eq!(pkts[5], Pkt::Data(b"ref-prefix refs/heads/\n"));
        assert_eq!(pkts[6], Pkt::Data(b"ref-prefix refs/tags/\n"));
        assert_eq!(*pkts.last().unwrap(), Pkt::Flush);
    }

    #[test]
    fn parse_ls_refs_symref_and_peeled() {
        let b = body(&[
            &dl(&format!("{OID_A} HEAD symref-target:refs/heads/main\n")),
            &dl(&format!("{OID_A} refs/heads/main\n")),
            &dl(&format!("{OID_TAG} refs/tags/v1.0 peeled:{OID_B}\n")),
            flush_pkt(),
        ]);
        let refs = parse_ls_refs_response(&b).unwrap();
        assert_eq!(refs.len(), 3);
        assert_eq!(refs[0].name, "HEAD");
        assert_eq!(refs[0].symref_target.as_deref(), Some("refs/heads/main"));
        assert_eq!(refs[1].name, "refs/heads/main");
        assert_eq!(refs[2].peeled.as_deref(), Some(OID_B));
        assert_eq!(refs[2].oid, OID_TAG);
    }

    // --- fetch request ---------------------------------------------------------

    #[test]
    fn build_fetch_orders_sections() {
        let req = FetchRequest {
            wants: vec![OID_A.into()],
            haves: vec![OID_B.into()],
            done: true,
            thin_pack: true,
            no_progress: true,
            filter: Some("blob:none".into()),
            deepen: Some(1),
            shallow: vec![OID_TAG.into()],
        };
        let out = req.build();
        let pkts: Vec<_> = PktReader::new(&out).collect::<GitResult<_>>().unwrap();
        assert_eq!(pkts[0], Pkt::Data(b"command=fetch\n"));
        assert!(matches!(pkts[1], Pkt::Data(d) if d.starts_with(b"agent=asp/")));
        assert_eq!(pkts[2], Pkt::Delim);
        // Collect the argument lines as text for order-insensitive presence checks.
        let lines: Vec<String> = pkts
            .iter()
            .filter_map(|p| match p {
                Pkt::Data(d) => Some(String::from_utf8_lossy(pkt_text(d)).into_owned()),
                _ => None,
            })
            .collect();
        assert!(lines.iter().any(|l| l == "ofs-delta"));
        assert!(lines.iter().any(|l| l == "thin-pack"));
        assert!(lines.iter().any(|l| l == "no-progress"));
        assert!(lines.iter().any(|l| l == &format!("want {OID_A}")));
        assert!(lines.iter().any(|l| l == &format!("have {OID_B}")));
        assert!(lines.iter().any(|l| l == &format!("shallow {OID_TAG}")));
        assert!(lines.iter().any(|l| l == "deepen 1"));
        assert!(lines.iter().any(|l| l == "filter blob:none"));
        assert!(lines.iter().any(|l| l == "done"));
        assert_eq!(*pkts.last().unwrap(), Pkt::Flush);
    }

    #[test]
    fn build_fetch_minimal_omits_optionals() {
        let req = FetchRequest { wants: vec![OID_A.into()], ..Default::default() };
        let out = req.build();
        let s = String::from_utf8_lossy(&out);
        assert!(s.contains("ofs-delta"));
        assert!(!s.contains("thin-pack"));
        assert!(!s.contains("no-progress"));
        assert!(!s.contains("done"));
        assert!(!s.contains("deepen"));
    }

    // --- fetch response --------------------------------------------------------

    fn fetch_response_full() -> Vec<u8> {
        body(&[
            &dl("acknowledgments\n"),
            &dl(&format!("ACK {OID_A}\n")),
            &dl("ready\n"),
            delim_pkt(),
            &dl("shallow-info\n"),
            &dl(&format!("shallow {OID_B}\n")),
            delim_pkt(),
            &dl("packfile\n"),
            &band(1, b"PACK\x00\x00"),
            &band(2, b"Counting objects: 3\n"),
            &band(1, b"\x01\x02\x03tail"),
            flush_pkt(),
        ])
    }

    #[test]
    fn parse_fetch_response_full_sections() {
        let mut progress = Vec::new();
        let r = FetchResponseParser::parse_with(&fetch_response_full(), |p| {
            progress.extend_from_slice(p)
        })
        .unwrap();
        assert_eq!(r.acks, vec![OID_A.to_string()]);
        assert!(r.ready);
        assert!(!r.nak);
        assert_eq!(r.shallow, vec![OID_B.to_string()]);
        assert!(r.saw_packfile);
        assert_eq!(r.pack, b"PACK\x00\x00\x01\x02\x03tail");
        assert_eq!(r.progress, b"Counting objects: 3\n");
        assert_eq!(progress, b"Counting objects: 3\n");
    }

    #[test]
    fn parse_fetch_response_done_packfile_only() {
        // With done=true the server may skip acknowledgments entirely.
        let b = body(&[
            &dl("packfile\n"),
            &band(1, b"PACKDATA"),
            flush_pkt(),
        ]);
        let r = FetchResponseParser::parse(&b).unwrap();
        assert!(r.acks.is_empty());
        assert!(r.saw_packfile);
        assert_eq!(r.pack, b"PACKDATA");
    }

    #[test]
    fn parse_fetch_response_nak_only_negotiation_round() {
        // acknowledgments + NAK + flush: no packfile, client must send `done`.
        let b = body(&[&dl("acknowledgments\n"), &dl("NAK\n"), flush_pkt()]);
        let r = FetchResponseParser::parse(&b).unwrap();
        assert!(r.nak);
        assert!(!r.saw_packfile);
        assert!(r.pack.is_empty());
    }

    #[test]
    fn parse_fetch_response_sideband_error() {
        let b = body(&[
            &dl("packfile\n"),
            &band(1, b"PACK"),
            &band(3, b"fatal: bad object"),
            flush_pkt(),
        ]);
        let err = FetchResponseParser::parse(&b).unwrap_err();
        assert_eq!(err, GitWireError::RemoteError("fatal: bad object".into()));
    }

    #[test]
    fn parse_fetch_response_ends_on_response_end() {
        let b = body(&[
            &dl("packfile\n"),
            &band(1, b"PACK"),
            response_end_pkt(),
        ]);
        let r = FetchResponseParser::parse(&b).unwrap();
        assert_eq!(r.pack, b"PACK");
    }

    // --- receive-pack advertisement --------------------------------------------

    #[test]
    fn parse_receive_pack_advert_with_refs() {
        let b = body(&[
            &dl("# service=git-receive-pack\n"),
            flush_pkt(),
            &dl(&format!(
                "{OID_A} refs/heads/main\0report-status report-status-v2 side-band-64k agent=git/2.40\n"
            )),
            &dl(&format!("{OID_B} refs/heads/dev\n")),
            flush_pkt(),
        ]);
        let a = parse_receive_pack_advertisement(&b).unwrap();
        assert_eq!(a.refs.len(), 2);
        assert_eq!(a.refs[0], (OID_A.to_string(), "refs/heads/main".to_string()));
        assert_eq!(a.refs[1], (OID_B.to_string(), "refs/heads/dev".to_string()));
        assert!(a.capabilities.iter().any(|c| c == "report-status-v2"));
        assert!(a.capabilities.iter().any(|c| c == "side-band-64k"));
    }

    #[test]
    fn parse_receive_pack_advert_empty_repo() {
        let b = body(&[
            &dl("# service=git-receive-pack\n"),
            flush_pkt(),
            &dl(&format!(
                "{OID_ZERO} capabilities^{{}}\0report-status delete-refs side-band-64k\n"
            )),
            flush_pkt(),
        ]);
        let a = parse_receive_pack_advertisement(&b).unwrap();
        assert!(a.refs.is_empty(), "unborn repo advertises no refs");
        assert!(a.capabilities.iter().any(|c| c == "delete-refs"));
    }

    // --- update request --------------------------------------------------------

    #[test]
    fn build_update_request_with_caps() {
        let out = build_update_request(
            OID_ZERO,
            OID_A,
            "refs/heads/main",
            &["report-status-v2", "side-band-64k", "agent=asp/x"],
        );
        let pkts: Vec<_> = PktReader::new(&out).collect::<GitResult<_>>().unwrap();
        let expected = format!(
            "{OID_ZERO} {OID_A} refs/heads/main\0report-status-v2 side-band-64k agent=asp/x\n"
        );
        assert_eq!(pkts[0], Pkt::Data(expected.as_bytes()));
        assert_eq!(pkts[1], Pkt::Flush);
    }

    #[test]
    fn build_update_request_no_caps() {
        let out = build_update_request(OID_ZERO, OID_A, "refs/heads/main", &[]);
        let pkts: Vec<_> = PktReader::new(&out).collect::<GitResult<_>>().unwrap();
        let expected = format!("{OID_ZERO} {OID_A} refs/heads/main\n");
        assert_eq!(pkts[0], Pkt::Data(expected.as_bytes()));
        assert_eq!(pkts[1], Pkt::Flush);
    }

    // --- report-status ---------------------------------------------------------

    #[test]
    fn parse_report_status_plain_ok() {
        let b = body(&[&dl("unpack ok\n"), &dl("ok refs/heads/main\n"), flush_pkt()]);
        let r = parse_report_status(&b, false).unwrap();
        assert!(r.unpack_ok);
        assert_eq!(r.ref_statuses.len(), 1);
        assert_eq!(r.ref_statuses[0], ("refs/heads/main".to_string(), Ok(())));
    }

    #[test]
    fn parse_report_status_ng() {
        let b = body(&[
            &dl("unpack ok\n"),
            &dl("ng refs/heads/main non-fast-forward\n"),
            flush_pkt(),
        ]);
        let r = parse_report_status(&b, false).unwrap();
        assert!(r.unpack_ok);
        assert_eq!(
            r.ref_statuses[0],
            ("refs/heads/main".to_string(), Err("non-fast-forward".to_string()))
        );
    }

    #[test]
    fn parse_report_status_v2_ignores_option_lines() {
        let b = body(&[
            &dl("unpack ok\n"),
            &dl("ok refs/heads/main\n"),
            &dl("option refname refs/heads/main\n"),
            &dl("option new-oid 1111111111111111111111111111111111111111\n"),
            flush_pkt(),
        ]);
        let r = parse_report_status(&b, false).unwrap();
        assert!(r.unpack_ok);
        assert_eq!(r.ref_statuses.len(), 1);
        assert_eq!(r.ref_statuses[0], ("refs/heads/main".to_string(), Ok(())));
    }

    #[test]
    fn parse_report_status_sidebanded() {
        // Inner report pkt-line stream, wrapped in band-1 frames.
        let inner = body(&[&dl("unpack ok\n"), &dl("ok refs/heads/main\n"), flush_pkt()]);
        let wrapped = body(&[
            &band(2, b"progress\n"),
            &band(1, &inner),
            flush_pkt(),
        ]);
        let r = parse_report_status(&wrapped, true).unwrap();
        assert!(r.unpack_ok);
        assert_eq!(r.ref_statuses[0], ("refs/heads/main".to_string(), Ok(())));
    }

    #[test]
    fn parse_report_status_sideband_split_frames() {
        // The inner stream can be split across multiple band-1 frames arbitrarily.
        let inner = body(&[&dl("unpack ok\n"), &dl("ng refs/heads/main locked\n"), flush_pkt()]);
        let (a, c) = inner.split_at(7);
        let wrapped = body(&[&band(1, a), &band(1, c), flush_pkt()]);
        let r = parse_report_status(&wrapped, true).unwrap();
        assert!(r.unpack_ok);
        assert_eq!(
            r.ref_statuses[0],
            ("refs/heads/main".to_string(), Err("locked".to_string()))
        );
    }

    #[test]
    fn parse_report_status_sideband_band3_errors() {
        let wrapped = body(&[&band(3, b"fatal: hook declined"), flush_pkt()]);
        let err = parse_report_status(&wrapped, true).unwrap_err();
        assert_eq!(err, GitWireError::RemoteError("fatal: hook declined".into()));
    }

    #[test]
    fn parse_report_status_unpack_error() {
        let b = body(&[&dl("unpack index-pack failed\n"), flush_pkt()]);
        let r = parse_report_status(&b, false).unwrap();
        assert!(!r.unpack_ok);
    }

    // --- URLs ------------------------------------------------------------------

    #[test]
    fn endpoint_url_builders() {
        assert_eq!(
            info_refs_url("https://github.com/o/r.git"),
            "https://github.com/o/r.git/info/refs?service=git-upload-pack"
        );
        assert_eq!(
            upload_pack_url("https://github.com/o/r.git/"),
            "https://github.com/o/r.git/git-upload-pack"
        );
        assert_eq!(
            receive_pack_url("https://x/y"),
            "https://x/y/git-receive-pack"
        );
        assert_eq!(
            receive_pack_info_refs_url("https://x/y"),
            "https://x/y/info/refs?service=git-receive-pack"
        );
    }

    #[test]
    fn normalize_https_remote_rules() {
        assert_eq!(
            normalize_https_remote("https://github.com/o/r.git/").unwrap(),
            "https://github.com/o/r.git"
        );
        assert_eq!(
            normalize_https_remote("https://github.com/o/r").unwrap(),
            "https://github.com/o/r"
        );
        // does NOT add .git
        assert!(!normalize_https_remote("https://github.com/o/r")
            .unwrap()
            .ends_with(".git"));
        assert!(normalize_https_remote("http://github.com/o/r").is_err());
        assert!(normalize_https_remote("https://tok@github.com/o/r").is_err());
        assert!(normalize_https_remote("git@github.com:o/r").is_err());
    }

    #[test]
    fn parse_git_url_table() {
        use GitUrl::*;
        // (input, expected)
        let https_cases: &[(&str, &str)] = &[
            ("https://github.com/o/r.git", "https://github.com/o/r.git"),
            ("https://github.com/o/r", "https://github.com/o/r"),
            ("https://github.com/o/r/", "https://github.com/o/r"),
            ("https://gitlab.example.com:8443/g/p.git", "https://gitlab.example.com:8443/g/p.git"),
        ];
        for (inp, base) in https_cases {
            assert_eq!(
                parse_git_url(inp),
                Some(Https { base: base.to_string() }),
                "https case {inp}"
            );
        }

        // ssh:// with and without user/port
        assert_eq!(
            parse_git_url("ssh://git@github.com/o/r.git"),
            Some(Ssh {
                user: Some("git".into()),
                host: "github.com".into(),
                port: None,
                path: "/o/r.git".into()
            })
        );
        assert_eq!(
            parse_git_url("ssh://git@github.com:2222/o/r.git"),
            Some(Ssh {
                user: Some("git".into()),
                host: "github.com".into(),
                port: Some(2222),
                path: "/o/r.git".into()
            })
        );
        assert_eq!(
            parse_git_url("ssh://example.com/srv/repo"),
            Some(Ssh {
                user: None,
                host: "example.com".into(),
                port: None,
                path: "/srv/repo".into()
            })
        );

        // scp-like
        assert_eq!(
            parse_git_url("git@github.com:owner/repo.git"),
            Some(Ssh {
                user: Some("git".into()),
                host: "github.com".into(),
                port: None,
                path: "owner/repo.git".into()
            })
        );
        assert_eq!(
            parse_git_url("github.com:owner/repo"),
            Some(Ssh {
                user: None,
                host: "github.com".into(),
                port: None,
                path: "owner/repo".into()
            })
        );
        assert_eq!(
            parse_git_url("git@localhost:repo.git"),
            Some(Ssh {
                user: Some("git".into()),
                host: "localhost".into(),
                port: None,
                path: "repo.git".into()
            })
        );

        // --- must NOT parse as git URLs ---
        let none_cases: &[&str] = &[
            "",
            "   ",
            "http://github.com/o/r",                 // http rejected
            "git://github.com/o/r",                  // git:// rejected
            "file:///srv/repo.git",                  // file:// rejected
            "https://tok:x@github.com/o/r",          // userinfo (credentials) rejected
            "https://user@github.com/o/r",           // userinfo rejected
            "https:///o/r",                          // no host
            "/home/chris/repo.git",                  // absolute local path
            "./relative/repo.git",                   // relative local path
            "../up/repo",                            // relative local path
            "repo.git",                              // bare local name
            "C:\\Users\\me\\repo",                   // windows path (host "C", no dot)
            "word:word",                             // bare word:word (not a real host)
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef", // 64-hex node id
            "nodeaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", // iroh-ish ticket
            "just some words with spaces",
        ];
        for c in none_cases {
            assert_eq!(parse_git_url(c), None, "expected None for {c:?}");
        }
    }

    #[test]
    fn looks_like_git_url_matches_parse() {
        assert!(looks_like_git_url("https://github.com/o/r"));
        assert!(looks_like_git_url("git@github.com:o/r.git"));
        assert!(!looks_like_git_url("/home/chris/repo"));
        assert!(!looks_like_git_url(
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        ));
    }

    // --- deterministic fuzz: never panic, always typed errors ------------------

    #[test]
    fn fuzz_parsers_never_panic() {
        use rand::{Rng, RngCore, SeedableRng};

        // Valid fixtures we will also feed and mutate.
        let fixtures: Vec<Vec<u8>> = vec![
            github_upload_pack_advert(),
            build_ls_refs(&["refs/heads/"]),
            fetch_response_full(),
            body(&[&dl("packfile\n"), &band(1, b"PACK"), flush_pkt()]),
            {
                let inner =
                    body(&[&dl("unpack ok\n"), &dl("ok refs/heads/main\n"), flush_pkt()]);
                body(&[&band(1, &inner), flush_pkt()])
            },
        ];

        // Every parser must either Ok or return a GitWireError (never panic). Since
        // the signatures already return GitResult, "no panic" is the property under
        // test; catch_unwind makes a regression fail loudly rather than abort.
        fn hammer(bytes: &[u8]) {
            let run = |f: &dyn Fn()| {
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
                assert!(r.is_ok(), "parser panicked on input len {}", bytes.len());
            };
            run(&|| {
                let _ = PktReader::new(bytes).collect::<GitResult<Vec<_>>>();
            });
            run(&|| {
                let _ = parse_capability_advertisement(bytes);
            });
            run(&|| {
                let _ = parse_ls_refs_response(bytes);
            });
            run(&|| {
                let _ = FetchResponseParser::parse(bytes);
            });
            run(&|| {
                let _ = parse_receive_pack_advertisement(bytes);
            });
            run(&|| {
                let _ = parse_report_status(bytes, false);
            });
            run(&|| {
                let _ = parse_report_status(bytes, true);
            });
            run(&|| {
                let _ = parse_git_url(&String::from_utf8_lossy(bytes));
            });
            run(&|| {
                let _ = normalize_https_remote(&String::from_utf8_lossy(bytes));
            });
        }

        let mut rng = rand::rngs::StdRng::seed_from_u64(0xA5217E);
        for _ in 0..600 {
            // (a) random raw bytes of random length
            let len = rng.gen_range(0..512);
            let mut buf = vec![0u8; len];
            rng.fill_bytes(&mut buf);
            hammer(&buf);

            // (b) a random valid fixture with a handful of random byte mutations
            let mut f = fixtures[rng.gen_range(0..fixtures.len())].clone();
            if !f.is_empty() {
                for _ in 0..rng.gen_range(0..8) {
                    let i = rng.gen_range(0..f.len());
                    f[i] = rng.gen();
                }
                // occasionally truncate
                if rng.gen_bool(0.3) {
                    let cut = rng.gen_range(0..=f.len());
                    f.truncate(cut);
                }
            }
            hammer(&f);
        }
    }

    #[test]
    fn error_display_and_asp_conversion() {
        let e = GitWireError::RemoteError("boom".into());
        assert!(e.to_string().contains("boom"));
        let a: AspError = e.into();
        assert!(matches!(a, AspError::Protocol(_)));
    }
}
