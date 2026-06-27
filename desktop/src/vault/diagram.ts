// Rendered diagrams for the live markdown editor. A fenced ```mermaid block (or
// the ```diagram alias) keeps its raw source as ordinary, editable per-line divs
// — exactly like any code fence, so it round-trips byte-for-byte through
// `readLive`. Immediately AFTER the closing fence we emit ONE extra
// `contenteditable=false` `.md-diagram` element that holds the rendered SVG. The
// editor's line walkers (`lineNodes` in markdown.ts) SKIP `.md-diagram`, so the
// preview contributes ZERO lines/characters to the source and the caret math is
// untouched.
//
// Mermaid itself is async + browser-only (it needs a real DOM; no jsdom). The
// real `import('mermaid')` lives behind the thin, injectable loader in
// `./mermaid` so this module's logic is fully unit-testable with a mock loader,
// and rendering degrades gracefully (the source stays visible) when the library
// is unavailable or a diagram fails to parse.

// The slice of mermaid's API we depend on. Kept local so this module never has to
// import the (heavy, browser-only) library type.
export interface MermaidLike {
  initialize?: (config: Record<string, unknown>) => void;
  render: (id: string, src: string) => Promise<{ svg: string }>;
}

// Resolves the mermaid implementation. May reject when the library is missing
// (offline / not installed) — callers treat a rejection as "render unavailable".
export type MermaidLoader = () => Promise<MermaidLike>;

const escAttr = (s: string) =>
  s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
const escText = (s: string) =>
  s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');

// True for the fence info-string of a diagram block: `mermaid` (the standard) or
// the `diagram` alias. Case-insensitive; trailing attributes (e.g. `mermaid foo`)
// still count so an AI assistant's output is recognised.
export function isDiagramLang(info: string): boolean {
  return /^(mermaid|diagram)\b/i.test(info.trim());
}

// The fence info-string (everything after the opening ``` on its line).
export function fenceInfo(line: string): string {
  return line.replace(/^```/, '').trim();
}

// The `.md-diagram` preview element appended after a diagram fence. The source is
// stashed in `data-diagram-src` (so the async renderer can read it back) and a
// `<pre>` code fallback is shown until — or unless — the SVG renders. Because the
// line walkers skip `.md-diagram`, none of this affects the source round-trip.
export function diagramPreviewHtml(source: string): string {
  return (
    '<div class="md-diagram" contenteditable="false" data-diagram-src="' +
    escAttr(source) +
    '"><pre class="md-diagram-fallback">' +
    (escText(source) || '<br>') +
    '</pre></div>'
  );
}

// In-process cache of rendered SVGs keyed by diagram source. A full re-render of
// the editor (which happens on every keystroke while a fence is present) rebuilds
// the `.md-diagram` placeholders from scratch; replaying the cache makes already
// rendered diagrams reappear instantly with no flicker and no re-parse.
const svgCache = new Map<string, string>();
let renderSeq = 0;

// Synchronously fill any `.md-diagram` whose source we've rendered before from the
// cache. Returns the number of nodes still needing an async render. Touches only
// `.md-diagram` elements (never the editable lines), so the caret is unaffected.
export function applyCachedDiagrams(root: ParentNode): number {
  let pending = 0;
  root.querySelectorAll('.md-diagram').forEach((node) => {
    const src = node.getAttribute('data-diagram-src') ?? '';
    const cached = svgCache.get(src);
    if (cached !== undefined) {
      if (node.getAttribute('data-diagram-rendered') !== src) {
        node.innerHTML = cached;
        node.setAttribute('data-diagram-rendered', src);
      }
    } else {
      pending += 1;
    }
  });
  return pending;
}

// Render every not-yet-rendered `.md-diagram` under `root` via mermaid, caching
// results. Resolves quietly (leaving the code fallback in place) when the loader
// rejects or an individual diagram fails to parse — the source is never lost.
export async function renderDiagrams(root: ParentNode, load: MermaidLoader): Promise<void> {
  const nodes = Array.from(root.querySelectorAll('.md-diagram')).filter(
    (n) => n.getAttribute('data-diagram-rendered') !== (n.getAttribute('data-diagram-src') ?? '')
  );
  if (nodes.length === 0) return;
  let mermaid: MermaidLike;
  try {
    mermaid = await load();
  } catch {
    return; // library unavailable → keep the code fallback
  }
  try {
    mermaid.initialize?.({ startOnLoad: false, securityLevel: 'strict' });
  } catch {
    /* v8 ignore next -- initialize is best-effort; render still attempted */
  }
  for (const node of nodes) {
    const src = node.getAttribute('data-diagram-src') ?? '';
    const cached = svgCache.get(src);
    if (cached !== undefined) {
      node.innerHTML = cached;
      node.setAttribute('data-diagram-rendered', src);
      continue;
    }
    try {
      const { svg } = await mermaid.render('md-diagram-' + ++renderSeq, src);
      svgCache.set(src, svg);
      node.innerHTML = svg;
      node.setAttribute('data-diagram-rendered', src);
    } catch {
      // Invalid diagram: leave the `<pre>` source fallback untouched.
    }
  }
}

// Test seam: clear the module-level SVG cache so cases don't leak into each other.
export function _clearDiagramCache(): void {
  svgCache.clear();
}
