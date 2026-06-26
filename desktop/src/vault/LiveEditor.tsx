// The live WYSIWYG Markdown editor — a contentEditable surface managed
// imperatively (React never owns its children). It repaints only when
// `paintKey` changes (a new selection or a time-travel instant), so typing
// never triggers a React re-render of its content and the caret stays put.
import React, { useEffect, useRef } from 'react';
import { caretOffset, readLive, renderLiveHtml, setCaret } from './markdown';

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
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [paintKey, source, readOnly, notExist, accent]);

  // Clean up the pending re-highlight on unmount.
  useEffect(() => () => { if (rehlTimer.current) clearTimeout(rehlTimer.current); }, []);

  // Re-applying syntax highlighting rebuilds the whole document's DOM, which is
  // O(document size) — far too slow to do on every keystroke. So while typing we
  // let the browser insert characters natively (instant) and only re-highlight
  // after a short pause. `readLive` still reconstructs the exact source from the
  // DOM, so saves are correct in the meantime.
  const rehighlight = () => {
    const el = ref.current;
    if (!el || roRef.current || composing.current) return;
    const off = caretOffset(el);
    const src = readLive(el);
    el.innerHTML = renderLiveHtml(src, accentRef.current);
    if (off != null) setCaret(el, off);
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
    const off = caretOffset(el);
    const src = readLive(el);
    el.innerHTML = renderLiveHtml(src, accentRef.current);
    if (off != null) setCaret(el, off);
    changeRef.current(src);
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
