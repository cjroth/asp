// "Pretty filenames" + hidden-file helpers — a faithful port of the design's
// `prettyName` / hidden predicate. Pure string transforms; no I/O.

export function isHidden(name: string): boolean {
  return name.charAt(0) === '.';
}

const titleize = (s: string): string =>
  s
    .split(/\s+/)
    .map((w) => (w ? w.charAt(0).toUpperCase() + w.slice(1).toLowerCase() : w))
    .join(' ');

export interface PrettyLabel {
  label: string;
  italic: boolean;
}

// Turn a raw filename into a human label. Dotfiles are shown verbatim; dirs and
// notes are titleized (dashes/underscores → spaces); an ALL-CAPS note stem (e.g.
// README.md) is titleized but flagged italic to hint it was a shouting filename.
export function prettyName(name: string, isDir: boolean): PrettyLabel {
  if (name.charAt(0) === '.') return { label: name, italic: false };
  if (isDir) return { label: titleize(name.replace(/[-_]+/g, ' ')), italic: false };
  if (/\.md$/i.test(name)) {
    const base = name.replace(/\.md$/i, '');
    const allCaps = /^[A-Z0-9]+$/.test(base) && /[A-Z]/.test(base);
    return { label: titleize(base.replace(/[-_]+/g, ' ')), italic: allCaps };
  }
  return { label: name, italic: false };
}
