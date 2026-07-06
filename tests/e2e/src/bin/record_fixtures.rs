//! Record real smart-HTTP protocol-v2 wire bytes from the hermetic server into
//! checked-in fixtures for the core team's `gitwire` parser tests (spec §10
//! "`gitwire` unit tests: protocol-v2 request/response fixtures").
//!
//! Run with:
//!
//! ```text
//! cargo run -p asp-e2e --bin record_fixtures
//! ```
//!
//! It builds the `linear_basic` fixture, serves it via [`GitHttpServer`], and
//! captures three response bodies into `tests/e2e/fixtures/`:
//!
//! - `info_refs_v2.bin`  — `GET /info/refs?service=git-upload-pack` advertisement
//! - `ls_refs_v2.bin`    — `POST /git-upload-pack` `command=ls-refs` response
//! - `fetch_v2.bin`      — `POST /git-upload-pack` `command=fetch` (want tip, done):
//!   a small packfile response with sideband framing
//!
//! The bytes depend on the local git version, so this is a recorder the core team
//! runs, not a build step. The `asp-core` team should copy these into
//! `crates/asp-core/src/git_fixtures/` (this crate must not write there).

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;

use asp_e2e::gitfix::{linear_basic, GitHttpServer};

/// pkt-line encode: 4-hex length prefix (incl. the prefix) + payload.
fn pkt(s: &str) -> Vec<u8> {
    let mut v = format!("{:04x}", s.len() + 4).into_bytes();
    v.extend_from_slice(s.as_bytes());
    v
}
const FLUSH: &[u8] = b"0000";
const DELIM: &[u8] = b"0001";

/// Minimal HTTP/1.1 client: send a request, return the response body (after the
/// header/body split). We ask for `Connection: close` and read to EOF.
fn http(base_url: &str, method: &str, path: &str, headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
    let authority = base_url.strip_prefix("http://").expect("http url");
    let mut stream = TcpStream::connect(authority).expect("connect");

    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n");
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
    stream.write_all(req.as_bytes()).unwrap();
    stream.write_all(body).unwrap();
    stream.flush().unwrap();

    let mut resp = Vec::new();
    stream.read_to_end(&mut resp).unwrap();

    // Split off the HTTP response headers.
    let split = resp.windows(4).position(|w| w == b"\r\n\r\n").expect("http header split");
    resp[split + 4..].to_vec()
}

fn main() {
    let repo = linear_basic();
    let tip = repo.head();
    let server = GitHttpServer::spawn(repo.repo_root());
    let base = server.base_url.clone();
    let name = "linear_basic.git";

    let out_dir: PathBuf = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    std::fs::create_dir_all(&out_dir).unwrap();

    // (1) info/refs advertisement (protocol v2).
    let info_refs = http(
        &base,
        "GET",
        &format!("/{name}/info/refs?service=git-upload-pack"),
        &[("Git-Protocol", "version=2")],
        &[],
    );
    std::fs::write(out_dir.join("info_refs_v2.bin"), &info_refs).unwrap();

    // (2) ls-refs response.
    let mut ls = Vec::new();
    ls.extend(pkt("command=ls-refs\n"));
    ls.extend(pkt("object-format=sha1\n"));
    ls.extend_from_slice(DELIM);
    ls.extend(pkt("peel\n"));
    ls.extend(pkt("symrefs\n"));
    ls.extend(pkt("unborn\n"));
    ls.extend(pkt("ref-prefix HEAD\n"));
    ls.extend(pkt("ref-prefix refs/heads/\n"));
    ls.extend(pkt("ref-prefix refs/tags/\n"));
    ls.extend_from_slice(FLUSH);
    let ls_refs = http(
        &base,
        "POST",
        &format!("/{name}/git-upload-pack"),
        &[
            ("Git-Protocol", "version=2"),
            ("Content-Type", "application/x-git-upload-pack-request"),
        ],
        &ls,
    );
    std::fs::write(out_dir.join("ls_refs_v2.bin"), &ls_refs).unwrap();

    // (3) fetch response (small packfile with sideband framing).
    let mut fetch = Vec::new();
    fetch.extend(pkt("command=fetch\n"));
    fetch.extend(pkt("object-format=sha1\n"));
    fetch.extend_from_slice(DELIM);
    fetch.extend(pkt("thin-pack\n"));
    fetch.extend(pkt("ofs-delta\n"));
    fetch.extend(pkt(&format!("want {tip}\n")));
    fetch.extend(pkt("done\n"));
    fetch.extend_from_slice(FLUSH);
    let fetch_resp = http(
        &base,
        "POST",
        &format!("/{name}/git-upload-pack"),
        &[
            ("Git-Protocol", "version=2"),
            ("Content-Type", "application/x-git-upload-pack-request"),
        ],
        &fetch,
    );
    std::fs::write(out_dir.join("fetch_v2.bin"), &fetch_resp).unwrap();

    println!("wrote fixtures to {}:", out_dir.display());
    for (f, n) in [
        ("info_refs_v2.bin", info_refs.len()),
        ("ls_refs_v2.bin", ls_refs.len()),
        ("fetch_v2.bin", fetch_resp.len()),
    ] {
        println!("  {f}  ({n} bytes)");
    }
    println!("tip commit: {tip}");
}
