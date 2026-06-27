import { describe, expect, it } from 'vitest';
import { isCodeFile, langOf, renderCodeHtml } from './markdown';

// --- helpers ---------------------------------------------------------------
const html = (s: string): HTMLDivElement => {
  const d = document.createElement('div');
  d.innerHTML = s;
  return d;
};
const esc = (s: string) => s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
// A token is colored `c` when the rendered HTML wraps its escaped text in that color span.
const colored = (h: string, c: string, tok: string) => h.includes('color:' + c + '">' + esc(tok));
const commented = (h: string, text: string) => h.includes('font-style:italic">' + esc(text));

const COL = {
  cmt: 'var(--faint)',
  str: '#3a7d4d',
  num: '#b6612e',
  kw: '#8250df',
  lit: '#b6612e',
  key: '#2563eb',
  fn: '#7c5cff',
  type: '#1f9aa0',
  tag: '#22863a',
  attr: '#6f42c1',
  sel: '#0a8f5b',
  prop: '#2563eb',
};

// Hard invariant: one div per line + each div's textContent === source line.
const roundtrip = (src: string, lang: string) => {
  const d = html(renderCodeHtml(src, lang));
  const lines = src.split('\n');
  expect(d.childNodes.length).toBe(lines.length);
  lines.forEach((ln, i) => expect((d.childNodes[i] as HTMLElement).textContent).toBe(ln));
};

// --- structural invariants -------------------------------------------------
describe('renderCodeHtml structural invariants', () => {
  it('emits exactly one div per line and <br> for blank lines', () => {
    const d = html(renderCodeHtml('const a = 1\n\nlet b = 2', 'js'));
    expect(d.childNodes.length).toBe(3);
    expect((d.childNodes[1] as HTMLElement).innerHTML).toBe('<br>');
  });

  it('round-trips textContent for every supported language', () => {
    roundtrip('const Foo = bar("s", 1, null) // c\n/* b */ x', 'js');
    roundtrip('{ "key": "val", "n": 12, "ok": true }', 'json');
    roundtrip('name: value # note\nflag: true', 'yaml');
    roundtrip('def greet(): return None # hi\nx: int = 0', 'py');
    roundtrip('pub fn make() -> Vec<i32> { true } // c\n/* b */ let w = Widget', 'rs');
    roundtrip('if test "x"; then echo hi; fi # c', 'sh');
    roundtrip('name = "v" # c\nflag = true', 'toml');
    roundtrip("SELECT * FROM t WHERE id = 1 -- c\n/* b */ x = 'a'", 'sql');
    roundtrip('<div class="a">Hi &amp; bye</div>\n<!-- c -->\n<br/>', 'html');
    roundtrip('.foo, #bar a:hover { color: #fff; }\n@media screen { display: flex !important; }\n/* c */ content: "x"; margin: 10px', 'css');
    roundtrip('plain "s" 12 stuff', 'go');
  });

  it('normalises CRLF line endings to one div per logical line', () => {
    expect(html(renderCodeHtml('a\r\nb', 'js')).childNodes.length).toBe(2);
  });
});

// --- JavaScript / TypeScript ----------------------------------------------
describe('js/ts highlighting', () => {
  const h = renderCodeHtml('const Foo = bar("s", 1, null) // c\n/* blk */ let y', 'js');
  it('keywords, types, fn calls, strings, numbers, literals, comments', () => {
    expect(colored(h, COL.kw, 'const')).toBe(true);
    expect(colored(h, COL.kw, 'let')).toBe(true);
    expect(colored(h, COL.type, 'Foo')).toBe(true); // Capitalized → type (upType)
    expect(colored(h, COL.fn, 'bar')).toBe(true);
    expect(colored(h, COL.str, '"s"')).toBe(true);
    expect(colored(h, COL.num, '1')).toBe(true);
    expect(colored(h, COL.lit, 'null')).toBe(true);
    expect(commented(h, '// c')).toBe(true);
    expect(commented(h, '/* blk */')).toBe(true);
  });
  it('leaves a bare identifier uncolored', () => {
    expect(colored(renderCodeHtml('let y', 'js'), COL.type, 'y')).toBe(false);
  });
});

// --- JSON ------------------------------------------------------------------
describe('json highlighting', () => {
  const h = renderCodeHtml('{ "key": "val", "n": 12, "ok": true }', 'json');
  it('distinguishes string keys from string values and literals', () => {
    expect(colored(h, COL.key, '"key"')).toBe(true); // followed by ':'
    expect(colored(h, COL.str, '"val"')).toBe(true);
    expect(colored(h, COL.num, '12')).toBe(true);
    expect(colored(h, COL.lit, 'true')).toBe(true);
  });
});

// --- YAML ------------------------------------------------------------------
describe('yaml highlighting', () => {
  const h = renderCodeHtml('name: value # note\nflag: true', 'yaml');
  it('colors keys at line start, literals and comments', () => {
    expect(colored(h, COL.key, 'name')).toBe(true);
    expect(colored(h, COL.kw, 'true')).toBe(true); // also a yaml keyword
    expect(commented(h, '# note')).toBe(true);
  });
});

// --- Python ----------------------------------------------------------------
describe('python highlighting', () => {
  const h = renderCodeHtml('def greet(): return None # hi\nx: int = 0', 'py');
  it('keywords, fn defs, builtins-as-types, literals, # comments', () => {
    expect(colored(h, COL.kw, 'def')).toBe(true);
    expect(colored(h, COL.kw, 'return')).toBe(true);
    expect(colored(h, COL.fn, 'greet')).toBe(true);
    expect(colored(h, COL.lit, 'None')).toBe(true);
    expect(colored(h, COL.type, 'int')).toBe(true);
    expect(commented(h, '# hi')).toBe(true);
  });
});

// --- Rust ------------------------------------------------------------------
describe('rust highlighting', () => {
  const h = renderCodeHtml('pub fn make() -> Vec<i32> { true } // c\n/* b */ let w = Widget', 'rs');
  it('keywords, builtin + user types, literals, both comment styles', () => {
    expect(colored(h, COL.kw, 'pub')).toBe(true);
    expect(colored(h, COL.kw, 'fn')).toBe(true);
    expect(colored(h, COL.fn, 'make')).toBe(true);
    expect(colored(h, COL.type, 'Vec')).toBe(true); // in types set
    expect(colored(h, COL.type, 'i32')).toBe(true); // in types set
    expect(colored(h, COL.type, 'Widget')).toBe(true); // Capitalized → upType
    expect(colored(h, COL.lit, 'true')).toBe(true);
    expect(commented(h, '// c')).toBe(true);
    expect(commented(h, '/* b */')).toBe(true);
  });
});

// --- Shell -----------------------------------------------------------------
describe('shell highlighting', () => {
  const h = renderCodeHtml('if test "x"; then echo hi; fi # c', 'sh');
  it('keywords, strings and # comments', () => {
    expect(colored(h, COL.kw, 'if')).toBe(true);
    expect(colored(h, COL.kw, 'then')).toBe(true);
    expect(colored(h, COL.kw, 'echo')).toBe(true);
    expect(colored(h, COL.str, '"x"')).toBe(true);
    expect(commented(h, '# c')).toBe(true);
  });
});

// --- TOML ------------------------------------------------------------------
describe('toml highlighting', () => {
  const h = renderCodeHtml('name = "v" # c\nflag = true', 'toml');
  it('colors keys before =, strings, literals and comments', () => {
    expect(colored(h, COL.key, 'name')).toBe(true);
    expect(colored(h, COL.str, '"v"')).toBe(true);
    expect(colored(h, COL.lit, 'true')).toBe(true);
    expect(commented(h, '# c')).toBe(true);
  });
});

// --- SQL -------------------------------------------------------------------
describe('sql highlighting', () => {
  const h = renderCodeHtml("SELECT id FROM t WHERE id = 1 -- c\n/* b */ x = 'a'", 'sql');
  it('case-insensitive keywords, numbers, strings, line + block comments', () => {
    expect(colored(h, COL.kw, 'SELECT')).toBe(true); // ci: matched despite uppercase
    expect(colored(h, COL.kw, 'FROM')).toBe(true);
    expect(colored(h, COL.kw, 'WHERE')).toBe(true);
    expect(colored(h, COL.num, '1')).toBe(true);
    expect(colored(h, COL.str, "'a'")).toBe(true);
    expect(commented(h, '-- c')).toBe(true);
    expect(commented(h, '/* b */')).toBe(true);
  });
});

// --- HTML ------------------------------------------------------------------
describe('html highlighting', () => {
  const h = renderCodeHtml('<div class="a">Hi &amp; bye</div>\n<!-- c -->\n<br/>', 'html');
  it('tags, attributes, strings, entities, text and comments', () => {
    expect(colored(h, COL.tag, '<div')).toBe(true);
    expect(colored(h, COL.tag, '</div')).toBe(true);
    expect(colored(h, COL.tag, '>')).toBe(true);
    expect(colored(h, COL.tag, '/>')).toBe(true); // self-closing
    expect(colored(h, COL.attr, 'class')).toBe(true); // inside a tag
    expect(colored(h, COL.str, '"a"')).toBe(true);
    expect(colored(h, COL.lit, '&amp;')).toBe(true); // entity
    expect(commented(h, '<!-- c -->')).toBe(true);
    // "Hi" is text content (not inside a tag) → uncolored.
    expect(colored(h, COL.attr, 'Hi')).toBe(false);
  });
  it('colors an unterminated comment to end of line', () => {
    expect(commented(renderCodeHtml('<!-- open', 'html'), '<!-- open')).toBe(true);
  });
});

// --- CSS -------------------------------------------------------------------
describe('css highlighting', () => {
  const h = renderCodeHtml('.foo, #bar a:hover { color: #fff; }\n@media screen { display: flex !important; }\n/* c */ content: "x"; margin: 10px', 'css');
  it('selectors, properties, values, colors, units, at-rules, comments', () => {
    expect(colored(h, COL.sel, '.foo')).toBe(true);
    expect(colored(h, COL.sel, '#bar')).toBe(true);
    expect(colored(h, COL.prop, 'color')).toBe(true); // first token + ':'
    expect(colored(h, COL.num, '#fff')).toBe(true); // hex colour
    expect(colored(h, COL.num, '10px')).toBe(true); // number + unit
    expect(colored(h, COL.kw, '@media')).toBe(true); // at-rule
    expect(colored(h, COL.kw, 'flex')).toBe(true); // value keyword
    expect(colored(h, COL.kw, '!important')).toBe(true);
    expect(colored(h, COL.str, '"x"')).toBe(true);
    expect(commented(h, '/* c */')).toBe(true);
  });
  it('does not treat a pseudo-class as a property and leaves plain values bare', () => {
    // "a:hover" — a is not first token, so it is not a property; "hover" plain.
    expect(colored(h, COL.prop, 'a')).toBe(false);
    expect(colored(h, COL.prop, 'hover')).toBe(false);
    // "screen" is a plain value identifier (not in the value-keyword set).
    expect(colored(h, COL.kw, 'screen')).toBe(false);
  });
  it('colors an unterminated block comment to end of line', () => {
    expect(commented(renderCodeHtml('/* open', 'css'), '/* open')).toBe(true);
  });
});

// --- fallback / misc -------------------------------------------------------
describe('fallback languages', () => {
  it('unknown languages fall back to the txt config (strings + numbers only)', () => {
    const h = renderCodeHtml('plain "s" 12 stuff', 'go');
    expect(colored(h, COL.str, '"s"')).toBe(true);
    expect(colored(h, COL.num, '12')).toBe(true);
    expect(colored(h, COL.kw, 'plain')).toBe(false); // no keywords in txt
  });
  it('langOf/isCodeFile classify the new extensions', () => {
    expect(langOf('a.py')).toBe('py');
    expect(langOf('a.rs')).toBe('rs');
    expect(langOf('a.html')).toBe('html');
    expect(langOf('a.toml')).toBe('toml');
    expect(langOf('a.sql')).toBe('sql');
    expect(isCodeFile('a.py')).toBe(true);
    expect(isCodeFile('a.rs')).toBe(true);
    expect(isCodeFile('a.html')).toBe(true);
    expect(isCodeFile('a.toml')).toBe(true);
    expect(isCodeFile('a.sql')).toBe(true);
  });
});
