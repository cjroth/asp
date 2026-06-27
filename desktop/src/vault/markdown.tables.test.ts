// Markdown tables: a wide table becomes its own horizontally-scrollable region
// (rows grouped under a single `.tbl-scroll > .tbl-grid` nesting) so columns
// align across rows without squashing cell content and without making the
// surrounding prose scroll. A trailing `.tbl-pad` spacer gives content-width
// tables extra scroll room on the right. The wrappers nest the row divs two
// levels deep, so these tests pin down that `readLive`/caret helpers still flatten
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
  it('groups a run of table rows under a single top-level .tbl-scroll > .tbl-grid', () => {
    const d = html(renderLiveHtml(TABLE));
    // The whole table is ONE top-level child (the scroll region), not 4 rows.
    expect(d.childNodes.length).toBe(1);
    const scroll = d.firstElementChild as HTMLElement;
    expect(scroll.classList.contains('tbl-scroll')).toBe(true);
    // The rows live inside the inner `display:table` grid box (one level deeper).
    const grid = scroll.querySelector('.tbl-grid') as HTMLElement;
    expect(grid).not.toBeNull();
    // All 4 source lines are rows nested inside the grid (direct children).
    expect(grid.querySelectorAll('.tbl-row').length).toBe(4);
    expect(grid.children.length).toBe(4);
    expect(grid.querySelector('.tbl-head')).not.toBeNull();
    expect(grid.querySelector('.tbl-sep')).not.toBeNull();
  });

  it('appends a trailing .tbl-pad scroll spacer as a sibling of the grid', () => {
    const d = html(renderLiveHtml(TABLE));
    const scroll = d.firstElementChild as HTMLElement;
    const pad = scroll.querySelector('.tbl-pad') as HTMLElement;
    expect(pad).not.toBeNull();
    // The spacer is a direct child of the scroll region, NOT inside the grid, and
    // is non-editable so the caret can never land in it.
    expect(pad.parentElement).toBe(scroll);
    expect(pad.closest('.tbl-grid')).toBeNull();
    expect(pad.getAttribute('contenteditable')).toBe('false');
  });

  it('emits cells that size to content (no ellipsis squashing)', () => {
    // The renderer no longer flexes cells to 0 / clips them; the CSS makes the
    // wrapper scroll. Sanity-check the structural classes the CSS targets exist.
    const d = html(renderLiveHtml(TABLE));
    expect(d.querySelectorAll('.tcell').length).toBeGreaterThan(0);
    expect(d.querySelector('.tbl-scroll')).not.toBeNull();
    expect(d.querySelector('.tbl-grid')).not.toBeNull();
  });

  it('round-trips the exact table source through readLive (rows nested two levels deep)', () => {
    const d = html(renderLiveHtml(TABLE));
    expect(readLive(d)).toBe(TABLE);
  });

  it('round-trips a table surrounded by prose, headings and blank lines', () => {
    const src = '# Title\n\nIntro paragraph.\n\n' + TABLE + '\n\nClosing paragraph.';
    const d = html(renderLiveHtml(src));
    expect(readLive(d)).toBe(src);
    // Exactly one wrapper, and the prose lines remain top-level siblings of it.
    expect(d.querySelectorAll('.tbl-scroll').length).toBe(1);
  });

  it('keeps two separate tables in two separate wrappers', () => {
    const src = TABLE + '\n\nbetween\n\n' + TABLE;
    const d = html(renderLiveHtml(src));
    expect(d.querySelectorAll('.tbl-scroll').length).toBe(2);
    expect(d.querySelectorAll('.tbl-grid').length).toBe(2);
    expect(d.querySelectorAll('.tbl-pad').length).toBe(2);
    expect(readLive(d)).toBe(src);
  });

  it('does not wrap pipe lines inside a code fence', () => {
    const src = '```\n| not | a | table |\n```';
    const d = html(renderLiveHtml(src));
    expect(d.querySelector('.tbl-scroll')).toBeNull();
    expect(readLive(d)).toBe(src);
  });

  it('leaves non-table documents structurally unchanged', () => {
    const src = 'plain line one\n\n- a list item\n> a quote';
    const d = html(renderLiveHtml(src));
    expect(d.querySelector('.tbl-scroll')).toBeNull();
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
