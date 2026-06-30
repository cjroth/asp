// Build a nested file tree from the backend's flat `FileEntry[]` (slash paths),
// and flatten it back to indented rows honoring the `expanded` map. Directories
// come both as explicit `is_dir` entries (asp-core materializes empty dirs) and
// as implied parents of file paths — we union both.
import type { FileEntry } from '../lib/api';

export interface TreeNode {
  type: 'file' | 'dir';
  name: string;
  path: string;
  children?: TreeNode[];
}

export function buildTree(files: FileEntry[]): TreeNode[] {
  const root: TreeNode = { type: 'dir', name: '', path: '', children: [] };
  // A path -> dir-node index and a seen-path set make the build O(total path
  // segments) instead of O(N × siblings): the old linear `find`/`some` scans
  // turned a 28k-file vault (with big flat directories) into a quadratic stall.
  const dirIndex = new Map<string, TreeNode>([['', root]]);
  const seen = new Set<string>();

  const ensureDir = (parts: string[], upto: number): TreeNode => {
    let node = root;
    let acc = '';
    for (let i = 0; i < upto; i++) {
      acc = acc ? acc + '/' + parts[i] : parts[i];
      let next = dirIndex.get(acc);
      if (!next) {
        next = { type: 'dir', name: parts[i], path: acc, children: [] };
        node.children!.push(next);
        dirIndex.set(acc, next);
      }
      node = next;
    }
    return node;
  };

  for (const f of files) {
    if (seen.has(f.path)) continue;
    const parts = f.path.split('/').filter(Boolean);
    if (parts.length === 0) continue;
    seen.add(f.path);
    if (f.is_dir) {
      ensureDir(parts, parts.length);
      continue;
    }
    const parent = ensureDir(parts, parts.length - 1);
    parent.children!.push({ type: 'file', name: parts[parts.length - 1], path: f.path });
  }

  const sortRec = (n: TreeNode) => {
    if (!n.children) return;
    n.children.sort(compareNodes);
    n.children.forEach(sortRec);
  };
  sortRec(root);
  return root.children!;
}

// The design's row order: ALL-CAPS note stems (e.g. README.md) float to the top,
// then folders, then files — each group ordered by natural (numeric) name.
const stemOf = (s: string): string => {
  const i = s.lastIndexOf('.');
  return i > 0 ? s.slice(0, i) : s;
};
const capRank = (n: TreeNode): number => {
  const st = stemOf(n.name);
  return n.type === 'file' && /[A-Za-z]/.test(st) && st === st.toUpperCase() ? 0 : 1;
};
// Folders before files. The ALL-CAPS notes are the documented exception — they
// outrank everything (including folders) via capRank above.
const dirRank = (n: TreeNode): number => (n.type === 'dir' ? 0 : 1);
export function compareNodes(a: TreeNode, b: TreeNode): number {
  return (
    capRank(a) - capRank(b) ||
    dirRank(a) - dirRank(b) ||
    a.name.localeCompare(b.name, undefined, { numeric: true, sensitivity: 'base' })
  );
}

export interface FlatRow {
  node: TreeNode;
  depth: number;
}

export function flatten(tree: TreeNode[], expanded: Record<string, boolean>, depth = 0): FlatRow[] {
  const out: FlatRow[] = [];
  for (const node of tree) {
    out.push({ node, depth });
    if (node.type === 'dir' && expanded[node.path] && node.children) {
      out.push(...flatten(node.children, expanded, depth + 1));
    }
  }
  return out;
}

// Every directory path in the tree (for "expand all" when opening a vault).
export function allDirPaths(tree: TreeNode[]): string[] {
  const out: string[] = [];
  const walk = (nodes: TreeNode[]) => {
    for (const n of nodes) {
      if (n.type === 'dir') {
        out.push(n.path);
        if (n.children) walk(n.children);
      }
    }
  };
  walk(tree);
  return out;
}

// First README (any depth) else the first file — the default selection. (Free
// "untitled" naming now lives in format.ts as `freeName`.)
export function firstSelectable(tree: TreeNode[]): string | null {
  let firstFile: string | null = null;
  let readme: string | null = null;
  const walk = (nodes: TreeNode[]) => {
    for (const n of nodes) {
      if (n.type === 'file') {
        if (firstFile == null) firstFile = n.path;
        if (readme == null && /readme/i.test(n.name)) readme = n.path;
      } else if (n.children) {
        walk(n.children);
      }
    }
  };
  walk(tree);
  return readme || firstFile;
}
