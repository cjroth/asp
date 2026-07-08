// Web backend: the real asp-core engine compiled to wasm (asp-wasm), with each
// vault's state persisted to the browser's Origin Private File System (OPFS) via
// `dump_state()`/`load_state()`. This is the "thin client" model from the design
// — no folders, no native FS; notes live in browser storage and converge over
// iroh sync. Implements the same `Api` surface as the desktop (Tauri) backend.
import init, { WasmEngine } from 'asp-wasm';
// Let Vite serve the .wasm as a hashed asset with the correct `application/wasm`
// MIME (in both dev and build). Relying on wasm-bindgen's default
// `new URL('…', import.meta.url)` breaks under Vite dev: for a linked package it
// resolves to a `file://` URL the browser refuses to fetch.
import wasmUrl from 'asp-wasm/asp_wasm_bg.wasm?url';
import type { Api, ClonePhase, CloneProgress, FileAt, FileEntry, GitStatus, HistEvent, PendingDiff, VaultInfo, VaultStatus } from './api';
import { WELCOME_MD } from '../vault/welcome';
import { makeCoalescer } from './coalesce';

const enc = new TextEncoder();
const dec = new TextDecoder();
const randHex = (n: number) => Array.from(crypto.getRandomValues(new Uint8Array(n)), (b) => b.toString(16).padStart(2, '0')).join('');

interface RegEntry {
  id: string;
  vault_id: string;
  // The upstream this vault was cloned from. Browsers can't open a listening
  // socket, so a web node stays in sync by holding a live connection to this
  // ticket open (`startLiveSync`), reconnecting if it drops.
  ticket?: string;
  authKey?: string | null;
  // Set when this vault was cloned from a git remote (git-bridge §7.3). Node-private
  // (lives only in the OPFS registry), so it's never synced to peers.
  git?: { url: string; proxyBase: string; rootSha: string; remoteRef: string; defaultBranch: string };
  // The HTTPS token (PAT) for the git remote, if any. Stored in the OPFS registry at
  // the SAME trust level as `authKey` — a stolen browser profile leaks it, so the UI
  // copy should recommend a fine-grained, single-repo PAT (git-bridge §7.3).
  gitToken?: string;
}

// The relay `--git-proxy` base URL for browser git clone/pull (git-bridge §7.3). A
// browser can't reach a git host's smart-HTTP endpoints directly (they send no CORS
// headers), so every git request routes through the relay-co-hosted proxy. Point
// this at your relay's `asp relay --git-proxy` listener via VITE_GIT_PROXY_BASE
// (build-time) or a `globalThis.__ASP_GIT_PROXY_BASE__` override (runtime/dev).
function gitProxyBase(): string {
  const env = (import.meta as unknown as { env?: Record<string, string | undefined> }).env;
  const base = (globalThis as { __ASP_GIT_PROXY_BASE__?: string }).__ASP_GIT_PROXY_BASE__ ?? env?.VITE_GIT_PROXY_BASE;
  if (!base) throw new Error("git proxy is not configured — set VITE_GIT_PROXY_BASE to your relay's `asp relay --git-proxy` URL");
  return base;
}

// The transport handed to the wasm git methods: wasm owns the git protocol + builds
// the proxy URL and request bytes; JS just performs the actual CORS fetch. `headers`
// is a plain object, `body` a Uint8Array|null; resolves to { status, body:Uint8Array }.
const gitFetch = async (method: string, url: string, headers: Record<string, string>, body: Uint8Array | null) => {
  const res = await fetch(url, { method, headers, body: body ? (body as BufferSource) : undefined, mode: 'cors' });
  return { status: res.status, body: new Uint8Array(await res.arrayBuffer()) };
};

// ---- byte store: OPFS when available, in-memory fallback otherwise ----
// OPFS needs a secure context and browser support; where it's missing (e.g. some
// private-browsing modes, older engines, headless drivers) we degrade to a
// non-persistent in-memory store so the app still runs.
interface ByteStore {
  read(name: string): Promise<Uint8Array | null>;
  write(name: string, bytes: Uint8Array): Promise<void>;
  remove(name: string): Promise<void>;
}

function memoryStore(): ByteStore {
  const m = new Map<string, Uint8Array>();
  return {
    read: async (n) => m.get(n) ?? null,
    write: async (n, b) => void m.set(n, b),
    remove: async (n) => void m.delete(n),
  };
}

function opfsStore(dir: FileSystemDirectoryHandle): ByteStore {
  return {
    read: async (n) => {
      try {
        const fh = await dir.getFileHandle(n);
        return new Uint8Array(await (await fh.getFile()).arrayBuffer());
      } catch {
        return null;
      }
    },
    write: async (n, b) => {
      const fh = await dir.getFileHandle(n, { create: true });
      const w = await fh.createWritable();
      await w.write(b as BufferSource);
      await w.close();
    },
    remove: async (n) => {
      try {
        await dir.removeEntry(n);
      } catch {
        /* already gone */
      }
    },
  };
}

let storePromise: Promise<ByteStore> | null = null;
function store(): Promise<ByteStore> {
  return (storePromise ??= (async () => {
    try {
      if (navigator.storage?.getDirectory) return opfsStore(await navigator.storage.getDirectory());
    } catch {
      /* fall through to the in-memory store */
    }
    console.warn('[asp] OPFS is unavailable in this browser — notes will not persist across reloads.');
    return memoryStore();
  })());
}

const readBytes = (name: string) => store().then((s) => s.read(name));
const writeBytes = (name: string, bytes: Uint8Array) => store().then((s) => s.write(name, bytes));
const removeFile = (name: string) => store().then((s) => s.remove(name));
async function readJson<T>(name: string, fallback: T): Promise<T> {
  const b = await readBytes(name);
  if (!b) return fallback;
  try {
    return JSON.parse(dec.decode(b)) as T;
  } catch {
    return fallback;
  }
}
const writeJson = (name: string, obj: unknown) => writeBytes(name, enc.encode(JSON.stringify(obj)));

// Persistence layout. Legacy vaults kept the WHOLE engine (rows + every blob's
// bytes) in one `.state` blob — serializing that on a large git clone OOMs the
// wasm32 heap. The split layout writes a tiny rows-only snapshot (`.rows`) plus
// one immutable content-addressed entry per blob (`.blob.<hash>`), and a
// `.blobs` index listing the hashes so a vault delete can find them all.
const stateName = (id: string) => `vault-${id}.state`;
const rowsName = (id: string) => `vault-${id}.rows`;
const blobName = (id: string, hash: string) => `vault-${id}.blob.${hash}`;
const blobIndexName = (id: string) => `vault-${id}.blobs`;

export function createWebApi(): Api {
  let wasmReady: Promise<void> | null = null;
  const ensureWasm = () => (wasmReady ??= init({ module_or_path: wasmUrl }).then(() => undefined));

  let seed: Uint8Array | null = null;
  async function deviceSeed(): Promise<Uint8Array> {
    if (seed) return seed;
    let s = await readBytes('device-seed');
    if (!s || s.length < 32) {
      s = crypto.getRandomValues(new Uint8Array(32));
      await writeBytes('device-seed', s);
    }
    seed = s;
    return s;
  }

  const registry = () => readJson<RegEntry[]>('registry.json', []);
  const engines = new Map<string, WasmEngine>();
  // Content-addressed blobs already on disk for a vault this session — a blob is
  // immutable under its hash, so persist only writes the ones it hasn't yet
  // (no more re-serializing the whole vault on every keystroke, webApi.ts:180).
  const writtenBlobs = new Map<string, Set<string>>();
  // Vaults whose legacy combined `.state` we've already reclaimed post-migration.
  const clearedOldState = new Set<string>();
  // Live connections, one per vault id. A browser can't accept inbound
  // connections, but once it dials the upstream it holds the link open and rows
  // stream both ways in realtime — no polling.
  const live = new Map<string, { stop: boolean; gitTimer?: ReturnType<typeof setInterval> }>();

  // Pull a git-configured vault once (git-bridge §4). Reads the vault's git config
  // from the registry, drives the wasm incremental pull, and schedules a persist if
  // anything landed. A no-op for a vault with no git remote. Returns the new-commit count.
  async function gitPullOnce(id: string): Promise<number> {
    const entry = (await registry()).find((r) => r.id === id);
    if (!entry?.git) return 0;
    const eng = await engineFor(id);
    const json = await eng.git_pull(entry.git.url, entry.gitToken ?? undefined, entry.git.proxyBase, gitFetch, undefined);
    const { new_commits } = JSON.parse(json) as { new_commits: number };
    if (new_commits > 0) persistQueue.schedule(id, eng);
    return new_commits;
  }

  async function engineFor(id: string): Promise<WasmEngine> {
    const cached = engines.get(id);
    if (cached) return cached;
    await ensureWasm();
    const reg = await registry();
    const entry = reg.find((r) => r.id === id);
    const eng = new WasmEngine(await deviceSeed(), entry?.vault_id ?? '');
    const rows = await readBytes(rowsName(id));
    if (rows) {
      // Split layout: restore each referenced blob into the engine BEFORE loading
      // the rows (branch reconciliation + the fold read blob bytes), then import.
      const hashes = JSON.parse(eng.blob_hashes_of_rows(rows)) as string[];
      const present = new Set<string>();
      const missing: string[] = [];
      for (const h of hashes) {
        const b = await readBytes(blobName(id, h));
        if (b) {
          eng.put_blob(b);
          present.add(h);
        } else {
          missing.push(h);
        }
      }
      eng.load_rows_state(rows);
      // A referenced blob absent from OPFS is DATA LOSS, not a cache miss: the row
      // is already integrated, so version-vector anti-entropy will never re-deliver
      // its WireRow — peers consider it delivered. The fold materializes the
      // affected file(s) as EMPTY (indistinguishable from intentionally-empty), and
      // any edit on top makes that permanent. This should never happen given the
      // persist write-order invariant (blobs before rows); if it does, something
      // truncated OPFS out from under us. Surface it loudly with the affected paths
      // (cheap: files' result_hash is in the fold detail) — see webApi persist().
      if (missing.length > 0) {
        let affected: string[] = [];
        try {
          const detail = JSON.parse(eng.files_detail_json()) as { path: string; result_hash: string | null }[];
          const miss = new Set(missing);
          affected = detail.filter((f) => f.result_hash && miss.has(f.result_hash)).map((f) => f.path);
        } catch {
          /* best-effort path derivation — the count + hashes below still land */
        }
        console.error(
          `[asp] vault ${entry?.vault_id ?? id}: ${missing.length} referenced blob(s) missing from OPFS — ` +
            `these file(s) restored EMPTY and will NOT re-sync from peers (already-integrated rows are never re-sent): ` +
            `${affected.length ? affected.join(', ') : '(paths undetermined)'} [hashes: ${missing.join(', ')}]`,
        );
      }
      writtenBlobs.set(id, present); // already on disk — persist won't rewrite these
      clearedOldState.add(id); // this vault is already split; no legacy state to clear
    } else {
      // Back-compat: pre-split vaults kept the whole engine (rows + every blob) in
      // one `.state` blob. Load it; the next persist migrates to the split layout.
      const state = await readBytes(stateName(id));
      if (state) eng.load_state(state);
    }
    engines.set(id, eng);
    return eng;
  }
  // Persist as a tiny rows-only snapshot plus one content-addressed entry per NEW
  // blob (immutable ⇒ never rewritten). This both fixes the browser-clone OOM (no
  // single giant buffer holding every blob) and the per-keystroke cost the old
  // comment flagged — an edit writes `.rows` + only the one new blob it created.
  //
  // Durability note: for locally-authored edits, OPFS is the ONLY copy until a peer
  // pulls the row — it is NOT a "reload re-syncs" cache. Once a row is integrated,
  // version-vector anti-entropy considers it delivered, so a lost blob is never
  // re-fetched from peers (see engineFor's missing-blob diagnostic). We flush on
  // page-hide so nothing pending is lost, and the write order below guards the rest.
  //
  // WRITE ORDER IS A DURABILITY INVARIANT: the rows snapshot references blobs by
  // hash, and OPFS createWritable/write/close is atomic per file. If a tab is
  // killed mid-persist (the ~700ms coalescer window fires after every edit), a
  // durable `.rows` must never point at a blob that isn't on disk yet. So we write
  // every NEW blob and the blob index FIRST, and the rows snapshot LAST — a crash
  // between the two just drops the latest edit (recovered on the next sync), never
  // leaves durable rows referencing an absent blob (which would materialize the
  // file as permanently EMPTY — see the missing-blob handling in engineFor).
  const persist = async (id: string, eng: WasmEngine): Promise<void> => {
    const written = writtenBlobs.get(id) ?? new Set<string>();
    const hashes = JSON.parse(eng.blob_hashes()) as string[];
    let changed = false;
    for (const h of hashes) {
      if (written.has(h)) continue;
      const bytes = eng.get_blob(h);
      if (bytes) {
        await writeBytes(blobName(id, h), bytes);
        written.add(h);
        changed = true;
      }
    }
    writtenBlobs.set(id, written);
    if (changed) await writeJson(blobIndexName(id), [...written]);
    // Rows LAST: every blob it references is now durable (invariant above).
    await writeBytes(rowsName(id), eng.export_rows_state());
    // Reclaim the legacy combined blob once (a no-op if this vault never had one).
    if (!clearedOldState.has(id)) {
      clearedOldState.add(id);
      await removeFile(stateName(id));
    }
  };
  const persistQueue = makeCoalescer<WasmEngine>((id, eng) => void persist(id, eng).catch(() => {}), 700);
  if (typeof document !== 'undefined') {
    document.addEventListener('visibilitychange', () => {
      if (document.visibilityState === 'hidden') persistQueue.flush();
    });
    window.addEventListener('pagehide', () => persistQueue.flush());
  }

  const fileList = (eng: WasmEngine): FileEntry[] => {
    const detail = JSON.parse(eng.files_detail_json()) as { file_id: string; path: string; merge_class: string; deleted: boolean }[];
    return detail
      .filter((f) => !f.deleted)
      .map((f) => ({ path: f.path, file_id: f.file_id, is_dir: f.merge_class === 'dir', merge_class: f.merge_class }));
  };

  const info = (e: RegEntry): VaultInfo => ({ id: e.id, path: '', vault_id: e.vault_id, enabled: true, listening_ticket: null });

  // Look up the upstream a vault was cloned from, so the poll-driven `syncNow`
  // (called without an explicit ticket) can re-dial it.
  const upstreamOf = async (id: string): Promise<{ ticket: string; authKey?: string | null } | null> => {
    const entry = (await registry()).find((r) => r.id === id);
    return entry?.ticket ? { ticket: entry.ticket, authKey: entry.authKey ?? null } : null;
  };

  return {
    listVaults: async () => (await registry()).map(info),

    // No background reopen on web (the registry is read synchronously above), so
    // the vault list is ready as soon as the app mounts.
    vaultsReady: async () => true,

    getIdentity: async () => {
      await ensureWasm();
      // A throwaway engine just to derive the device's ssh identity.
      return new WasmEngine(await deviceSeed(), randHex(8)).node_ssh();
    },

    createVault: async (_name: string): Promise<VaultInfo> => {
      await ensureWasm();
      const id = 'w_' + randHex(8);
      const vault_id = randHex(16);
      const eng = new WasmEngine(await deviceSeed(), vault_id);
      eng.record_write('README.md', enc.encode(WELCOME_MD));
      engines.set(id, eng);
      await persist(id, eng);
      const reg = await registry();
      reg.unshift({ id, vault_id });
      await writeJson('registry.json', reg);
      return info({ id, vault_id });
    },

    cloneRemote: async (_dest: string, ticket: string, authKey?: string, onProgress?: CloneProgress): Promise<VaultInfo> => {
      await ensureWasm();
      const id = 'w_' + randHex(8);
      const eng = new WasmEngine(await deviceSeed(), ''); // empty → adopt the peer's vault
      let lastDone = 0;
      let lastTotal = 0;
      // Stream catch-up progress to the UI, then flip to the 'saving' phase for the
      // (one) OPFS write — on a big vault that final write is itself a few seconds.
      const cb = onProgress
        ? (done: number, total: number) => {
            lastDone = done;
            lastTotal = total;
            onProgress(done, total, 'receiving');
          }
        : undefined;
      await eng.sync(ticket, authKey ?? null, null, cb);
      const vault_id = eng.vault_id();
      engines.set(id, eng);
      onProgress?.(lastDone, lastTotal || lastDone, 'saving');
      await persist(id, eng);
      const reg = await registry();
      // Remember the upstream so the live connection can re-dial it — a browser
      // node has no other way to pull a peer's later pushes.
      reg.unshift({ id, vault_id, ticket, authKey: authKey ?? null });
      await writeJson('registry.json', reg);
      return info({ id, vault_id });
    },

    // Clone a git repo into a new OPFS vault via the relay CORS proxy (git-bridge
    // §7.3). The wasm engine owns the git protocol + import; `gitFetch` supplies the
    // browser transport. Clone/pull only — the browser never pushes (spec non-goal).
    cloneGit: async (_dest: string, url: string, token: string | undefined, depth: number | undefined, allBranches: boolean | undefined, onProgress?: CloneProgress): Promise<VaultInfo> => {
      await ensureWasm();
      const proxyBase = gitProxyBase();
      const id = 'w_' + randHex(8);
      const eng = new WasmEngine(await deviceSeed(), ''); // pristine → adopts the repo-derived vault id
      // wasm fires (phase, done, total); the Api progress cb wants (done, total, phase).
      const cb = onProgress ? (phase: string, done: number, total: number) => onProgress(done, total, phase as ClonePhase) : undefined;
      // `allBranches` (git-open-branches §5): the wasm engine reads the ls-refs
      // advertisement and imports every unmerged branch as a live ASP branch. Imported
      // open branches are ordinary ASP branches, so nothing extra is stored in the
      // registry (`git` entry unchanged).
      const reportJson = await eng.git_clone(url, token ?? undefined, proxyBase, depth ?? undefined, allBranches ?? false, gitFetch, cb);
      const report = JSON.parse(reportJson) as { root_sha: string; remote_ref: string; default_branch: string; open_branches: number; refs_skipped: number };
      // The modal has no clone-report surface today (cloneGit returns a VaultInfo, not a
      // report), so surface the open-branch counts in the console for now.
      if (allBranches) console.info(`git clone: imported ${report.open_branches} open branch(es), skipped ${report.refs_skipped} reachable ref(s)`);
      const vault_id = eng.vault_id();
      engines.set(id, eng);
      onProgress?.(0, 0, 'saving');
      await persist(id, eng);
      const reg = await registry();
      reg.unshift({
        id,
        vault_id,
        git: { url, proxyBase, rootSha: report.root_sha, remoteRef: report.remote_ref, defaultBranch: report.default_branch },
        gitToken: token,
      });
      await writeJson('registry.json', reg);
      return info({ id, vault_id });
    },

    gitPull: async (id: string) => {
      await gitPullOnce(id);
    },

    // The status chip DTO, computed from the fold ledger + registry config. Web is
    // clone/pull only, so it never freezes (no force-push detection) and doesn't
    // compute ahead/behind here — those are native-side (git-bridge §4.4, §5); the
    // shared DTO keeps the shape identical to the desktop `git_status`.
    gitStatus: async (id: string): Promise<GitStatus | null> => {
      const entry = (await registry()).find((r) => r.id === id);
      if (!entry?.git) return null;
      const eng = await engineFor(id);
      const { at_sha } = JSON.parse(eng.git_ledger_json()) as { at_sha: string | null; ingested: number };
      return { remoteUrl: entry.git.url, atSha: at_sha, frozen: false, ahead: 0, behind: 0, policy: 'manual' };
    },

    // Pushing is a spec non-goal in the browser (no outbound smart-HTTP; clone/pull
    // only) — reject clearly so the UI can point the user at the desktop app or CLI.
    gitPush: async (): Promise<never> => {
      throw new Error("Pushing to git from the browser isn't supported — use the desktop app or CLI.");
    },

    // The browser never pushes, so there's nothing pending to show — return an empty
    // diff rather than throwing (harmless, keeps callers uniform across surfaces).
    gitPendingDiff: async (): Promise<PendingDiff> => ({ filesChanged: 0, paths: [], unified: '' }),

    // Hold a live connection to the upstream open, reconnecting if it drops. The
    // engine integrates remote pushes in realtime and fires `onChange` so the UI
    // refreshes; locally-authored rows push out over the same connection. A vault
    // with no upstream (created locally) has nothing to connect to.
    startLiveSync: async (id, onChange) => {
      if (live.has(id)) return;
      const entry = (await registry()).find((r) => r.id === id);
      const up = entry?.ticket ? { ticket: entry.ticket, authKey: entry.authKey ?? null } : null;
      if (!up && !entry?.git) return; // nothing to follow
      const handle: { stop: boolean; gitTimer?: ReturnType<typeof setInterval> } = { stop: false };
      live.set(id, handle);
      // A git-configured vault polls the remote every 60s while the tab is open. This
      // is a fallback: a vault that ALSO holds a live ASP link to a native bridge peer
      // receives git updates over ordinary sync for free (no git traffic), so this
      // tick is belt-and-braces (git-bridge §7.3).
      if (entry?.git) {
        handle.gitTimer = setInterval(() => {
          void gitPullOnce(id)
            .then((n) => {
              if (n > 0) {
                try {
                  onChange();
                } catch {
                  /* listener threw — ignore */
                }
              }
            })
            .catch(() => {});
        }, 60_000);
      }
      if (!up) return; // git-only vault: the poll tick above is the whole story
      const eng = await engineFor(id);
      void (async () => {
        while (!handle.stop) {
          try {
            // Resolves when the connection closes; on_change fires per remote push.
            await eng.connect_live(up.ticket, up.authKey ?? undefined, null, () => {
              persistQueue.schedule(id, eng);
              try {
                onChange();
              } catch {
                /* listener threw — ignore */
              }
            });
          } catch {
            /* connect/dial failed — back off and retry below */
          }
          if (handle.stop) break;
          await new Promise((r) => setTimeout(r, 1500)); // reconnect backoff
        }
        live.delete(id);
      })();
    },

    stopLiveSync: async (id) => {
      const h = live.get(id);
      if (h) {
        h.stop = true;
        if (h.gitTimer) clearInterval(h.gitTimer);
      }
      live.delete(id);
      persistQueue.flushKey(id); // leaving the vault — write its latest state now
    },

    syncNow: async (id, ticket, authKey) => {
      // Called from the editor poll with no ticket: fall back to the upstream we
      // cloned from. A vault created locally (no upstream) simply has nothing to
      // sync against, so this is a no-op.
      let t = ticket;
      let k = authKey;
      if (!t) {
        const up = await upstreamOf(id);
        if (!up) return;
        t = up.ticket;
        k = up.authKey ?? undefined;
      }
      const eng = await engineFor(id);
      await eng.sync(t, k ?? null, null);
      persistQueue.schedule(id, eng);
    },

    getStatus: async (id): Promise<VaultStatus> => {
      const eng = await engineFor(id);
      return { id, vault_id: eng.vault_id(), rows: eng.row_count(), files: fileList(eng).length, head: '', listening_ticket: null, peers: [], last_ts: null };
    },

    listFiles: async (id) => fileList(await engineFor(id)),

    readFile: async (id, path) => {
      const bytes = (await engineFor(id)).read_file(path);
      return bytes ? dec.decode(bytes) : '';
    },

    writeFile: async (id, path, content) => {
      const eng = await engineFor(id);
      eng.record_write(path, enc.encode(content));
      persistQueue.schedule(id, eng);
    },

    renameFile: async (id, oldPath, newPath) => {
      const eng = await engineFor(id);
      eng.record_rename(oldPath, newPath);
      persistQueue.schedule(id, eng);
    },

    deleteFile: async (id, path) => {
      const eng = await engineFor(id);
      eng.record_remove(path);
      persistQueue.schedule(id, eng);
    },

    // Empty directories aren't first-class in the thin (wasm) engine — the folder
    // shows optimistically in the UI and persists once it holds a file.
    createDir: async () => {},

    // ---- branches (§2, §7): the SAME wasm engine the desktop drives ----
    listBranches: async (id) => {
      const eng = await engineFor(id);
      const head = eng.current_branch();
      return (JSON.parse(eng.branches_json()) as { branch_id: string; name: string; parent: string | null }[]).map(
        (b) => ({ branch_id: b.branch_id, name: b.name, parent: b.parent ?? null, current: b.branch_id === head }),
      );
    },
    currentBranch: async (id) => (await engineFor(id)).current_branch(),
    branchGraph: async (id, cap) => JSON.parse((await engineFor(id)).graph_json(cap)),
    createBranch: async (id, name) => {
      const eng = await engineFor(id);
      const bid = eng.create_branch(name, eng.current_branch());
      persistQueue.schedule(id, eng);
      return bid;
    },
    checkoutBranch: async (id, branchId) => {
      // HEAD is per-device (not synced/persisted) — switching just re-materializes
      // the in-memory working set; the caller re-reads the files.
      (await engineFor(id)).checkout(branchId);
    },
    forkBranchAt: async (id, name, ts) => {
      const eng = await engineFor(id);
      const bid = eng.fork_at(name, ts);
      persistQueue.schedule(id, eng);
      return bid;
    },
    deleteBranch: async (id, branchId) => {
      const eng = await engineFor(id);
      eng.delete_branch(branchId);
      persistQueue.schedule(id, eng);
    },

    // ---- tags: named markers at points in history (synced like branch records) ----
    listTags: async (id) =>
      JSON.parse((await engineFor(id)).tags_json()) as { tag_id: string; name: string; at_ts: number; branch_id: string }[],
    createTag: async (id, name, atTs) => {
      const eng = await engineFor(id);
      const tid = eng.create_tag(name, atTs);
      persistQueue.schedule(id, eng);
      return tid;
    },
    deleteTag: async (id, tagId) => {
      const eng = await engineFor(id);
      eng.delete_tag(tagId);
      persistQueue.schedule(id, eng);
    },

    // Time-travel now works on web too (the wasm engine folds as-of a timestamp,
    // same as native) — so scrubbing history + editing-in-the-past auto-branch work
    // in the browser, not just desktop.
    history: async (id) => JSON.parse((await engineFor(id)).history_json()) as HistEvent[],
    readFileAt: async (id, path, ts): Promise<FileAt> =>
      JSON.parse((await engineFor(id)).file_at_json(path, ts)) as FileAt,
    restoreFileAt: async (id, path, ts) => {
      const eng = await engineFor(id);
      eng.restore_file_at(path, ts);
      persistQueue.schedule(id, eng);
    },
    rescan: async () => {},

    removeVault: async (id) => {
      persistQueue.cancel(id); // don't let a debounced write resurrect the state file
      engines.delete(id);
      writtenBlobs.delete(id);
      clearedOldState.delete(id);
      // Delete every content-addressed blob this vault wrote (from its index),
      // then the index, rows snapshot, and any legacy combined state.
      const hashes = await readJson<string[]>(blobIndexName(id), []);
      for (const h of hashes) await removeFile(blobName(id, h));
      await removeFile(blobIndexName(id));
      await removeFile(rowsName(id));
      await removeFile(stateName(id));
      const reg = (await registry()).filter((r) => r.id !== id);
      await writeJson('registry.json', reg);
    },

    // Browsers can't accept inbound connections (no listening socket), so sharing
    // FROM a web node isn't available; these are no-ops on web.
    addLocalFolder: () => Promise.reject(new Error('addLocalFolder is desktop-only')),
    setAllowConnections: async () => null,
    // A browser can't co-host a relay (no listening socket); always off on web.
    setLocalRelay: async () => false,
    getLocalRelay: async () => false,
    authorize: async () => {},
    createSnapshot: async () => '',
    restore: async () => {},

    // No native file manager in the browser — revealing a folder is a no-op.
    revealPath: async () => {},
  };
}
