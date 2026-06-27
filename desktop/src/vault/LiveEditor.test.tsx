import { cleanup, fireEvent, render } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { _clearDiagramCache } from './diagram';
import LiveEditor, { type LiveEditorProps } from './LiveEditor';
import { caretOffset, readLive } from './markdown';

// Mock the browser-only mermaid loader so the diagram render path is deterministic
// in jsdom (the real library needs a real DOM). `loadMermaid` resolves to a fake
// whose render() returns a marker SVG.
const { mockRender } = vi.hoisted(() => ({
  mockRender: vi.fn(async (_id: string, src: string) => ({ svg: '<svg class="rendered">' + src + '</svg>' })),
}));
vi.mock('./mermaid', () => ({
  loadMermaid: async () => ({ initialize: vi.fn(), render: mockRender }),
}));

afterEach(() => { cleanup(); vi.useRealTimers(); _clearDiagramCache(); mockRender.mockClear(); });
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

  it('edits a frontmatter value in place: persists the source and keeps the caret put', () => {
    const onChange = vi.fn();
    const source = '---\ntitle: My note\ntags: [a, b]\n---\n# Body';
    const { getByTestId } = render(<LiveEditor {...props({ source, onChange, frontmatterStyle: 'Below' })} />);
    const el = getByTestId('live-editor');
    const titleVal = el.querySelector('.fmd-val') as HTMLElement; // 'My note'
    const text = titleVal.firstChild as Text;
    text.data = 'My notes'; // natural in-place edit of the value
    const range = document.createRange();
    range.setStart(text, text.data.length); // caret at the end of the value
    range.collapse(true);
    const sel = getSelection()!;
    sel.removeAllRanges();
    sel.addRange(range);
    const before = caretOffset(el); // 4 + 'title: My notes'.length = 19
    fireEvent.input(el);
    const updated = '---\ntitle: My notes\ntags: [a, b]\n---\n# Body';
    expect(onChange).toHaveBeenLastCalledWith(updated);
    vi.advanceTimersByTime(320); // frontmatter ⇒ full re-highlight + caret restore
    expect(readLive(el)).toBe(updated);
    expect(caretOffset(el)).toBe(before); // caret stayed in the title value, no field jump
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

  it('toggles a task checkbox on click: unchecked → checked, persists + re-renders', () => {
    const onChange = vi.fn();
    const { getByTestId } = render(<LiveEditor {...props({ source: '- [ ] todo', onChange })} />);
    const el = getByTestId('live-editor');
    const lineDiv = el.querySelector('.cm-task') as HTMLElement;
    expect(lineDiv.classList.contains('cm-task-done')).toBe(false);
    const box = lineDiv.querySelector('.cm-task-box') as HTMLElement;

    const ev = fireEvent.mouseDown(box);
    expect(ev).toBe(false); // preventDefault() was called → no caret lands in the line
    expect(onChange).toHaveBeenCalledWith('- [x] todo');
    // The line re-rendered into a done task, and the source still round-trips.
    expect(el.querySelector('.cm-task-done')).not.toBeNull();
    expect(readLive(el)).toBe('- [x] todo');
  });

  it('toggles a checked task back to unchecked', () => {
    const onChange = vi.fn();
    const { getByTestId } = render(<LiveEditor {...props({ source: '- [x] done', onChange })} />);
    const el = getByTestId('live-editor');
    const box = el.querySelector('.cm-task-box') as HTMLElement;
    fireEvent.mouseDown(box);
    expect(onChange).toHaveBeenCalledWith('- [ ] done');
    expect(el.querySelector('.cm-task-done')).toBeNull();
    expect(readLive(el)).toBe('- [ ] done');
  });

  it('toggles the correct line when multiple tasks exist (round-trip exact)', () => {
    const onChange = vi.fn();
    const source = '- [ ] one\n- [ ] two\n- [x] three';
    const { getByTestId } = render(<LiveEditor {...props({ source, onChange })} />);
    const el = getByTestId('live-editor');
    const boxes = el.querySelectorAll('.cm-task-box');
    fireEvent.mouseDown(boxes[1] as HTMLElement); // the middle task
    expect(onChange).toHaveBeenCalledWith('- [ ] one\n- [x] two\n- [x] three');
    expect(readLive(el)).toBe('- [ ] one\n- [x] two\n- [x] three');
  });

  it('does not toggle when read-only', () => {
    const onChange = vi.fn();
    const { getByTestId } = render(<LiveEditor {...props({ source: '- [ ] todo', readOnly: true, onChange })} />);
    const el = getByTestId('live-editor');
    const box = el.querySelector('.cm-task-box') as HTMLElement;
    fireEvent.mouseDown(box);
    expect(onChange).not.toHaveBeenCalled();
    expect(readLive(el)).toBe('- [ ] todo');
  });

  it('does not toggle when clicking the task text (not the checkbox)', () => {
    const onChange = vi.fn();
    const { getByTestId } = render(<LiveEditor {...props({ source: '- [ ] todo', onChange })} />);
    const el = getByTestId('live-editor');
    const body = el.querySelector('.cm-body') as HTMLElement;
    const ev = fireEvent.mouseDown(body);
    expect(ev).toBe(true); // not prevented → normal caret placement
    expect(onChange).not.toHaveBeenCalled();
    expect(readLive(el)).toBe('- [ ] todo');
  });

  it('ignores mousedown outside any task line', () => {
    const onChange = vi.fn();
    const { getByTestId } = render(<LiveEditor {...props({ source: 'plain text', onChange })} />);
    const el = getByTestId('live-editor');
    fireEvent.mouseDown(el.firstChild as HTMLElement);
    expect(onChange).not.toHaveBeenCalled();
  });

  it('renders a mermaid fence: code fallback first, then injects the SVG preview async', async () => {
    const src = '```mermaid\ngraph TD\nA --> B\n```';
    const { getByTestId } = render(<LiveEditor {...props({ source: src })} />);
    const el = getByTestId('live-editor');
    const node = el.querySelector('.md-diagram') as HTMLElement;
    expect(node).not.toBeNull();
    // Before the debounced render fires, the code fallback is shown (graceful).
    expect(node.querySelector('.md-diagram-fallback')).not.toBeNull();
    expect(node.querySelector('svg')).toBeNull();
    // The editable fence source still round-trips (preview is skipped).
    expect(readLive(el)).toBe(src);
    await vi.advanceTimersByTimeAsync(200);
    expect(el.querySelector('.md-diagram svg')).not.toBeNull();
    expect(mockRender).toHaveBeenCalledWith(expect.any(String), 'graph TD\nA --> B');
    expect(readLive(el)).toBe(src); // still exact after the SVG is injected
  });

  it('replays the cached SVG synchronously on a re-render (no flicker, no re-parse)', async () => {
    const src = '```mermaid\ngraph TD\n```';
    const { getByTestId } = render(<LiveEditor {...props({ source: src })} />);
    const el = getByTestId('live-editor');
    await vi.advanceTimersByTimeAsync(200);
    expect(mockRender).toHaveBeenCalledTimes(1);
    // A full re-highlight (triggered by editing inside a fence) rebuilds the DOM;
    // the cached SVG is replayed synchronously without calling mermaid again.
    caretInLastLine(el);
    fireEvent.input(el);
    vi.advanceTimersByTime(320); // full re-highlight (fence ⇒ context-dependent)
    expect(el.querySelector('.md-diagram svg')).not.toBeNull();
    await vi.advanceTimersByTimeAsync(200);
    expect(mockRender).toHaveBeenCalledTimes(1); // served from cache
  });

  it('does not create a diagram preview or invoke mermaid for a plain code fence', async () => {
    const { getByTestId } = render(<LiveEditor {...props({ source: '```js\ncode\n```' })} />);
    const el = getByTestId('live-editor');
    expect(el.querySelector('.md-diagram')).toBeNull();
    await vi.advanceTimersByTimeAsync(200);
    expect(mockRender).not.toHaveBeenCalled();
  });

  it('repaints when paintKey changes', () => {
    const { getByTestId, rerender } = render(<LiveEditor {...props({ source: 'first' })} />);
    const el = getByTestId('live-editor');
    expect(el.textContent).toContain('first');
    rerender(<LiveEditor {...props({ source: 'second', paintKey: 'k2' })} />);
    expect(el.textContent).toContain('second');
  });
});
