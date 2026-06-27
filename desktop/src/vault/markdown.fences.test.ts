// Syntax highlighting of fenced code blocks INSIDE a live markdown document. The
// language is taken from the opening fence's info string (```tsx, ```python, …)
// and each body line is run through the same per-language highlighter used for
// whole code files (`renderCodeHtml`). These tests pin: (1) the fence-info→lang
// mapping (via the exported `fenceLang`), (2) that body lines actually gain color
// spans, (3) the byte-exact readLive round-trip of a highlighted fence, (4) that
// ```mermaid stays a diagram (not code), and (5) blank/unknown fences don't crash.
import { describe, expect, it } from 'vitest';
import { fenceLang, readLive, renderLiveHtml } from './markdown';

const html = (s: string): HTMLDivElement => {
  const d = document.createElement('div');
  d.innerHTML = s;
  return d;
};
const esc = (s: string) => s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
// A token is colored `c` when the rendered HTML wraps its escaped text in that span.
const colored = (h: string, c: string, tok: string) => h.includes('color:' + c + '">' + esc(tok));

const COL = {
  cmt: 'var(--faint)',
  str: '#3a7d4d',
  num: '#b6612e',
  kw: '#8250df',
  lit: '#b6612e',
  key: '#2563eb',
  type: '#1f9aa0',
  tag: '#22863a',
  prop: '#2563eb',
};

// --- fence-info → language-key mapping -------------------------------------
describe('fenceLang', () => {
  it('maps the JS/TS family (and aliases) to "js"', () => {
    for (const w of ['ts', 'tsx', 'js', 'jsx', 'mjs', 'cjs', 'javascript', 'typescript'])
      expect(fenceLang(w)).toBe('js');
  });

  it('maps python / rust / shell / data aliases to their keys', () => {
    expect(fenceLang('py')).toBe('py');
    expect(fenceLang('python')).toBe('py');
    expect(fenceLang('rs')).toBe('rs');
    expect(fenceLang('rust')).toBe('rs');
    for (const w of ['sh', 'bash', 'zsh', 'shell', 'console']) expect(fenceLang(w)).toBe('sh');
    for (const w of ['yml', 'yaml']) expect(fenceLang(w)).toBe('yaml');
    expect(fenceLang('toml')).toBe('toml');
    expect(fenceLang('sql')).toBe('sql');
    for (const w of ['json', 'jsonc', 'json5']) expect(fenceLang(w)).toBe('json');
    for (const w of ['html', 'htm', 'xml', 'svg', 'vue']) expect(fenceLang(w)).toBe('html');
    for (const w of ['css', 'scss', 'sass', 'less']) expect(fenceLang(w)).toBe('css');
  });

  it('is case-insensitive and ignores trailing attributes', () => {
    expect(fenceLang('TSX')).toBe('js');
    expect(fenceLang('Python title="x"')).toBe('py');
  });

  it('returns "txt" for blank, missing or unknown languages', () => {
    expect(fenceLang('')).toBe('txt');
    expect(fenceLang('   ')).toBe('txt');
    expect(fenceLang('cobol')).toBe('txt');
    // @ts-expect-error — defensive: a non-string info still resolves to txt
    expect(fenceLang(undefined)).toBe('txt');
  });
});

// --- highlighting fenced bodies in a live document -------------------------
describe('renderLiveHtml — fenced code highlighting', () => {
  it('highlights a ```tsx body (keyword + type + string colored)', () => {
    const src = '```tsx\nconst x: Foo = "hi"\n```';
    const d = html(renderLiveHtml(src));
    const h = d.innerHTML;
    expect(colored(h, COL.kw, 'const')).toBe(true);
    expect(colored(h, COL.type, 'Foo')).toBe(true);
    expect(colored(h, COL.str, '"hi"')).toBe(true);
  });

  it('highlights a ```python body (keyword + literal + comment)', () => {
    const src = '```python\ndef f(): return None # hi\n```';
    const h = html(renderLiveHtml(src)).innerHTML;
    expect(colored(h, COL.kw, 'def')).toBe(true);
    expect(colored(h, COL.lit, 'None')).toBe(true);
    expect(h).toContain('font-style:italic">' + esc('# hi'));
  });

  it('highlights a ```rust body (keyword + type)', () => {
    const h = html(renderLiveHtml('```rust\npub fn make() -> Vec<i32> {}\n```')).innerHTML;
    expect(colored(h, COL.kw, 'pub')).toBe(true);
    expect(colored(h, COL.type, 'Vec')).toBe(true);
  });

  it('highlights a ```json body (string key + number)', () => {
    const h = html(renderLiveHtml('```json\n{ "n": 12 }\n```')).innerHTML;
    expect(colored(h, COL.key, '"n"')).toBe(true);
    expect(colored(h, COL.num, '12')).toBe(true);
  });

  it('highlights an ```html body (tag) and a ```css body (property)', () => {
    const htmlDoc = html(renderLiveHtml('```html\n<div>hi</div>\n```')).innerHTML;
    expect(colored(htmlDoc, COL.tag, '<div')).toBe(true);
    const cssDoc = html(renderLiveHtml('```css\na { color: red }\n```')).innerHTML;
    expect(colored(cssDoc, COL.prop, 'color')).toBe(true);
  });

  it('detects the language from the OPENING fence only (closing ``` carries none)', () => {
    // The body between the fences is highlighted; the literal ``` delimiter lines
    // round-trip verbatim and never become code tokens.
    const d = html(renderLiveHtml('```ts\nlet y = 1\n```'));
    expect(colored(d.innerHTML, COL.kw, 'let')).toBe(true);
    expect(readLive(d)).toBe('```ts\nlet y = 1\n```');
  });

  it('leaves a blank-language fence body un-highlighted (plain, no crash)', () => {
    const src = '```\nconst x = 1\n```';
    const d = html(renderLiveHtml(src));
    // No color span was emitted for the keyword — it stays plain text.
    expect(colored(d.innerHTML, COL.kw, 'const')).toBe(false);
    expect(d.textContent).toContain('const x = 1');
    expect(readLive(d)).toBe(src);
  });

  it('leaves an unknown-language fence body un-highlighted (no crash)', () => {
    const src = '```cobol\nMOVE x TO y\n```';
    const d = html(renderLiveHtml(src));
    expect(colored(d.innerHTML, COL.kw, 'MOVE')).toBe(false);
    expect(readLive(d)).toBe(src);
  });

  // --- hard invariant: byte-exact round-trip ------------------------------
  it('round-trips a highlighted multi-line fence byte-for-byte', () => {
    const src = '```ts\nconst Foo = bar("s", 1, null) // c\nlet w = 2\n```';
    const d = html(renderLiveHtml(src));
    expect(colored(d.innerHTML, COL.kw, 'const')).toBe(true);
    expect(readLive(d)).toBe(src);
  });

  it('round-trips a highlighted fence surrounded by prose', () => {
    const src = '# Title\n\nintro\n\n```python\nx = 1\n```\n\nafter';
    const d = html(renderLiveHtml(src));
    expect(colored(d.innerHTML, COL.num, '1')).toBe(true);
    expect(readLive(d)).toBe(src);
  });

  it('keeps one top-level div per source line of a highlighted fence', () => {
    const d = html(renderLiveHtml('```ts\nlet y = 1\n```'));
    // open fence, body, close fence → 3 divs, each one source line.
    expect(d.childNodes.length).toBe(3);
    ['```ts', 'let y = 1', '```'].forEach((ln, i) =>
      expect((d.childNodes[i] as HTMLElement).textContent).toBe(ln)
    );
  });

  it('escapes special characters in a highlighted body (no raw markup leaks)', () => {
    const src = '```html\n<b> & "x"\n```';
    const d = html(renderLiveHtml(src));
    // textContent (what readLive reads) is the verbatim source line.
    expect(readLive(d)).toBe(src);
  });
});

// --- mermaid stays a diagram, not code -------------------------------------
describe('renderLiveHtml — ```mermaid is a diagram, not a code language', () => {
  const SRC = '```mermaid\ngraph TD\nA --> B\n```';

  it('renders a .md-diagram preview and does NOT color the body as code', () => {
    const d = html(renderLiveHtml(SRC));
    expect(d.querySelector('.md-diagram')).not.toBeNull();
    // The diagram source is left literal — no syntax-color spans inside the body.
    expect(colored(d.innerHTML, COL.kw, 'graph')).toBe(false);
    expect(readLive(d)).toBe(SRC);
  });
});

// --- a plain prose document is completely unaffected -----------------------
describe('renderLiveHtml — prose without fences is unaffected', () => {
  it('emits no code-color spans and round-trips', () => {
    const src = '# Heading\n\nSome **bold** prose with `inline` code.\n\n- a\n- b';
    const d = html(renderLiveHtml(src));
    const h = d.innerHTML;
    for (const c of [COL.kw, COL.type, COL.tag, COL.prop])
      expect(h.includes('color:' + c + '">')).toBe(false);
    expect(readLive(d)).toBe(src);
  });
});
