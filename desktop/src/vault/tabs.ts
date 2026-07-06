// Open-tabs + active-file-in-URL model. Pure, side-effect-free helpers (plus a
// thin localStorage shim that mirrors prefs.ts' try/catch persistence) so the
// whole tab/hash story is unit-testable in isolation from React.
//
// URL scheme — the ACTIVE file lives in the location HASH (works in the web
// build AND the Tauri custom-protocol webview, where path-based routing is not
// available):
//
//   #<encodeURIComponent(vaultId)>/<encodeURIComponent(filePath)>
//
// Both halves are fully percent-encoded, so the single literal "/" separating
// them is unambiguous even when the path itself contains "/", spaces or unicode.
// The list of OPEN tabs is kept per-vault in localStorage (key asp.tabs.<vaultId>)
// — only the active one rides in the URL.

// ---------- URL hash <-> {vaultId, path} ----------

export function buildHash(vaultId: string, path: string): string {
  return `#${encodeURIComponent(vaultId)}/${encodeURIComponent(path)}`;
}

export function parseHash(hash: string): { vaultId: string; path: string } | null {
  if (!hash) return null;
  // Tolerate both "#foo/bar" (location.hash) and a bare "foo/bar".
  const body = hash.charAt(0) === '#' ? hash.slice(1) : hash;
  if (!body) return null;
  const slash = body.indexOf('/');
  if (slash <= 0 || slash === body.length - 1) return null; // need non-empty both sides
  try {
    // Both sides are guaranteed non-empty by the slash-position checks above.
    const vaultId = decodeURIComponent(body.slice(0, slash));
    const path = decodeURIComponent(body.slice(slash + 1));
    return { vaultId, path };
  } catch {
    return null; // malformed percent-encoding
  }
}

// ---------- per-vault open-tabs persistence ----------

const tabsKey = (vaultId: string): string => `asp.tabs.${vaultId}`;

export function loadOpenTabs(vaultId: string): string[] {
  try {
    const raw = localStorage.getItem(tabsKey(vaultId));
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    if (Array.isArray(parsed)) return parsed.filter((p): p is string => typeof p === 'string');
    return [];
  } catch {
    return [];
  }
}

export function saveOpenTabs(vaultId: string, tabs: string[]): void {
  try {
    localStorage.setItem(tabsKey(vaultId), JSON.stringify(tabs));
  } catch {
    /* ignore */
  }
}

// ---------- per-vault expanded-group persistence (wave C accordion) ----------
// The set of history-graph prefix groups the user has EXPANDED, remembered per
// vault (same localStorage try/catch idiom as the open-tabs list above). Absence
// (null) means "no explicit choice yet" → the caller applies the size-based
// default (collapse a branch-farm, leave a small graph expanded).

const groupsKey = (vaultId: string): string => `asp.groups.${vaultId}`;

/** Persisted expanded prefixes for a vault, or null if the user hasn't chosen. */
export function loadExpandedGroups(vaultId: string): string[] | null {
  try {
    const raw = localStorage.getItem(groupsKey(vaultId));
    if (raw == null) return null;
    const parsed = JSON.parse(raw);
    if (Array.isArray(parsed)) return parsed.filter((p): p is string => typeof p === 'string');
    return null;
  } catch {
    return null;
  }
}

export function saveExpandedGroups(vaultId: string, prefixes: string[]): void {
  try {
    localStorage.setItem(groupsKey(vaultId), JSON.stringify(prefixes));
  } catch {
    /* ignore */
  }
}

// ---------- tab-list transforms (pure) ----------

// Append `path` if it isn't already open (focus is handled by the caller via the
// active file). Returns the same array reference when nothing changes.
export function withTab(tabs: string[], path: string): string[] {
  return tabs.includes(path) ? tabs : [...tabs, path];
}

// Close `path`. When it's the ACTIVE tab, pick the right neighbor to become
// active: prefer the next tab, else the previous, else null (no tabs left).
// Closing a non-active tab leaves the active file untouched.
export function closeTab(
  tabs: string[],
  active: string | null,
  path: string,
): { tabs: string[]; active: string | null } {
  const idx = tabs.indexOf(path);
  if (idx === -1) return { tabs, active };
  const next = tabs.filter((t) => t !== path);
  if (path !== active) return { tabs: next, active };
  if (next.length === 0) return { tabs: next, active: null };
  // After removing the element at `idx`, the tab that *followed* it now sits at
  // `idx`. If `path` was last, fall back to the new last (the previous tab).
  const nextActive = idx < next.length ? next[idx] : next[next.length - 1];
  return { tabs: next, active: nextActive };
}

// Remap a renamed/moved file — and any tab living under a renamed/moved FOLDER
// (the `oldPath + '/'` subtree prefix). De-dupes if a remap collides with an
// already-open tab, preserving first-seen order.
export function remapTabs(tabs: string[], oldPath: string, newPath: string): string[] {
  const out: string[] = [];
  for (const t of tabs) {
    let mapped = t;
    if (t === oldPath) mapped = newPath;
    else if (t.startsWith(oldPath + '/')) mapped = newPath + t.slice(oldPath.length);
    if (!out.includes(mapped)) out.push(mapped);
  }
  return out;
}

// Drop every tab that matches one of `paths` exactly, or that lives under one of
// them as a folder subtree (`p + '/'`).
export function removeTabs(tabs: string[], paths: string[]): string[] {
  const set = new Set(paths);
  return tabs.filter((t) => !set.has(t) && !paths.some((p) => t.startsWith(p + '/')));
}

// Move the tab at index `from` to index `to` (drag-to-reorder). A no-op or any
// out-of-range index returns the original array unchanged.
export function reorderTabs(tabs: string[], from: number, to: number): string[] {
  if (from === to) return tabs;
  if (from < 0 || from >= tabs.length || to < 0 || to >= tabs.length) return tabs;
  const next = tabs.slice();
  const [moved] = next.splice(from, 1);
  next.splice(to, 0, moved);
  return next;
}

// ---------- multi-close transforms (pure) ----------
// Each returns the new tab-path array; active-file reassignment is the caller's
// job. These CLOSE tabs only — no file is ever deleted.

// Close every tab EXCEPT `path` (the right-clicked tab). A `path` that isn't open
// keeps nothing (→ []).
export function closeOthers(tabs: string[], path: string): string[] {
  return tabs.filter((t) => t === path);
}

// Close every tab to the LEFT of `path`, keeping `path` and everything after it.
// A `path` that isn't present leaves the list unchanged (same ref).
export function closeToLeft(tabs: string[], path: string): string[] {
  const idx = tabs.indexOf(path);
  return idx === -1 ? tabs : tabs.slice(idx);
}

// Close every tab to the RIGHT of `path`, keeping `path` and everything before it.
// A `path` that isn't present leaves the list unchanged (same ref).
export function closeToRight(tabs: string[], path: string): string[] {
  const idx = tabs.indexOf(path);
  return idx === -1 ? tabs : tabs.slice(0, idx + 1);
}

// Close ALL tabs.
export function closeAll(): string[] {
  return [];
}
