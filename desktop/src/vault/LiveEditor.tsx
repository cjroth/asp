// The live WYSIWYG Markdown editor — a contentEditable surface managed
// imperatively (React never owns its children). It repaints only when
// `paintKey` changes (a new selection or a time-travel instant), so typing
// never triggers a React re-render of its content and the caret stays put.
import React, { useEffect, useRef } from 'react';
import { caretOffset, readLive, renderLiveHtml, setCaret, setTextOffsetIn, textOffsetIn } from './markdown';

export interface LiveEditorProps {
  source: string;
  paintKey: string; // bump to force a repaint from outside (select / scrub)
  readOnly: boolean;
  notExist: boolean;
  accent: string;
  centered: boolean;
  fontFamily: string;
  onChange: (src: string) => void;
}

export default function LiveEditor(props: LiveEditorProps) {
  const { source, paintKey, readOnly, notExist, accent, centered, fontFamily, onChange } = props;
  const ref = useRef<HTMLDivElement | null>(null);
  const composing = useRef(false);
  const paintedKey = useRef<string>('');
  const rehlTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const lineCount = useRef(0); // top-level line divs as of the last full render
  // Keep handler-visible values fresh without re-binding (avoids stale closures).
  const roRef = useRef(readOnly);
  const accentRef = useRef(accent);
  const changeRef = useRef(onChange);
  roRef.current = readOnly;
  accentRef.current = accent;
  changeRef.current = onChange;

  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    if (paintedKey.current === paintKey) return;
    paintedKey.current = paintKey;
    el.contentEditable = readOnly ? 'false' : 'true';
    el.style.opacity = readOnly ? '0.92' : '1';
    if (rehlTimer.current) clearTimeout(rehlTimer.current);
    if (notExist) {
      el.innerHTML = '<div style="color:#b0aaa2; font-style:italic">This file did not exist at this point in time.</div>';
    } else {
      el.innerHTML = renderLiveHtml(source, accent);
      lineCount.current = source.split('\n').length;
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [paintKey, source, readOnly, notExist, accent]);

  // Clean up the pending re-highlight on unmount.
  useEffect(() => () => { if (rehlTimer.current) clearTimeout(rehlTimer.current); }, []);

  // Re-applying syntax highlighting rebuilds DOM. Rebuilding the WHOLE document is
  // O(document size) — a ~600ms hitch on a 4000-line file. So while typing we let
  // the browser insert characters natively (instant), and on a short pause we
  // re-highlight only the EDITED line. A full re-render happens only when the line
  // count changed (Enter/Backspace/paste) or the doc has a code fence (where a
  // line's rendering depends on lines above it). `readLive` reconstructs the exact
  // source either way, so saves are always correct.
  const fullRehighlight = (el: HTMLElement) => {
    const off = caretOffset(el);
    const src = readLive(el);
    el.innerHTML = renderLiveHtml(src, accentRef.current);
    lineCount.current = src.split('\n').length;
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
    if (!lineDiv || structural || src.indexOf('```') !== -1) {
      fullRehighlight(el);
      return;
    }
    const idx = Array.prototype.indexOf.call(el.childNodes, lineDiv);
    const off = textOffsetIn(lineDiv);
    const tmp = document.createElement('div');
    tmp.innerHTML = renderLiveHtml(lines[idx] ?? '', accentRef.current);
    const newDiv = tmp.firstChild as HTMLElement | null;
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

  const style: React.CSSProperties & Record<string, string> = {
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
    color: '#1c1917',
    padding: '44px 40px 140px',
    '--accent': accent,
  };

  return (
    <div
      ref={ref}
      data-testid="live-editor"
      spellCheck={false}
      onInput={onInput}
      onPaste={onPaste}
      onCompositionStart={onCompositionStart}
      onCompositionEnd={onCompositionEnd}
      suppressContentEditableWarning
      style={style}
    />
  );
}
