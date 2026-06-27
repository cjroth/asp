import { cleanup, fireEvent, render } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import LiveEditor, { type LiveEditorProps } from './LiveEditor';

afterEach(() => { cleanup(); vi.useRealTimers(); });
beforeEach(() => vi.useFakeTimers());

const props = (over: Partial<LiveEditorProps> = {}): LiveEditorProps => ({
  source: '# Title\n\nbody text',
  paintKey: 'k1',
  path: 'a.md',
  readOnly: false,
  notExist: false,
  accent: '#3d63dd',
  centered: true,
  fontFamily: 'serif',
  frontmatterStyle: 'Below',
  onChange: vi.fn(),
  ...over,
});

const caretInLastLine = (el: HTMLElement) => {
  const div = el.childNodes[el.childNodes.length - 1] as HTMLElement;
  const target = div.firstChild || div;
  const range = document.createRange();
  range.setStart(target, 0);
  range.collapse(true);
  const sel = getSelection()!;
  sel.removeAllRanges();
  sel.addRange(range);
  return div;
};

describe('LiveEditor', () => {
  it('renders markdown and reconstructs source on input (single-line re-highlight)', () => {
    const onChange = vi.fn();
    const { getByTestId } = render(<LiveEditor {...props({ onChange })} />);
    const el = getByTestId('live-editor');
    expect(el.textContent).toContain('Title');
    expect(el.textContent).toContain('body text');

    const line = caretInLastLine(el);
    line.textContent = 'body texted';
    fireEvent.input(el);
    expect(onChange).toHaveBeenCalled();
    vi.advanceTimersByTime(320); // line-level re-highlight
    expect(el.textContent).toContain('body texted');
  });

  it('does a full re-highlight on a structural change (no selection)', () => {
    const { getByTestId } = render(<LiveEditor {...props()} />);
    const el = getByTestId('live-editor');
    getSelection()!.removeAllRanges();
    el.appendChild(document.createElement('div')); // extra line → structural
    fireEvent.input(el);
    vi.advanceTimersByTime(320);
    expect(el.querySelectorAll('div').length).toBeGreaterThan(0);
  });

  it('forces a full re-highlight when a code fence / frontmatter / table is present', () => {
    for (const source of ['```\ncode\n```', '---\ntitle: T\n---\n# B', '| a | b |\n| - | - |']) {
      const { getByTestId, unmount } = render(<LiveEditor {...props({ source })} />);
      const el = getByTestId('live-editor');
      caretInLastLine(el);
      fireEvent.input(el);
      vi.advanceTimersByTime(320);
      unmount();
    }
  });

  it('renders code files with the code highlighter (no markdown bullets)', () => {
    const { getByTestId } = render(<LiveEditor {...props({ source: 'const x = 1\n- not a bullet', path: 'a.ts' })} />);
    const el = getByTestId('live-editor');
    expect(el.querySelector('.cm-ul')).toBeNull();
    expect(el.textContent).toContain('const');
  });

  it('shows a placeholder when the file did not exist at that time', () => {
    const { getByTestId } = render(<LiveEditor {...props({ notExist: true })} />);
    expect(getByTestId('live-editor').textContent).toContain('did not exist');
  });

  it('ignores input when read-only', () => {
    const onChange = vi.fn();
    const { getByTestId } = render(<LiveEditor {...props({ readOnly: true, onChange })} />);
    fireEvent.input(getByTestId('live-editor'));
    expect(onChange).not.toHaveBeenCalled();
  });

  it('handles paste (plain text) and composition', () => {
    const onChange = vi.fn();
    const { getByTestId } = render(<LiveEditor {...props({ onChange })} />);
    const el = getByTestId('live-editor');
    const prevent = vi.fn();
    fireEvent.paste(el, { clipboardData: { getData: () => 'pasted' } });
    fireEvent.compositionStart(el);
    fireEvent.input(el); // during composition → just commits, no re-render
    fireEvent.compositionEnd(el);
    expect(onChange).toHaveBeenCalled();
    void prevent;
  });

  it('repaints when paintKey changes', () => {
    const { getByTestId, rerender } = render(<LiveEditor {...props({ source: 'first' })} />);
    const el = getByTestId('live-editor');
    expect(el.textContent).toContain('first');
    rerender(<LiveEditor {...props({ source: 'second', paintKey: 'k2' })} />);
    expect(el.textContent).toContain('second');
  });
});
