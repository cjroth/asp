// A tiny line-level diff for the history "what changed" popup. Not a full Myers
// diff — it trims the common prefix and suffix and reports the changed middle as
// removed + added blocks, which reads well for the typical single-region edit and
// is cheap + dependency-free. Pure, so it's unit-testable without a DOM.

export interface LineDiff {
  /** 1-based line number in the "before" where the change starts. */
  atLine: number;
  removed: string[];
  added: string[];
  /** A few unchanged lines just before the change, for context. */
  context: string[];
  /** True when before and after are identical. */
  unchanged: boolean;
}

export function lineDiff(before: string, after: string, contextLines = 2): LineDiff {
  if (before === after) return { atLine: 0, removed: [], added: [], context: [], unchanged: true };
  const a = before.length ? before.split('\n') : [];
  const b = after.length ? after.split('\n') : [];
  let start = 0;
  while (start < a.length && start < b.length && a[start] === b[start]) start++;
  let endA = a.length;
  let endB = b.length;
  while (endA > start && endB > start && a[endA - 1] === b[endB - 1]) {
    endA--;
    endB--;
  }
  return {
    atLine: start + 1,
    removed: a.slice(start, endA),
    added: b.slice(start, endB),
    context: a.slice(Math.max(0, start - contextLines), start),
    unchanged: false,
  };
}
