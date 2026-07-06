// A UI-side hint that decides whether the connect-modal input is a git remote
// (route to `cloneGit`) rather than an ASP peer ticket / node id (route to
// `cloneRemote`). The Rust `gitwire::parse_git_url` is the real authority — this
// only has to be a loose, syntactic classifier good enough to swap the modal's
// fields and pick the clone path. It must never misfire on the two ASP inputs it
// shares the box with: an iroh ticket (a long base32 blob) or a 64-hex node id —
// neither carries a scheme, a scp-style `host:path` colon, or a `.git` suffix.

// Any explicit `scheme://…` prefix. Only https/ssh are git; everything else
// (http, git, file, ftp, …) is rejected outright.
const SCHEME_RE = /^[a-z][a-z0-9+.-]*:\/\//i;
// scp-like `[user@]host:path` — the colon must precede any slash, and the path
// must be non-empty. Captures the optional user and the host.
const SCP_RE = /^(?:([^/\\@:]+)@)?([^/\\:]+):(.+)$/;

// A host is "hostish" if it carries a dot (a real DNS name / IP) or is the
// literal `localhost` — this rules out `word:word` false positives like `12:34`.
function isHostish(host: string): boolean {
  return host.includes('.') || host === 'localhost';
}

export type GitUrlScheme = 'https' | 'ssh' | null;

// Classify the input: 'https' for `https://…`, 'ssh' for `ssh://…` or a scp-like
// `git@host:path`, or null if it isn't a git URL at all. `https://` with an `@`
// in the authority is rejected to mirror the Rust parser.
export function gitUrlScheme(input: string): GitUrlScheme {
  const s = input.trim();
  if (!s) return null;

  const https = s.match(/^https:\/\/(\S+)$/i);
  if (https) {
    const authority = https[1].split('/')[0] ?? '';
    // Reject an empty authority or an embedded `user@` (mirrors the Rust parser).
    if (!authority || authority.includes('@')) return null;
    return 'https';
  }

  if (/^ssh:\/\/\S+$/i.test(s)) return 'ssh';

  // Any other explicit scheme is not a git URL we handle.
  if (SCHEME_RE.test(s)) return null;

  // scp-like `[user@]host:path` (ssh transport, e.g. `git@github.com:o/r.git`).
  const scp = s.match(SCP_RE);
  if (scp) {
    const user = scp[1];
    const host = scp[2];
    if (user || isHostish(host)) return 'ssh';
  }

  // A bare filesystem-ish path ending in `.git` (looser than Rust; a UI hint).
  if (/\.git\/?$/.test(s) && !/\s/.test(s)) return 'ssh';

  return null;
}

// True when the input looks like any git remote URL. Used to branch the connect
// modal to the git clone path instead of the ASP peer path.
export function looksLikeGitUrl(input: string): boolean {
  return gitUrlScheme(input) !== null;
}
