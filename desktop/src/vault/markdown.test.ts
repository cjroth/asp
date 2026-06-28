import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import {
  caretOffset,
  countLabel,
  hasFrontmatter,
  inlineMd,
  isCodeFile,
  langOf,
  readLive,
  renderCodeHtml,
  renderDoc,
  renderLiveHtml,
  setCaret,
  setTextOffsetIn,
  textOffsetIn,
  wordCountOf,
} from './markdown';

const html = (s: string): HTMLDivElement => {
  const d = document.createElement('div');
  d.innerHTML = s;
  return d;
};

describe('renderLiveHtml', () => {
  it('emits one div per source line and a <br> for blank lines', () => {
    const html = renderLiveHtml('a\n\nb');
    const div = document.createElement('div');
    div.innerHTML = html;
    expect(div.childNodes.length).toBe(3);
    expect((div.childNodes[1] as HTMLElement).innerHTML).toBe('<br>');
  });

  it('renders a heading with the hashes kept in a hidden cm-mark', () => {
    const div = document.createElement('div');
    div.innerHTML = renderLiveHtml('## Title');
    const mark = div.querySelector('.cm-mark');
    expect(mark).not.toBeNull();
    expect(mark!.textContent).toBe('## ');
    expect(div.textContent).toContain('Title');
  });

  it('renders task items with done state', () => {
    const div = document.createElement('div');
    div.innerHTML = renderLiveHtml('- [x] done\n- [ ] todo');
    const tasks = div.querySelectorAll('.cm-task');
    expect(tasks.length).toBe(2);
    expect(div.querySelectorAll('.cm-task-done').length).toBe(1);
  });

  it('renders a clickable checkbox hit-zone that carries no source text', () => {
    const div = document.createElement('div');
    const src = '- [x] done\n- [ ] todo';
    div.innerHTML = renderLiveHtml(src);
    const boxes = div.querySelectorAll('.cm-task-box');
    expect(boxes.length).toBe(2);
    boxes.forEach((b) => expect(b.textContent).toBe(''));
    // The box adds no characters: readLive (textContent per line) round-trips exact.
    expect(readLive(div)).toBe(src);
  });

  it('renders bullets and links and inline code', () => {
    const div = document.createElement('div');
    div.innerHTML = renderLiveHtml('- a [t](http://x) `c`');
    expect(div.querySelector('.cm-ul')).not.toBeNull();
    expect(div.querySelector('.cm-link')!.textContent).toBe('t');
    expect(div.querySelector('.cm-code')!.textContent).toBe('c');
  });

  it('escapes HTML in content', () => {
    const html = renderLiveHtml('<script>');
    expect(html).toContain('&lt;script&gt;');
    expect(html).not.toContain('<script>');
  });
});

describe('inlineMd', () => {
  it('keeps the literal markers inside cm-mark spans (so source round-trips)', () => {
    const div = document.createElement('div');
    div.innerHTML = inlineMd('**bold** and *em*');
    // textContent includes the hidden markers → reconstructs the source.
    expect(div.textContent).toBe('**bold** and *em*');
    expect(div.querySelector('strong')!.textContent).toBe('bold');
    expect(div.querySelector('em')!.textContent).toBe('em');
  });

  it('renders an inline image badge and round-trips the literal via readLive', () => {
    const div = document.createElement('div');
    div.innerHTML = inlineMd('![CI](https://x/ci.svg)');
    const img = div.querySelector('img.cm-img') as HTMLImageElement;
    expect(img).not.toBeNull();
    expect(img.getAttribute('src')).toBe('https://x/ci.svg');
    expect(img.getAttribute('alt')).toBe('CI');
    // A plain image is not a link → no data-href.
    expect(img.hasAttribute('data-href')).toBe(false);
    // The visible img carries no text; the literal lives in hidden marks, so the
    // line's textContent (what readLive reads) reconstructs the source exactly.
    expect(div.textContent).toBe('![CI](https://x/ci.svg)');
  });

  it('renders an image-wrapped link: clickable badge carrying the link URL, round-trips', () => {
    const src = '[![CI](https://x/badge.svg)](https://x/actions)';
    const div = document.createElement('div');
    div.innerHTML = inlineMd(src);
    const img = div.querySelector('img.cm-img') as HTMLImageElement;
    expect(img.getAttribute('src')).toBe('https://x/badge.svg');
    expect(img.getAttribute('data-href')).toBe('https://x/actions');
    // inlineMd emits inline content (not line divs); textContent IS the literal
    // source the editor reads back per line — so this is the round-trip check.
    expect(div.textContent).toBe(src);
  });

  it('makes a plain link clickable (data-href) and keeps the text + round-trip', () => {
    const src = '[docs](https://x/readme)';
    const div = document.createElement('div');
    div.innerHTML = inlineMd(src);
    const link = div.querySelector('.cm-link') as HTMLElement;
    expect(link.textContent).toBe('docs');
    expect(link.getAttribute('data-href')).toBe('https://x/readme');
    expect(div.textContent).toBe(src);
  });

  it('escapes quotes/ampersands in URLs for attribute safety and round-trips', () => {
    const src = '![a](https://x/q?a=1&b=2)';
    const div = document.createElement('div');
    div.innerHTML = inlineMd(src);
    const img = div.querySelector('img.cm-img') as HTMLImageElement;
    expect(img.getAttribute('src')).toBe('https://x/q?a=1&b=2');
    expect(div.textContent).toBe(src);
  });

  it('renders an image inside a heading (heading body passes through inlineMd)', () => {
    const div = document.createElement('div');
    div.innerHTML = renderLiveHtml('# Thunderbolt ![CI](https://x/ci.svg)');
    expect(div.querySelector('img.cm-img')).not.toBeNull();
    expect(readLive(div)).toBe('# Thunderbolt ![CI](https://x/ci.svg)');
  });
});

describe('readLive round-trips source through the rendered DOM', () => {
  const cases = ['# Hello\n\nedited', '- [x] a\n- [ ] b', 'plain `code` and **bold**', 'line1\n\n\nline4'];
  for (const src of cases) {
    it(JSON.stringify(src), () => {
      const div = document.createElement('div');
      div.innerHTML = renderLiveHtml(src);
      expect(readLive(div)).toBe(src);
    });
  }
});

describe('caret offset/setCaret round-trip', () => {
  it('restores the caret to the same flat offset after a re-render', () => {
    const div = document.createElement('div');
    document.body.appendChild(div);
    div.innerHTML = renderLiveHtml('# Title\n\nbody text');
    // Place caret 3 chars into the body line (offset = len('# Title')+1+1+3).
    setCaret(div, 12);
    const off = caretOffset(div);
    expect(off).toBe(12);
    document.body.removeChild(div);
  });
});

describe('textOffsetIn / setTextOffsetIn (single-line caret, for line-level re-highlight)', () => {
  it('round-trips a caret offset within one rendered line (incl. hidden markers)', () => {
    const div = document.createElement('div');
    document.body.appendChild(div);
    // One heading line; its rendered DOM has hidden cm-mark for "## ".
    div.innerHTML = renderLiveHtml('## Hello world');
    const line = div.firstElementChild as HTMLElement;
    // Offset 5 = after "## He" counting the hidden "## " markers too.
    setTextOffsetIn(line, 5);
    expect(textOffsetIn(line)).toBe(5);
    // End of line.
    setTextOffsetIn(line, 14);
    expect(textOffsetIn(line)).toBe(14);
    document.body.removeChild(div);
  });
});

describe('wordCountOf', () => {
  it('counts words and pluralizes', () => {
    expect(wordCountOf('')).toBe('0 words');
    expect(wordCountOf('one')).toBe('1 word');
    expect(wordCountOf('two words here')).toBe('3 words');
  });
});

const FM = '---\ntitle: My note\ntags: [a, b]\ndate: 2026-06-27\n---\n# Body';

describe('frontmatter rendering', () => {
  it('hasFrontmatter detects a leading --- … --- block', () => {
    expect(hasFrontmatter(FM)).toBe(true);
    expect(hasFrontmatter('# no frontmatter')).toBe(false);
    expect(hasFrontmatter('---\nunterminated')).toBe(false);
    expect(hasFrontmatter('')).toBe(false);
  });

  it('renders the Below style by default (fmd-* classes, array → arrow)', () => {
    const d = html(renderLiveHtml(FM));
    expect(d.querySelector('.fmd-start')).not.toBeNull();
    expect(d.querySelector('.fmd-title')).not.toBeNull();
    expect(d.querySelector('.fmd-meta')).not.toBeNull();
    expect(d.querySelector('.fmd-end')).not.toBeNull();
    expect(d.querySelector('.fmd-arr')).not.toBeNull(); // tags: [a, b]
    expect(readLive(d)).toBe(FM); // round-trips
  });

  it('renders the Banner style (fmb-* classes)', () => {
    const d = html(renderLiveHtml(FM, '#3d63dd', 'Banner'));
    expect(d.querySelector('.fmb-start')).not.toBeNull();
    expect(d.querySelector('.fmb-title')).not.toBeNull();
    expect(d.querySelector('.fmb-end')).not.toBeNull();
    expect(readLive(d)).toBe(FM);
  });

  it('renders the Card style (fm-top/fm-row/fm-bot, key/val/arr)', () => {
    const d = html(renderLiveHtml(FM, '#3d63dd', 'Card'));
    expect(d.querySelector('.fm-top')).not.toBeNull();
    expect(d.querySelector('.fm-bot')).not.toBeNull();
    expect(d.querySelector('.fm-key')).not.toBeNull();
    expect(d.querySelector('.fm-arr')).not.toBeNull();
    expect(readLive(d)).toBe(FM);
  });

  it('handles a non key:value line inside frontmatter', () => {
    const src = '---\njust text\n---\n';
    expect(html(renderLiveHtml(src, '#3d63dd', 'Card')).querySelector('.fm-line')).not.toBeNull();
    expect(html(renderLiveHtml(src, '#3d63dd', 'Below')).querySelector('.fmd-meta')).not.toBeNull();
  });
});

describe('tables, fences, quotes, rules, ordered lists', () => {
  it('renders a table with header, separator and body cells', () => {
    const d = html(renderLiveHtml('| a | b |\n| - | - |\n| 1 | 2 |'));
    expect(d.querySelector('.tbl-head')).not.toBeNull();
    expect(d.querySelector('.tbl-sep')).not.toBeNull();
    expect(d.querySelectorAll('.tbl-row').length).toBe(3);
    expect(d.querySelectorAll('.tcell').length).toBeGreaterThan(0);
  });

  it('renders code fences, blockquotes, hr and ordered lists', () => {
    const src = '```js\ncode\n```\n> quote\n---\n1. first';
    const d = html(renderLiveHtml(src));
    expect(d.textContent).toContain('code');
    expect(readLive(d)).toBe(src);
  });
});

describe('blockquote renders as one continuous accent bar', () => {
  it('emits one .cm-quote div per quote line with NO inter-line vertical margin', () => {
    const src = '> first\n> second\n> third';
    const d = html(renderLiveHtml(src));
    const quotes = d.querySelectorAll('.cm-quote');
    // One top-level div per source line (the editor's line↔node mapping).
    expect(quotes.length).toBe(3);
    quotes.forEach((q) => {
      // Styling lives in the CSS class — NO inline margin re-breaks the bar.
      const style = (q as HTMLElement).getAttribute('style');
      expect(style).toBeNull();
      // The `>` marker is preserved inside the hidden cm-mark for round-tripping.
      const mark = q.querySelector('.cm-mark');
      expect(mark).not.toBeNull();
      expect(mark!.textContent).toBe('> ');
    });
    // Source round-trips byte-exact through the rendered DOM.
    expect(readLive(d)).toBe(src);
  });

  it('renders a single-line quote (one .cm-quote, marker preserved, round-trips)', () => {
    const src = '> lonely';
    const d = html(renderLiveHtml(src));
    const quotes = d.querySelectorAll('.cm-quote');
    expect(quotes.length).toBe(1);
    expect((quotes[0] as HTMLElement).getAttribute('style')).toBeNull();
    expect(quotes[0].querySelector('.cm-mark')!.textContent).toBe('> ');
    expect(quotes[0].textContent).toBe('> lonely');
    expect(readLive(d)).toBe(src);
  });

  it('renders an empty quote line (just ">") as a .cm-quote that round-trips', () => {
    const src = '> a\n>\n> b';
    const d = html(renderLiveHtml(src));
    expect(d.querySelectorAll('.cm-quote').length).toBe(3);
    expect(readLive(d)).toBe(src);
  });

  it('leaves non-quote lines untouched (no .cm-quote on plain prose)', () => {
    const src = 'plain line\n> quote\nmore prose';
    const d = html(renderLiveHtml(src));
    expect(d.querySelectorAll('.cm-quote').length).toBe(1);
    expect(readLive(d)).toBe(src);
  });
});

describe('mermaid / diagram fences', () => {
  const SRC = '```mermaid\ngraph TD\nA --> B\n```';

  it('emits a contenteditable=false .md-diagram preview after the closing fence', () => {
    const d = html(renderLiveHtml(SRC));
    const node = d.querySelector('.md-diagram') as HTMLElement;
    expect(node).not.toBeNull();
    expect(node.getAttribute('contenteditable')).toBe('false');
    // The extracted source is exactly the fence body (between the ``` lines).
    expect(node.getAttribute('data-diagram-src')).toBe('graph TD\nA --> B');
    // The raw fence lines are still rendered as editable divs above the preview.
    expect(d.textContent).toContain('graph TD');
  });

  it('recognises the ```diagram alias', () => {
    const d = html(renderLiveHtml('```diagram\nsequenceDiagram\n```'));
    expect(d.querySelector('.md-diagram')!.getAttribute('data-diagram-src')).toBe('sequenceDiagram');
  });

  it('round-trips the source: the preview contributes zero lines/characters', () => {
    const d = html(renderLiveHtml(SRC));
    expect(readLive(d)).toBe(SRC);
  });

  it('round-trips with prose surrounding the diagram', () => {
    const src = '# Title\n\n' + SRC + '\n\nafter';
    const d = html(renderLiveHtml(src));
    expect(d.querySelector('.md-diagram')).not.toBeNull();
    expect(readLive(d)).toBe(src);
  });

  it('does NOT add a preview for a plain (non-diagram) code fence', () => {
    const d = html(renderLiveHtml('```js\ncode\n```'));
    expect(d.querySelector('.md-diagram')).toBeNull();
  });

  it('does NOT add a preview for an unterminated diagram fence', () => {
    const d = html(renderLiveHtml('```mermaid\ngraph TD'));
    expect(d.querySelector('.md-diagram')).toBeNull();
  });

  it('caret offsets skip the diagram preview (offsets land in the real source)', () => {
    const d = html(renderLiveHtml(SRC));
    document.body.appendChild(d);
    // Total source length is preserved despite the extra preview node.
    setCaret(d, SRC.length);
    expect(caretOffset(d)).toBe(SRC.length);
    // An offset inside the fence body still maps correctly across the preview.
    const mid = '```mermaid\ngraph TD'.length;
    setCaret(d, mid);
    expect(caretOffset(d)).toBe(mid);
    document.body.removeChild(d);
  });
});

describe('code files', () => {
  it('isCodeFile / langOf classify by extension', () => {
    expect(isCodeFile('a.ts')).toBe(true);
    expect(isCodeFile('a.md')).toBe(false);
    // Only markdown is NOT code; everything else (incl. unknown/no-extension) is.
    expect(isCodeFile('a.markdown')).toBe(false);
    expect(isCodeFile('A.MD')).toBe(false); // case-insensitive
    expect(isCodeFile('a.txt')).toBe(true);
    expect(isCodeFile('a.log')).toBe(true);
    expect(isCodeFile('a.py')).toBe(true);
    expect(isCodeFile('Makefile')).toBe(true); // no extension → still code/monospace
    expect(langOf('a.tsx')).toBe('js');
    expect(langOf('a.json')).toBe('json');
    expect(langOf('a.sh')).toBe('sh');
    expect(langOf('a.yaml')).toBe('yaml');
    expect(langOf('a.css')).toBe('css');
    expect(langOf('a.rs')).toBe('rs');
    expect(langOf('Makefile')).toBe('txt');
  });

  it('highlights js keywords, strings, numbers and comments (one div per line)', () => {
    const d = html(renderCodeHtml('// note\nconst f = fn(1)\nlet s = "x"', 'js'));
    expect(d.childNodes.length).toBe(3);
    expect(d.innerHTML).toContain('#8250df'); // keyword color (const/let)
    expect(d.innerHTML).toContain('#3a7d4d'); // string color
    expect(d.innerHTML).toContain('#b6612e'); // number color
    expect(d.innerHTML).toContain('font-style:italic'); // comment
    expect(d.innerHTML).toContain('#7c5cff'); // fn( call color
  });

  it('colors json keys and literals', () => {
    const d = html(renderCodeHtml('{ "key": true, "n": 12 }', 'json'));
    expect(d.innerHTML).toContain('#2563eb'); // key color (followed by :)
    expect(d.innerHTML).toContain('#b6612e'); // literal/number color
  });

  it('colors a yaml key at line start', () => {
    const d = html(renderCodeHtml('name: value', 'yaml'));
    expect(d.innerHTML).toContain('#2563eb');
  });

  it('renderDoc dispatches code vs markdown by path', () => {
    expect(renderDoc('const x = 1', 'a.ts', '#3d63dd', 'Below')).not.toContain('cm-ul');
    expect(renderDoc('- bullet', 'a.md', '#3d63dd', 'Below')).toContain('cm-ul');
  });

  it('countLabel: words for markdown, lines otherwise', () => {
    expect(countLabel('two words', 'a.md')).toBe('2 words');
    expect(countLabel('l1\nl2\nl3\n', 'a.ts')).toBe('3 lines');
    expect(countLabel('only', 'a.ts')).toBe('1 line');
    expect(countLabel('', 'a.ts')).toBe('0 lines');
  });
});

// The hidden-syntax `.cm-mark` spans must stay invisible AND keep their literal
// source text in the DOM so `readLive` round-trips byte-exact. They were switched
// from `display:none` to an IN-FLOW zero-width technique (`font-size:0`) so the
// markers no longer leave the line box — which is what disturbs the caret height
// and selection rectangles of the adjacent styled text in Chromium-based engines
// (see e2e/prose-metrics.mjs for the real-browser measurements). These tests pin
// both halves of the contract: the invisibility CSS technique, and the byte-exact
// round-trip across every prose construct that uses marks.
describe('cm-mark invisibility technique + round-trip (caret/selection fix)', () => {
  // vitest runs from the project root; the jsdom env makes import.meta.url an
  // http URL, so resolve the stylesheet from cwd instead of a file: URL.
  const cssText = readFileSync(resolve(process.cwd(), 'src/styles.css'), 'utf8');
  // The standalone `.cm-mark { … }` rule (not the more specific frontmatter ones).
  const markRule = (cssText.match(/^\.cm-mark\s*\{([^}]*)\}/m) || ['', ''])[1];

  it('keeps marks IN-FLOW and zero-width (not display:none) so the line box is intact', () => {
    expect(markRule).not.toBe('');
    // The whole point of the fix: marks are no longer pulled out of the line box.
    expect(markRule).not.toMatch(/display\s*:\s*none/);
    // Invisible + zero-footprint: no glyphs, no width.
    expect(markRule).toMatch(/font-size\s*:\s*0/);
  });

  it('marks still carry the LITERAL source syntax as their text (round-trip data)', () => {
    const div = document.createElement('div');
    div.innerHTML = renderLiveHtml('# Heading\n**bold** and `code` and [t](u)');
    const marks = [...div.querySelectorAll('.cm-mark')].map((m) => m.textContent);
    expect(marks).toContain('# '); // heading hashes + space
    expect(marks).toContain('**'); // bold fences
    expect(marks).toContain('`'); // inline-code fences
    // The link literal is split across hidden marks around the visible text.
    expect(marks.join('')).toContain('](u)');
  });

  it('round-trips byte-exact for representative prose (headings, bold, italic, code, links, lists, quotes)', () => {
    const srcs = [
      '# Title with **bold** and `code`',
      'Para with *italic*, `inline`, and a [link](https://example.com/a?b=1&c=2).',
      '- bullet with **strong** text\n- [x] a done task `here`',
      '> quoted **line** with a [ref](http://x) and `tt`',
      'plain trailing line',
    ];
    for (const src of srcs) {
      const div = document.createElement('div');
      div.innerHTML = renderLiveHtml(src);
      expect(readLive(div)).toBe(src);
    }
  });
});
