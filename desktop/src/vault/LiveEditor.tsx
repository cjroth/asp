// The live WYSIWYG editor — a contentEditable surface managed imperatively (React
// never owns its children). It repaints only when `paintKey` changes (a new
// selection or a time-travel instant), so typing never triggers a React re-render
// of its content and the caret stays put. Markdown files render with the live
// markdown highlighter; code files render with the per-line syntax highlighter.
import React, { useEffect, useRef } from 'react';
import { applyCachedDiagrams, renderDiagrams } from './diagram';
import { caretOffset, hasFrontmatter, isCodeFile, lineIndexOf, readLive, renderDoc, setCaret, setTextOffsetIn, textOffsetIn } from './markdown';
import { loadMermaid } from './mermaid';
import type { FrontmatterStyle } from './prefs';

// Open a link/image-badge URL outside the editor. There is no in-app "open URL"
// backend (api.ts only reveals paths in the OS file manager), so we hand the URL
// to the platform: on desktop Tauri intercepts `window.open(_blank)` and hands it
// to the OS browser; on web it opens a new tab. `noopener` severs the opener ref.
function openExternal(url: string): void {
  window.open(url, '_blank', 'noopener');
}

export interface LiveEditorProps {
  source: string;
  paintKey: string; // bump to force a repaint from outside (select / scrub)
  path: string; // the selected file path (selects code vs markdown rendering)
  readOnly: boolean;
  notExist: boolean;
  accent: string;
  centered: boolean;
  fontFamily: string;
  frontmatterStyle: FrontmatterStyle;
  onChange: (src: string) => void;
}

export default function LiveEditor(props: LiveEditorProps) {
  const { source, paintKey, path, readOnly, notExist, accent, centered, fontFamily, frontmatterStyle, onChange } = props;
  const ref = useRef<HTMLDivElement | null>(null);
  const composing = useRef(false);
  const paintedKey = useRef<string>('');
  const rehlTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const diagTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const lineCount = useRef(0); // top-level line divs as of the last full render
  // Keep handler-visible values fresh without re-binding (avoids stale closures).
  const roRef = useRef(readOnly);
  const accentRef = useRef(accent);
  const pathRef = useRef(path);
  const fmRef = useRef(frontmatterStyle);
  const changeRef = useRef(onChange);
  roRef.current = readOnly;
  accentRef.current = accent;
  pathRef.current = path;
  fmRef.current = frontmatterStyle;
  changeRef.current = onChange;

  const render = (src: string) => renderDoc(src, pathRef.current, accentRef.current, fmRef.current);

  // Fill in rendered diagram SVGs after a (re)paint. Cached diagrams are replayed
  // synchronously (no flicker for unchanged blocks); any new ones are rendered via
  // the async, dynamically-imported mermaid boundary on a short debounce so a
  // burst of edits coalesces into one render. Diagram previews are
  // contenteditable=false and skipped by the line walkers, so this never disturbs
  // the caret. It degrades to the visible code fallback if mermaid is unavailable.
  const paintDiagrams = (el: HTMLElement) => {
    const pending = applyCachedDiagrams(el);
    if (pending === 0) return;
    if (diagTimer.current) clearTimeout(diagTimer.current);
    diagTimer.current = setTimeout(() => {
      const cur = ref.current;
      if (cur) void renderDiagrams(cur, loadMermaid);
    }, 200);
  };

  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    if (paintedKey.current !== paintKey) {
      paintedKey.current = paintKey;
      el.contentEditable = readOnly ? 'false' : 'true';
      el.style.opacity = readOnly ? '0.92' : '1';
      if (rehlTimer.current) clearTimeout(rehlTimer.current);
      if (notExist) {
        el.innerHTML = '<div style="color:var(--faint2); font-style:italic">This file did not exist at this point in time.</div>';
      } else {
        el.innerHTML = renderDoc(source, path, accent, frontmatterStyle);
        lineCount.current = source.split('\n').length;
      }
    }
    // Always (re)schedule the diagram render — even when the repaint above is
    // skipped for an unchanged paintKey. React 18 StrictMode runs effects
    // setup→cleanup→setup in dev: the cleanup effect below clears the pending
    // diagram timer BETWEEN the two setups, and the guarded repaint won't re-arm
    // it on the second setup. Scheduling here unconditionally re-arms it, so the
    // SVG renders instead of staying stuck on its <pre> code fallback. It only
    // touches contenteditable=false `.md-diagram` nodes and is a no-op once every
    // diagram is rendered, so the caret and repaint behavior are unaffected.
    if (!notExist) paintDiagrams(el);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [paintKey, source, readOnly, notExist, accent, path, frontmatterStyle]);

  // Clean up the pending re-highlight on unmount.
  useEffect(() => () => {
    if (rehlTimer.current) clearTimeout(rehlTimer.current);
    if (diagTimer.current) clearTimeout(diagTimer.current);
  }, []);

  // Re-applying syntax highlighting rebuilds DOM. Rebuilding the WHOLE document is
  // O(document size) — a ~600ms hitch on a 4000-line file. So while typing we let
  // the browser insert characters natively (instant), and on a short pause we
  // re-highlight only the EDITED line. A full re-render happens only when the line
  // count changed (Enter/Backspace/paste), or rendering is context-dependent (a
  // code fence, frontmatter, or a table row — where a line depends on its
  // neighbors). `readLive` reconstructs the exact source either way, so saves are
  // always correct.
  const fullRehighlight = (el: HTMLElement) => {
    const off = caretOffset(el);
    const src = readLive(el);
    el.innerHTML = render(src);
    lineCount.current = src.split('\n').length;
    paintDiagrams(el);
    if (off != null) setCaret(el, off);
  };

  const currentLineDiv = (el: HTMLElement): HTMLElement | null => {
    const sel = getSelection();
    if (!sel || !sel.rangeCount) return null;
    let node: Node | null = sel.getRangeAt(0).endContainer;
    while (node && node.parentNode !== el) node = node.parentNode;
    return node && node.parentNode === el ? (node as HTMLElement) : null;
  };

  const rehighlight = () => {
    const el = ref.current;
    if (!el || roRef.current || composing.current) return;
    const src = readLive(el);
    const lines = src.split('\n');
    const lineDiv = currentLineDiv(el);
    const structural = lines.length !== lineCount.current || el.childNodes.length !== lines.length;
    const idx = lineDiv ? Array.prototype.indexOf.call(el.childNodes, lineDiv) : -1;
    const lineText = idx >= 0 ? lines[idx] ?? '' : '';
    const contextDependent = src.indexOf('```') !== -1 || hasFrontmatter(src) || lineText.indexOf('|') !== -1;
    if (!lineDiv || structural || contextDependent) {
      fullRehighlight(el);
      return;
    }
    const off = textOffsetIn(lineDiv);
    const tmp = document.createElement('div');
    tmp.innerHTML = render(lineText);
    const newDiv = tmp.firstChild as HTMLElement | null;
    /* v8 ignore next 4 -- defensive: render() always yields at least one line div */
    if (!newDiv) {
      fullRehighlight(el);
      return;
    }
    el.replaceChild(newDiv, lineDiv);
    if (off != null) setTextOffsetIn(newDiv, off);
  };

  const onInput = () => {
    const el = ref.current;
    if (!el || roRef.current) return;
    changeRef.current(readLive(el));
    if (composing.current) return;
    if (rehlTimer.current) clearTimeout(rehlTimer.current);
    rehlTimer.current = setTimeout(rehighlight, 320);
  };

  // Clicking the checkbox of a `- [ ]` task line toggles its source between
  // `[ ]` and `[x]`. We preventDefault so no caret/selection lands in the line,
  // re-render just that line div, and commit the new source via onChange. The
  // `.cm-task-box` hit target carries no source text, so readLive is unaffected.
  const onMouseDown = (e: React.MouseEvent<HTMLDivElement>) => {
    const el = ref.current;
    if (!el) return;
    // Clicking an inline link or image badge that carries a URL opens it
    // externally. We preventDefault so no caret/selection lands in the
    // contentEditable line (which would corrupt the document). This runs even when
    // read-only — opening a URL never mutates the source — and is keyed off
    // `[data-href]`, a different target than the `.cm-task-box` checkbox below.
    const link = (e.target as HTMLElement).closest?.('.cm-link[data-href], .cm-img[data-href]') as HTMLElement | null;
    const href = link?.getAttribute('data-href');
    if (href) {
      e.preventDefault();
      openExternal(href);
      return;
    }
    if (roRef.current) return;
    const box = (e.target as HTMLElement).closest?.('.cm-task-box');
    if (!box) return;
    const lineDiv = box.closest('.cm-task') as HTMLElement | null;
    if (!lineDiv || lineDiv.parentNode !== el) return;
    // Map to the SOURCE-line index in readLive's flattened space (tables expanded,
    // diagrams skipped) — NOT the raw el.childNodes index, which a preceding table
    // or diagram preview shifts out of sync, making us read/toggle the wrong line.
    const idx = lineIndexOf(el, lineDiv);
    /* v8 ignore next -- lineDiv is a line node, so idx is always found */
    if (idx < 0) return;
    const lines = readLive(el).split('\n');
    const m = (lines[idx] ?? '').match(/^(\s*[-*]\s+\[)([ xX])(\].*)$/);
    /* v8 ignore next -- the box only renders on a valid task line, so m matches */
    if (!m) return;
    e.preventDefault();
    lines[idx] = m[1] + (m[2] === ' ' ? 'x' : ' ') + m[3];
    const tmp = document.createElement('div');
    tmp.innerHTML = render(lines[idx]);
    el.replaceChild(tmp.firstChild as HTMLElement, lineDiv);
    changeRef.current(lines.join('\n'));
  };

  const onPaste = (e: React.ClipboardEvent<HTMLDivElement>) => {
    e.preventDefault();
    if (roRef.current) return;
    const t = e.clipboardData.getData('text/plain');
    document.execCommand('insertText', false, t);
  };

  const onCompositionStart = () => {
    composing.current = true;
  };
  const onCompositionEnd = () => {
    composing.current = false;
    const el = ref.current;
    if (!el || roRef.current) return;
    fullRehighlight(el);
    changeRef.current(readLive(el));
  };

  const code = isCodeFile(path);
  const style: React.CSSProperties & Record<string, string | number> = code
    ? {
        width: '100%',
        maxWidth: '100%',
        minHeight: '100%',
        outline: 'none',
        background: 'transparent',
        whiteSpace: 'pre-wrap',
        wordBreak: 'break-word',
        fontFamily: "'JetBrains Mono', ui-monospace, Menlo, monospace",
        fontSize: '13px',
        lineHeight: '1.7',
        color: 'var(--text)',
        padding: '30px 36px 140px',
        tabSize: 2,
        '--accent': accent,
      }
    : {
        width: centered ? '760px' : '100%',
        maxWidth: '100%',
        minHeight: '100%',
        outline: 'none',
        background: 'transparent',
        whiteSpace: 'pre-wrap',
        wordBreak: 'break-word',
        fontFamily,
        fontSize: '15.5px',
        lineHeight: '1.8',
        color: 'var(--text)',
        padding: '44px 40px 140px',
        '--accent': accent,
      };

  return (
    <div
      ref={ref}
      data-testid="live-editor"
      spellCheck={false}
      onMouseDown={onMouseDown}
      onInput={onInput}
      onPaste={onPaste}
      onCompositionStart={onCompositionStart}
      onCompositionEnd={onCompositionEnd}
      suppressContentEditableWarning
      style={style}
    />
  );
}
