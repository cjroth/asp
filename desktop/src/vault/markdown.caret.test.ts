// Branch coverage for the contentEditable caret helpers — deep-element walks,
// line boundaries, and beyond-end fallbacks.
import { afterEach, describe, expect, it } from 'vitest';
import { caretOffset, renderLiveHtml, setCaret, setTextOffsetIn, textOffsetIn } from './markdown';

let el: HTMLDivElement;
afterEach(() => { if (el && el.parentNode) el.parentNode.removeChild(el); });
const mount = (md: string) => {
  el = document.createElement('div');
  document.body.appendChild(el);
  el.innerHTML = renderLiveHtml(md);
  return el;
};

describe('caret helpers', () => {
  it('round-trips offsets across lines and into inline elements (deep walk)', () => {
    // Heading line has a hidden cm-mark span + text → selection lands inside an
    // element child, exercising the deep tree-walk branch of caretOffset.
    mount('# Head\n\nbody');
    for (const off of [0, 2, 5, 7, 8, 11]) {
      setCaret(el, off);
      expect(caretOffset(el)).toBe(off);
    }
  });

  it('places the caret exactly on a line boundary (remaining 0)', () => {
    mount('ab\ncd');
    setCaret(el, 3); // start of the 2nd line
    expect(caretOffset(el)).toBe(3);
  });

  it('clamps a beyond-end target to the end of the document', () => {
    mount('xy\nz');
    setCaret(el, 999);
    expect(caretOffset(el)).toBe(4); // 'xy' + boundary + 'z'
  });

  it('single-line offset round-trips and falls back past the end', () => {
    mount('## Hi there');
    const line = el.firstElementChild as HTMLElement;
    setTextOffsetIn(line, 5);
    expect(textOffsetIn(line)).toBe(5);
    setTextOffsetIn(line, 999); // beyond → last text node
    expect(textOffsetIn(line)).not.toBeNull();
  });

  it('caretOffset returns null without a selection, textOffsetIn null when outside', () => {
    mount('hello');
    getSelection()!.removeAllRanges();
    expect(caretOffset(el)).toBeNull();
    expect(textOffsetIn(el.firstElementChild as HTMLElement)).toBeNull();
  });
});
