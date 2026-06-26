import { describe, expect, it } from 'vitest';
import { caretOffset, inlineMd, readLive, renderLiveHtml, setCaret, wordCountOf } from './markdown';

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

describe('wordCountOf', () => {
  it('counts words and pluralizes', () => {
    expect(wordCountOf('')).toBe('0 words');
    expect(wordCountOf('one')).toBe('1 word');
    expect(wordCountOf('two words here')).toBe('3 words');
  });
});
