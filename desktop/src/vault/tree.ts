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

  const ensureDir = (parts: string[]): TreeNode => {
    let node = root;
    let acc = '';
    for (const part of parts) {
      acc = acc ? acc + '/' + part : part;
      const kids = node.children!;
      let next = kids.find((c) => c.type === 'dir' && c.name === part);
      if (!next) {
        next = { type: 'dir', name: part, path: acc, children: [] };
        kids.push(next);
      }
      node = next;
    }
    return node;
  };

  for (const f of files) {
    const parts = f.path.split('/').filter(Boolean);
    if (parts.length === 0) continue;
    if (f.is_dir) {
      ensureDir(parts);
      continue;
    }
    const name = parts[parts.length - 1];
    const parent = ensureDir(parts.slice(0, -1));
    if (!parent.children!.some((c) => c.type === 'file' && c.name === name)) {
      parent.children!.push({ type: 'file', name, path: f.path });
    }
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
// then everything else — dirs and files intermixed — by natural (numeric) name.
const stemOf = (s: string): string => {
  const i = s.lastIndexOf('.');
  return i > 0 ? s.slice(0, i) : s;
};
const capRank = (n: TreeNode): number => {
  const st = stemOf(n.name);
  return n.type === 'file' && /[A-Za-z]/.test(st) && st === st.toUpperCase() ? 0 : 1;
};
export function compareNodes(a: TreeNode, b: TreeNode): number {
  return capRank(a) - capRank(b) || a.name.localeCompare(b.name, undefined, { numeric: true, sensitivity: 'base' });
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
