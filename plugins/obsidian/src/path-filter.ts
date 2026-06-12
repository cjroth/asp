// Scope filtering (the plugin's view of §scope). A set of well-known dirs is
// ALWAYS out of scope regardless of any ignore file; a `.aspignore` adds further
// gitignore-style excludes. Mirrors the core matcher's semantics but in TS (host
// glue only — the engine owns convergence).

export class PathFilter {
  private patterns: { glob: string; negate: boolean }[] = [];

  // Always ignored, no matter what the ignore file says. These are never vault
  // content and syncing them causes real harm:
  //   .asp / .context — the engine's private/home dir (current + legacy name);
  //     .context holds the node's PRIVATE KEY, which must never leave the device.
  //   .git           — version-control internals (huge packs, no merge value).
  //   .obsidian      — the editor's config AND this plugin's own main.js + data +
  //     persisted engine-state.bin. Syncing it is self-referential: a dump lands
  //     inside the synced tree, the next reconcile captures it, the next dump
  //     contains the previous dump — an exponential blow-up that also re-imports
  //     the plugin binary as `main (1).js`, `main (2).js`… (the duplicate loop).
  //   .trash         — Obsidian's local trash.
  private static readonly HARD_IGNORE_DIRS = ['.asp', '.context', '.git', '.obsidian', '.trash'];

  constructor(aspignore = '') {
    for (const raw of aspignore.split('\n')) {
      const line = raw.trim();
      if (!line || line.startsWith('#')) continue;
      const negate = line.startsWith('!');
      this.patterns.push({ glob: negate ? line.slice(1) : line, negate });
    }
  }

  ignored(path: string): boolean {
    // A hard-ignored dir at ANY depth — non-overridable. Checking every segment
    // (not just the first) matters: vaults often hold cloned repos as reference
    // material, so `notes/proj/.git/objects/pack/…` must be ignored too — those
    // multi-MB packs were a second source of the bloat-and-dup explosion. Also
    // catches `.DS_Store` anywhere.
    const segs = path.split('/');
    if (segs.some((s) => PathFilter.HARD_IGNORE_DIRS.includes(s) || s === '.DS_Store')) {
      return true;
    }
    let ignored = false;
    for (const p of this.patterns) {
      if (this.match(p.glob, path)) ignored = !p.negate;
    }
    return ignored;
  }

  private match(glob: string, path: string): boolean {
    const g = glob.endsWith('/') ? glob.slice(0, -1) : glob;
    // `*.ext` matches any segment; `dir/` matches a prefix; otherwise exact-ish.
    const re = new RegExp(
      `^${g.replace(/[.+^${}()|[\]\\]/g, '\\$&').replace(/\*\*/g, ' ').replace(/\*/g, '[^/]*').replace(/ /g, '.*')}(/.*)?$`,
    );
    if (re.test(path)) return true;
    // unanchored: match any suffix segment
    return path.split('/').some((_, i, parts) => re.test(parts.slice(i).join('/')));
  }
}
