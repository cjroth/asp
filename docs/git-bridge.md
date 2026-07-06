# Git bridge — clone from and two-way-sync with a git remote

ASP can treat an ordinary git remote (GitHub, Gitea, self-hosted; `https://` or
`ssh://`) as **just another peer**. Clone a repo into a live vault, keep pulling
upstream commits in, and roll your vault edits back up into real git commits that
push. This is the practical guide; the design rationale lives in
[`../specs/git-bridge.md`](../specs/git-bridge.md).

The one-paragraph model: a git remote's commits enter the ASP log as ordinary rows
under a repo-derived site id, chained like any peer's edits; a local edit that raced
an upstream commit merges through the normal 3-way fold. Outbound, your rows roll up
into synthesized commits built on the imported history, so a push is a plain
fast-forward. Because every input is either derived from the log or carried in synced
records, **any native node can bridge** — no coordinator.

---

## Clone

### CLI

`asp clone` auto-detects the source: an iroh ticket / node id clones from an ASP
peer; a git URL clones from git. A git URL is `https://…`, `ssh://…`,
`git@host:path`, or any path ending in `.git`.

```sh
asp clone https://github.com/owner/repo ./repo
cd ./repo                       # the full commit history is now your timeline
asp git status                  # remote / at-sha / ahead·behind / policy
```

Flags (git sources only):

| Flag | Effect |
|---|---|
| `--depth <n>` | Import only the last `n` first-parent commits of the default branch (plus side ancestry merged within that window), fronted by one snapshot batch. For big/monorepos — see [Large repos](#large-repos). |
| `--new-identity` | Clone into a fresh random `vault_id` instead of the repo-derived one, when you deliberately want a vault that will *not* auto-converge with other clones of the same repo. |
| `--token <t>` | HTTPS personal access token (also read from `ASP_GIT_TOKEN`). See [Authentication](#authentication). |
| `--watch` | After cloning, stay running: watch the working tree and run the periodic pull + policy ticks. |

A clone is all-or-nothing: rows fold only after the whole pack decodes, so a torn
download leaves no half-vault. A bad URL / missing repo / rejected auth fails before
any vault directory is created.

### Desktop

Open the connect modal and paste a git URL into the same box you'd paste an invite
code into. When the input looks like a git URL the access-key field becomes a
**Token** field (shown only for `https://`; `ssh://` shows "uses your SSH agent").
Pick a destination folder and clone; progress runs through `fetching → replaying →
saving`. The vault card then carries a git status chip.

### Web

The browser clones through the relay's git proxy (see
[Browser setup](#browser-setup-the-relay-git-proxy)); web is **clone/pull only, no
push**. Paste the git URL and (for HTTPS) a token into the connect modal. The proxy
base must be configured or the clone errors with a pointer to set it.

---

## Authentication

Credentials never enter the synced log, the config, or `desktop_folders.json`.

**HTTPS (token / PAT).** Resolution order per remote:

1. `--token <t>` on `asp clone` / `asp git remote add`, which is stored in the OS
   keyring (macOS Keychain / Secret Service / Windows Credential Manager) under the
   entry `asp-git/<remote_id>`; `git_remotes.auth_ref` holds only the entry name.
2. `ASP_GIT_TOKEN` in the environment.

Anonymous HTTPS works for public repos (no token needed). Prefer **fine-grained,
single-repo** PATs — the smallest credential that can clone/push the one repo.

**SSH.** Use an `ssh://` or `git@host:path` URL. ASP spawns your own `ssh` binary
(`ssh -o BatchMode=yes …`), so your existing keys, agent, `~/.ssh/config` host
aliases, and hardware keys all just work — ASP never parses a private key, and
host-key verification is ssh's own `known_hosts`. If `ssh` isn't on `PATH` you get a
clear error suggesting the HTTPS URL instead.

---

## Ongoing pull

A cloned vault tracks the remote's default branch. Pull on demand:

```sh
asp git pull        # "already up to date" | "pulled N new commit(s)" | FROZEN
```

A fetch that reveals a merged PR ingests the whole thing at once — the merge commit
plus its side chain become an ASP branch (create record, rows, merge marker,
delete-after-merge), then processing continues on `main`. Two bridge nodes ingesting
the same commit converge (content is identical; the duplicate marker collapses in the
UI).

**The watch loop.** `asp watch` (or `asp clone --watch`) runs the pull tick
(~5 min + jitter) and, if the policy is `interval`, the policy tick, alongside the fs
watcher and peer reconnects. A web tab pulls on an interval while open; and a web
vault that is *also* connected to a native peer gets git updates over ordinary ASP
sync with zero git traffic from the browser.

**Force-push / history rewrite.** If the remote ref is no longer a descendant of the
last-ingested commit, the bridge **freezes**: it stops ingesting and surfaces a
persistent error. Recover explicitly (§[force-push recovery](#force-push-recovery)).

---

## Push and rollup policies

Pushing means: author a **plan** (a commit boundary + message), then synthesize a
commit deterministically and push it as a fast-forward. Who authors plans and when is
the *policy*; synthesis itself is fixed and deterministic, so two bridge nodes
compute identical commit SHAs and a racing push is a harmless no-op.

Set or show the policy:

```sh
asp git policy              # show current (default: manual)
asp git policy interval     # switch to time-based auto-commit
asp git policy manual
```

### manual (default)

Nothing pushes without an explicit action.

```sh
asp git push                       # opens $EDITOR with a diff-summary message
asp git push -m "fix the parser"   # non-interactive message
asp git push -m "…" --author "Ada <ada@example.com>"
```

With no `-m` and no changes, it says "nothing to push". On the desktop, the "Commit &
push to git" button does the same with an editable, pre-filled message.

### interval

A watching bridge auto-authors a plan when the vault has pending rows and has gone
quiet (default 10 min) or a window elapsed (default 4 h), with an auto-generated
`asp: N file(s) changed (…)` message. Fetch jitter and an equal-frontier skip guard
against two bridges double-committing. Enable it with `asp git policy interval` and
keep `asp watch` running.

### LLM-authored messages (the `diff` / `plan` primitives)

The engine never calls a model. Instead it exposes two primitives so an external
agent (Claude Code via MCP, a cron, your own script) can decide commit boundaries and
write real messages, while synthesis stays deterministic because the message is
recorded in the synced plan:

```sh
asp git diff                 # the pending unified diff + "# N file(s) changed (…)"
asp git diff --json          # { files_changed, paths, unified } for a program
asp git plan -m "message"    # record a commit boundary + message WITHOUT pushing
asp git push                 # (or the interval tick) synthesizes + pushes the plan(s)
```

Keep the policy on `manual` and drive `diff` → `plan` on your own cadence; a later
`asp git push` (or interval tick) turns the recorded plans into pushed commits.

---

## Remotes

A clone configures its remote automatically. To manage remotes by hand:

```sh
asp git remote show                                   # list configured remotes
asp git remote add https://github.com/owner/repo \
    --policy interval --push-ref refs/heads/asp --token ghp_xxx
asp git remote remove
```

`--push-ref` targets a non-default branch if you'd rather ASP not push straight to
the remote default (ASP `main` maps to the remote default branch by default).

---

## Browser setup: the relay git proxy

A browser can't fetch a git host's smart-HTTP endpoints directly — hosts send no CORS
headers, even for read-only clone. So every browser git request routes through a CORS
proxy co-hosted with the relay:

```sh
asp relay --git-proxy                        # proxy on 0.0.0.0:8081 (relay on :8080)
asp relay --git-proxy --git-proxy-addr 0.0.0.0:9000
asp relay --git-proxy --git-proxy-allow github.com --git-proxy-allow gitea.example.com
```

Point the web app at it via `VITE_GIT_PROXY_BASE` (build-time) or a
`globalThis.__ASP_GIT_PROXY_BASE__` override (runtime/dev).

**Security — read this.** The proxy **TLS-terminates** git traffic: unlike relayed
ASP traffic (which stays end-to-end-encrypted, the relay seeing only ciphertext), the
git proxy sees git payloads — and any HTTPS token in them — in plaintext. **Run your
own proxy; don't send tokens through someone else's.** The proxy is SSRF-hardened: it
forwards only the two smart-HTTP shapes (`GET …/info/refs?service=git-upload-pack`
and `POST …/git-upload-pack`), allows HTTPS/443 only, resolves and rejects
private/loopback/link-local addresses, passes through only `Authorization` /
`Content-Type` / `Accept` (never logging `Authorization`), enforces size/rate caps,
and honors the optional `--git-proxy-allow` host allowlist for locked-down
deployments.

Browser tokens are stored in OPFS `registry.json` — the same trust level as the
stored ASP auth key. A stolen browser profile leaks them, so use fine-grained
single-repo PATs.

---

## Large repos

Full-DAG import is O(history): every side-branch commit contributes its own diff, and
each merged PR adds two branch records. For big or monorepos, cap it:

```sh
asp clone https://github.com/owner/big-monorepo ./m --depth 500
```

`--depth <n>` imports the last `n` first-parent commits of the default branch (plus
side ancestry merged inside that window), preceded by one synthetic snapshot batch of
the tree at the cut point. Determinism holds for equal `depth`. A pre-flight size
estimate warns before downloading an unexpectedly huge pack.

---

## Force-push recovery

If upstream history was rewritten, the bridge freezes and `asp git status` /
`asp git pull` report `FROZEN`. Recover explicitly:

```sh
asp git rebaseline --yes
```

This re-imports the rewritten tip as one snapshot batch (diffing your current vault
state against the new tip), records the rebaseline, and clears the freeze. It is
deliberately manual — there is no automatic healing in v1.

---

## Determinism: paste the same URL on two machines

Every imported row's identity derives purely from the git history —
`vault_id = sha256("asp-git-vault/v1" ‖ root_sha)`, the repo's `site_id` and each
`file_id` likewise. So if Alice and Bob independently `asp clone` the same URL on
different machines, they get **byte-identical** rows and vault ids and can immediately
ASP-sync with *each other* over normal anti-entropy — every genesis row dedups by its
Merkle id. This is what makes the git bridge feel native rather than bolted on. Use
`--new-identity` only when you deliberately want a separate, non-converging vault.

---

## Troubleshooting & limitations

- **Old peer refuses to connect after you added a git remote.** Git-bridge log
  records are `PROTO 4`; a peer built before the 3 → 4 bump refuses at the handshake
  with an "upgrade" message. Upgrade every peer in the same window (see
  [`../RELEASING.md`](../RELEASING.md) → Protocol version).
- **`git proxy is not configured` in the browser.** Set `VITE_GIT_PROXY_BASE` (or the
  `globalThis.__ASP_GIT_PROXY_BASE__` override) to your `asp relay --git-proxy` URL.
- **SSH clone fails with "ssh not found".** Install/expose `ssh`, or use the HTTPS URL
  + a token.
- **A local `chmod +x` didn't stick after push.** ASP doesn't model the executable
  bit as vault state; the ledger replays the *imported* mode, so a local mode change
  is invisible to git. (Documented limitation.)
- **`asp git push` says "nothing to push".** No rows are pending since the last
  plan/ingest frontier — check `asp git diff`.

**v1 non-goals** (won't happen yet, by design): git submodule recursion, git-LFS
smudging (LFS pointers import as their pointer text), importing *unmerged* remote refs
(only the default branch's ancestry imports), pushing ASP branches other than `main`,
automatic healing after an upstream force-push (use `rebaseline`), browser-side push,
and the `git://` protocol.
