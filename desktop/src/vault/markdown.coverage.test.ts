// Exhaustive branch coverage for the markdown/code renderers — many small inputs
// hitting every construct and empty-content path.
import { describe, expect, it } from 'vitest';
import { inlineMd, readLive, renderCodeHtml, renderLiveHtml } from './markdown';

const html = (s: string) => { const d = document.createElement('div'); d.innerHTML = s; return d; };

describe('inlineMd branches', () => {
  it('emphasis at line start and after a character', () => {
    expect(html(inlineMd('*em* start')).querySelector('em')!.textContent).toBe('em');
    expect(html(inlineMd('a *em*')).querySelector('em')!.textContent).toBe('em');
  });
});

describe('renderLiveHtml — all block constructs (incl. empty content)', () => {
  it('headings of every level and empty heading', () => {
    const d = html(renderLiveHtml('# h1\n## h2\n### h3\n#### h4\n# '));
    expect(d.textContent).toContain('h1');
    expect(d.textContent).toContain('h4');
  });

  it('blockquote, hr (--- and ***), and empty variants', () => {
    const src = '> quote\n> \n---\n***';
    const d = html(renderLiveHtml(src));
    expect(d.textContent).toContain('quote');
    expect(readLive(d)).toBe(src);
  });

  it('tasks (done/undone, indented) and bullets (indented) and ordered (indented)', () => {
    const src = '- [x] done\n  - [ ] sub\n- bullet\n  - nested\n1. one\n  2. two\n- ';
    const d = html(renderLiveHtml(src));
    expect(d.querySelectorAll('.cm-task').length).toBe(2);
    expect(d.querySelectorAll('.cm-ul').length).toBeGreaterThanOrEqual(2);
    expect(readLive(d)).toBe(src);
  });

  it('code fence with body line and empty body line', () => {
    const src = '```ts\nconst x = 1\n\n```';
    const d = html(renderLiveHtml(src));
    expect(d.textContent).toContain('const x = 1');
    expect(readLive(d)).toBe(src);
  });

  it('tables: header+separator+body, empty cell, single-column', () => {
    const src = '| a | b |\n| - | - |\n| 1 |  |\n| solo |';
    const d = html(renderLiveHtml(src));
    expect(d.querySelector('.tbl-head')).not.toBeNull();
    expect(d.querySelector('.tbl-sep')).not.toBeNull();
    expect(readLive(d)).toBe(src);
  });

  it('frontmatter empty value + non key:value line, in all three styles', () => {
    const src = '---\ntitle:\nplain text line\ntags: [x]\n---\nbody';
    for (const style of ['Card', 'Banner', 'Below'] as const) {
      const d = html(renderLiveHtml(src, '#3d63dd', style));
      expect(readLive(d)).toBe(src);
    }
  });
});

describe('renderCodeHtml — token branches per language', () => {
  it('javascript: comment, string, number, keyword, literal, fn call, plain ident', () => {
    const d = html(renderCodeHtml('// c\nconst y = foo(1) + true\nlet s = "hi"\nbare\n', 'js'));
    expect(d.innerHTML).toContain('font-style:italic'); // comment
    expect(d.innerHTML).toContain('#8250df'); // keyword
    expect(d.innerHTML).toContain('#3a7d4d'); // string
    expect(d.innerHTML).toContain('#b6612e'); // number / literal
    expect(d.innerHTML).toContain('#7c5cff'); // fn(
  });
  it('json: key vs string value, number, literal', () => {
    const d = html(renderCodeHtml('{\n  "k": "v",\n  "n": 1,\n  "b": true\n}', 'json'));
    expect(d.innerHTML).toContain('#2563eb'); // key
    expect(d.innerHTML).toContain('#3a7d4d'); // string value
  });
  it('yaml: key at line start and keyword literal', () => {
    const d = html(renderCodeHtml('enabled: true\nname: value', 'yaml'));
    expect(d.innerHTML).toContain('#2563eb');
  });
  it('shell uses # comments', () => {
    const d = html(renderCodeHtml('# comment\necho hi', 'sh'));
    expect(d.innerHTML).toContain('font-style:italic');
  });
  it('css and plain-text languages render without crashing', () => {
    expect(html(renderCodeHtml('a { display: flex; }', 'css')).textContent).toContain('flex');
    expect(html(renderCodeHtml('just words 42', 'txt')).textContent).toContain('words');
  });
});
