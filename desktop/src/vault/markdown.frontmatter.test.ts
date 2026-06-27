// In-place frontmatter editing: a click on a property key/value must land the
// caret at the right column (even when the browser anchors the selection to a
// styled element rather than a text node), edits must round-trip the YAML source
// byte-for-byte for every `frontmatterStyle`, and adding/removing a property line
// must reconstruct cleanly. A doc without frontmatter must be unaffected.
import { afterEach, describe, expect, it } from 'vitest';
import type { FrontmatterStyle } from './prefs';
import { caretOffset, readLive, renderLiveHtml, setCaret } from './markdown';

let el: HTMLDivElement;
afterEach(() => {
  if (el && el.parentNode) el.parentNode.removeChild(el);
});
const mount = (md: string, style: FrontmatterStyle = 'Below') => {
  el = document.createElement('div');
  document.body.appendChild(el);
  el.innerHTML = renderLiveHtml(md, '#3d63dd', style);
  return el;
};
const select = (container: Node, offset: number) => {
  const range = document.createRange();
  range.setStart(container, offset);
  range.collapse(true);
  const sel = getSelection()!;
  sel.removeAllRanges();
  sel.addRange(range);
};

const FM = '---\ntitle: My note\ntags: [a, b]\ndate: 2026-06-27\n---\n# Body';
const STYLES: FrontmatterStyle[] = ['Card', 'Banner', 'Below'];
const valSel = '.fm-val, .fmb-val, .fmd-val';

describe('frontmatter in-place editing', () => {
  it('editing a property value round-trips the source for every style', () => {
    for (const style of STYLES) {
      mount(FM, style);
      const titleVal = el.querySelector(valSel) as HTMLElement; // first value = title
      expect(titleVal.textContent).toBe('My note');
      titleVal.textContent = 'Edited title';
      // The hidden `---`/`:` markers survive, so the source round-trips exactly.
      expect(readLive(el)).toBe(FM.replace('My note', 'Edited title'));
      el.remove();
    }
  });

  it('editing a property KEY round-trips and keeps the hidden colon', () => {
    for (const style of STYLES) {
      mount(FM, style);
      const key = el.querySelector('.fm-key, .fmb-key, .fmd-key') as HTMLElement;
      // The key span holds key + hidden ':' + trailing space; replacing only the
      // key text node keeps the marker so `title:` becomes `name:` cleanly.
      (key.firstChild as Text).data = 'name';
      expect(readLive(el)).toBe(FM.replace('title:', 'name:'));
      el.remove();
    }
  });

  it('caret offset round-trips into a frontmatter key and value', () => {
    mount(FM, 'Below');
    // '---'(3) NL → 'title: My note' starts at 4. key 'title' = 4..8, value at 11..17.
    for (const off of [4, 6, 9, 11, 14, 18]) {
      setCaret(el, off);
      expect(caretOffset(el)).toBe(off);
    }
  });

  it('caret round-trips at every column of a frontmatter doc (all styles)', () => {
    for (const style of STYLES) {
      mount(FM, style);
      for (let off = 0; off <= FM.length; off++) {
        setCaret(el, off);
        expect(caretOffset(el)).toBe(off);
      }
      el.remove();
    }
  });

  it('a click between the key and value spans lands after the key, not at line start', () => {
    mount(FM, 'Below');
    const line = el.childNodes[1] as HTMLElement; // 'title: My note'
    // Browser anchors the selection to the line div with a child-index offset
    // (after the key span). Before the fix this snapped to the start of the line.
    select(line, 1);
    expect(caretOffset(el)).toBe(11); // base 4 + 'title: ' (7)
  });

  it('a click on an empty value lands at the value column', () => {
    mount('---\ntitle: My note\nempty:\n---\nbody', 'Below');
    const empty = el.querySelectorAll(valSel)[1] as HTMLElement; // <br> only
    select(empty, 0);
    // '---'(3)NL=4 + 'title: My note'(14)NL=19 + 'empty:'(6) = 25
    expect(caretOffset(el)).toBe(25);
  });

  it('a selection anchored to the end of a line element maps to the line end', () => {
    mount(FM, 'Below');
    const line = el.childNodes[1] as HTMLElement; // 'title: My note'
    select(line, line.childNodes.length); // element container, end boundary
    expect(caretOffset(el)).toBe(18); // base 4 + 'title: My note' (14)
  });

  it('a bare text-node line measures its offset directly', () => {
    el = document.createElement('div');
    document.body.appendChild(el);
    el.appendChild(document.createTextNode('abc'));
    select(el.firstChild as Node, 2);
    expect(caretOffset(el)).toBe(2);
  });

  it('adding a property line round-trips through render + readLive', () => {
    const added = FM.replace('date: 2026-06-27\n', 'date: 2026-06-27\nstatus: draft\n');
    mount(added, 'Card');
    expect(readLive(el)).toBe(added);
  });

  it('removing a property line (DOM mutation) round-trips', () => {
    mount(FM, 'Below');
    const lines = readLive(el).split('\n');
    const tagsIdx = lines.indexOf('tags: [a, b]');
    el.childNodes[tagsIdx].remove(); // user deletes the whole property line
    expect(readLive(el)).toBe(FM.replace('tags: [a, b]\n', ''));
  });

  it('leaves a doc without frontmatter unaffected', () => {
    mount('# Heading\n\nbody text', 'Below');
    expect(readLive(el)).toBe('# Heading\n\nbody text');
    for (const off of [0, 2, 9, 10, 11, 16]) {
      setCaret(el, off);
      expect(caretOffset(el)).toBe(off);
    }
  });
});
