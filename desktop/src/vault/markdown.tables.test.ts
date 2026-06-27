// Markdown tables: a wide table becomes its own horizontally-scrollable region
// (rows grouped under a single `.tbl-wrap`) without squashing cell content and
// without making the surrounding prose scroll. The wrapper nests the row divs one
// level deep, so these tests pin down that `readLive`/caret helpers still flatten
// it back to the strict 1:1 line↔node mapping the source reconstruction relies on.
import { afterEach, describe, expect, it } from 'vitest';
import { caretOffset, readLive, renderLiveHtml, setCaret } from './markdown';

const html = (s: string): HTMLDivElement => {
  const d = document.createElement('div');
  d.innerHTML = s;
  return d;
};

const TABLE = '| name | value |\n| --- | --- |\n| alpha | 1 |\n| beta | 2 |';

describe('table horizontal-scroll wrapper', () => {
  it('groups a run of table rows under a single top-level .tbl-wrap', () => {
    const d = html(renderLiveHtml(TABLE));
    // The whole table is ONE top-level child (the scroll region), not 4 rows.
    expect(d.childNodes.length).toBe(1);
    const wrap = d.firstElementChild as HTMLElement;
    expect(wrap.classList.contains('tbl-wrap')).toBe(true);
    // All 4 source lines are rows nested inside the wrapper.
    expect(wrap.querySelectorAll('.tbl-row').length).toBe(4);
    expect(wrap.querySelector('.tbl-head')).not.toBeNull();
    expect(wrap.querySelector('.tbl-sep')).not.toBeNull();
  });

  it('emits cells that size to content (no ellipsis squashing)', () => {
    // The renderer no longer flexes cells to 0 / clips them; the CSS makes the
    // wrapper scroll. Sanity-check the structural classes the CSS targets exist.
    const d = html(renderLiveHtml(TABLE));
    expect(d.querySelectorAll('.tcell').length).toBeGreaterThan(0);
    expect(d.querySelector('.tbl-wrap')).not.toBeNull();
  });

  it('round-trips the exact table source through readLive (rows nested in wrapper)', () => {
    const d = html(renderLiveHtml(TABLE));
    expect(readLive(d)).toBe(TABLE);
  });

  it('round-trips a table surrounded by prose, headings and blank lines', () => {
    const src = '# Title\n\nIntro paragraph.\n\n' + TABLE + '\n\nClosing paragraph.';
    const d = html(renderLiveHtml(src));
    expect(readLive(d)).toBe(src);
    // Exactly one wrapper, and the prose lines remain top-level siblings of it.
    expect(d.querySelectorAll('.tbl-wrap').length).toBe(1);
  });

  it('keeps two separate tables in two separate wrappers', () => {
    const src = TABLE + '\n\nbetween\n\n' + TABLE;
    const d = html(renderLiveHtml(src));
    expect(d.querySelectorAll('.tbl-wrap').length).toBe(2);
    expect(readLive(d)).toBe(src);
  });

  it('does not wrap pipe lines inside a code fence', () => {
    const src = '```\n| not | a | table |\n```';
    const d = html(renderLiveHtml(src));
    expect(d.querySelector('.tbl-wrap')).toBeNull();
    expect(readLive(d)).toBe(src);
  });

  it('leaves non-table documents structurally unchanged', () => {
    const src = 'plain line one\n\n- a list item\n> a quote';
    const d = html(renderLiveHtml(src));
    expect(d.querySelector('.tbl-wrap')).toBeNull();
    expect(d.childNodes.length).toBe(4);
    expect(readLive(d)).toBe(src);
  });
});

describe('caret round-trips through the flattened table wrapper', () => {
  let el: HTMLDivElement;
  afterEach(() => { if (el && el.parentNode) el.parentNode.removeChild(el); });
  const mount = (md: string) => {
    el = document.createElement('div');
    document.body.appendChild(el);
    el.innerHTML = renderLiveHtml(md);
    return el;
  };

  it('round-trips offsets that land inside table cells', () => {
    mount(TABLE);
    const total = TABLE.length;
    // Sample boundaries, inside-cell positions, and line boundaries across rows.
    for (const off of [0, 3, 7, 16, 17, 24, 30, total]) {
      setCaret(el, off);
      expect(caretOffset(el)).toBe(off);
    }
  });

  it('round-trips a caret across a table embedded between paragraphs', () => {
    const src = 'before\n\n' + TABLE + '\n\nafter';
    mount(src);
    for (const off of [0, 6, 8, 20, src.length - 1, src.length]) {
      setCaret(el, off);
      expect(caretOffset(el)).toBe(off);
    }
  });
});
