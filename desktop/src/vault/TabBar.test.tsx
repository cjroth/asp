// Presentational tests for the tab strip: rendering, active highlight, select vs
// close (and that closing doesn't also select), middle-click close, pretty names,
// and the hidden-when-empty case.
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from '../test-shim';
import TabBar from './TabBar';

afterEach(cleanup);

const base = {
  prettyNames: false,
  accent: '#3d63dd',
  accentSoft: '#3d63dd22',
  onSelect: () => {},
  onClose: () => {},
};

describe('TabBar', () => {
  it('renders nothing when there are no tabs', () => {
    const { container } = render(<TabBar {...base} tabs={[]} active={null} />);
    expect(container.firstChild).toBeNull();
    expect(screen.queryByTestId('tab-bar')).toBeNull();
  });

  it('renders one tab per path using the basename', () => {
    render(<TabBar {...base} tabs={['README.md', 'notes/a.md']} active="README.md" />);
    const tabs = screen.getAllByTestId('tab');
    expect(tabs).toHaveLength(2);
    expect(tabs[0].textContent).toContain('README.md');
    // nested path → basename only
    expect(tabs[1].textContent).toContain('a.md');
    expect(tabs[1].textContent).not.toContain('notes/');
  });

  it('uses the no-scrollbar tab-strip class (not asp-scroll) so tabs fill the full row height', () => {
    render(<TabBar {...base} tabs={['a.md', 'b.md']} active="a.md" />);
    const strip = screen.getByTestId('tab-bar');
    // The strip must NOT use asp-scroll (which draws a 10px-tall scrollbar that
    // eats the bottom of the 48px header row); it uses the hidden-scrollbar class.
    expect(strip.classList.contains('tab-strip')).toBe(true);
    expect(strip.classList.contains('asp-scroll')).toBe(false);
    // It still stretches each tab to the full header-row height.
    expect(strip.style.alignItems).toBe('stretch');
    expect(strip.style.alignSelf).toBe('stretch');
    // …while staying horizontally scrollable (just without a visible bar).
    expect(strip.style.overflowX).toBe('auto');
    expect(strip.style.overflowY).toBe('hidden');
  });

  it('marks the active tab via aria-selected', () => {
    render(<TabBar {...base} tabs={['a.md', 'b.md']} active="b.md" />);
    const tabs = screen.getAllByTestId('tab');
    expect(tabs[0].getAttribute('aria-selected')).toBe('false');
    expect(tabs[1].getAttribute('aria-selected')).toBe('true');
  });

  it('shows pretty basenames when prettyNames is on', () => {
    render(<TabBar {...base} prettyNames tabs={['notes/my-note.md']} active="notes/my-note.md" />);
    // prettyName("my-note.md") → "My Note"
    expect(screen.getByTestId('tab').textContent).toContain('My Note');
  });

  it('calls onSelect when a tab body is clicked', () => {
    const onSelect = vi.fn();
    render(<TabBar {...base} onSelect={onSelect} tabs={['a.md', 'b.md']} active="a.md" />);
    fireEvent.click(screen.getAllByTestId('tab')[1]);
    expect(onSelect).toHaveBeenCalledWith('b.md');
  });

  it('calls onClose (and NOT onSelect) when the × is clicked', () => {
    const onSelect = vi.fn();
    const onClose = vi.fn();
    render(<TabBar {...base} onSelect={onSelect} onClose={onClose} tabs={['a.md', 'b.md']} active="a.md" />);
    fireEvent.click(screen.getAllByTestId('tab-close')[1]);
    expect(onClose).toHaveBeenCalledWith('b.md');
    expect(onSelect).not.toHaveBeenCalled();
  });

  it('closes on middle-click (mousedown button 1) without selecting', () => {
    const onSelect = vi.fn();
    const onClose = vi.fn();
    render(<TabBar {...base} onSelect={onSelect} onClose={onClose} tabs={['a.md', 'b.md']} active="a.md" />);
    fireEvent.mouseDown(screen.getAllByTestId('tab')[0], { button: 1 });
    expect(onClose).toHaveBeenCalledWith('a.md');
    expect(onSelect).not.toHaveBeenCalled();
  });

  it('does not close on a normal (left) mousedown', () => {
    const onClose = vi.fn();
    render(<TabBar {...base} onClose={onClose} tabs={['a.md']} active="a.md" />);
    fireEvent.mouseDown(screen.getAllByTestId('tab')[0], { button: 0 });
    expect(onClose).not.toHaveBeenCalled();
  });

  it('mousedown on the × stops propagation (no middle-click close from the button)', () => {
    const onClose = vi.fn();
    render(<TabBar {...base} onClose={onClose} tabs={['a.md']} active="a.md" />);
    // The close button swallows mousedown so the tab's middle-click handler can't
    // fire from it; the × only closes on a real click.
    fireEvent.mouseDown(screen.getByTestId('tab-close'), { button: 1 });
    expect(onClose).not.toHaveBeenCalled();
  });

  it('uses the FULL path as the hover title', () => {
    render(<TabBar {...base} tabs={['notes/sub/a.md']} active="notes/sub/a.md" />);
    expect(screen.getByTestId('tab').getAttribute('title')).toBe('notes/sub/a.md');
  });

  it('reports a right-click via onContext (with the path)', () => {
    const onContext = vi.fn();
    render(<TabBar {...base} onContext={onContext} tabs={['a.md', 'b.md']} active="a.md" />);
    fireEvent.contextMenu(screen.getAllByTestId('tab')[1]);
    expect(onContext).toHaveBeenCalled();
    expect(onContext.mock.calls[0][0]).toBe('b.md');
  });

  it('double-clicking a tab requests a rename (with its path)', () => {
    const onRequestRename = vi.fn();
    render(<TabBar {...base} onRequestRename={onRequestRename} tabs={['a.md', 'b.md']} active="a.md" />);
    fireEvent.doubleClick(screen.getAllByTestId('tab')[1]);
    expect(onRequestRename).toHaveBeenCalledWith('b.md');
  });

  it('does not request a rename when double-clicking the tab already being renamed', () => {
    const onRequestRename = vi.fn();
    render(<TabBar {...base} onRequestRename={onRequestRename} tabs={['a.md']} active="a.md" renamingPath="a.md" renameValue="a.md" />);
    fireEvent.doubleClick(screen.getByTestId('tab'));
    expect(onRequestRename).not.toHaveBeenCalled();
  });

  // --- @dnd-kit/sortable reordering -------------------------------------------
  // Pointer dragging needs real layout (jsdom getBoundingClientRect is 0-sized),
  // so reordering is driven deterministically through the KeyboardSensor: focus a
  // tab, Space to pick up, Arrow to move, Space to drop. We feed the sortable
  // measurement a synthetic horizontal layout via getBoundingClientRect so the
  // arrow-key coordinate getter can find the neighbouring slot. (The pure index
  // mapping is exhaustively covered in tabDnd.test.ts; a real pointer drag is
  // covered by e2e/tab-reorder-drag.mjs.)
  function layOutHorizontally() {
    // Give every rendered tab a 100px-wide slot at left = index*100. dnd-kit reads
    // these rects when a keyboard drag starts to locate the next item rightward.
    for (const t of screen.getAllByTestId('tab')) {
      const i = Number(t.getAttribute('data-path')!.match(/\d+/)?.[0] ?? 0);
      (t as HTMLElement).getBoundingClientRect = () =>
        ({ left: i * 100, right: i * 100 + 90, top: 0, bottom: 30, width: 90, height: 30, x: i * 100, y: 0, toJSON() {} }) as DOMRect;
    }
  }

  // @dnd-kit's KeyboardSensor registers its document keydown listener inside a
  // setTimeout and measures droppable rects on a rAF, so the test must yield a
  // macrotask after pick-up (and between each key) for the move/drop to register.
  const tick = () => new Promise((r) => setTimeout(r, 0));
  async function keyboardDrag(tab: HTMLElement, arrows: string[]) {
    tab.focus();
    fireEvent.keyDown(tab, { key: ' ', code: 'Space' }); // pick up
    await tick();
    for (const key of arrows) {
      fireEvent.keyDown(tab, { key, code: key });
      await tick();
    }
    fireEvent.keyDown(tab, { key: ' ', code: 'Space' }); // drop
    await tick();
  }

  it('reorders forward via the keyboard sensor (Space, ArrowRight, Space)', async () => {
    const onReorder = vi.fn();
    render(<TabBar {...base} onReorder={onReorder} tabs={['t0.md', 't1.md', 't2.md']} active="t0.md" />);
    layOutHorizontally();
    await keyboardDrag(screen.getAllByTestId('tab')[0], ['ArrowRight']);
    expect(onReorder).toHaveBeenCalledTimes(1);
    expect(onReorder).toHaveBeenCalledWith(0, 1);
  }, 15000);

  it('reorders backward via the keyboard sensor (Space, ArrowLeft, Space)', async () => {
    const onReorder = vi.fn();
    render(<TabBar {...base} onReorder={onReorder} tabs={['t0.md', 't1.md', 't2.md']} active="t2.md" />);
    layOutHorizontally();
    await keyboardDrag(screen.getAllByTestId('tab')[2], ['ArrowLeft']);
    expect(onReorder).toHaveBeenCalledTimes(1);
    expect(onReorder).toHaveBeenCalledWith(2, 1);
  }, 15000);

  it('does not fire onReorder when the keyboard drag ends where it started', async () => {
    const onReorder = vi.fn();
    render(<TabBar {...base} onReorder={onReorder} tabs={['t0.md', 't1.md', 't2.md']} active="t0.md" />);
    layOutHorizontally();
    // Pick up and immediately drop without moving → no-op (handleDragEnd → null).
    await keyboardDrag(screen.getAllByTestId('tab')[0], []);
    expect(onReorder).not.toHaveBeenCalled();
  }, 15000);

  it('opens a file dragged from the tree (x-asp-path) without reordering', () => {
    const onReorder = vi.fn();
    const onDropOpenPath = vi.fn();
    const dt = { effectAllowed: '', setData: vi.fn(), getData: (t: string) => (t === 'application/x-asp-path' ? 'notes/x.md' : '') };
    render(<TabBar {...base} onReorder={onReorder} onDropOpenPath={onDropOpenPath} tabs={['a.md']} active="a.md" />);
    // No internal dragStart → the drop carries only the tree path.
    fireEvent.drop(screen.getByTestId('tab'), { dataTransfer: dt });
    expect(onDropOpenPath).toHaveBeenCalledWith('notes/x.md');
    expect(onReorder).not.toHaveBeenCalled();
  });

  it('renders an empty rename input when no renameValue is supplied', () => {
    render(<TabBar {...base} tabs={['a.md']} active="a.md" renamingPath="a.md" />);
    expect((screen.getByTestId('tab-rename-input') as HTMLInputElement).value).toBe('');
  });

  it('swallows a dataTransfer that throws on getData (restricted clipboard)', () => {
    const onDropOpenPath = vi.fn();
    const dt = {
      effectAllowed: '',
      setData: vi.fn(),
      getData: () => {
        throw new Error('blocked');
      },
    };
    render(<TabBar {...base} onDropOpenPath={onDropOpenPath} tabs={['a.md']} active="a.md" />);
    // The drop must not throw, and with no readable path nothing opens.
    fireEvent.drop(screen.getByTestId('tab-bar'), { dataTransfer: dt });
    expect(onDropOpenPath).not.toHaveBeenCalled();
  });

  it('renders an inline rename input for the renaming tab', () => {
    const onRenameChange = vi.fn();
    render(<TabBar {...base} tabs={['a.md', 'b.md']} active="a.md" renamingPath="a.md" renameValue="a.md" onRenameChange={onRenameChange} />);
    const input = screen.getByTestId('tab-rename-input') as HTMLInputElement;
    expect(input.value).toBe('a.md');
    fireEvent.change(input, { target: { value: 'a2.md' } });
    expect(onRenameChange).toHaveBeenCalledWith('a2.md');
  });

  it('opens a tree-dragged file dropped on blank strip space', () => {
    const onDropOpenPath = vi.fn();
    const dt = { effectAllowed: '', setData: vi.fn(), getData: (t: string) => (t === 'application/x-asp-path' ? 'q.md' : '') };
    render(<TabBar {...base} onDropOpenPath={onDropOpenPath} tabs={['a.md']} active="a.md" />);
    fireEvent.dragOver(screen.getByTestId('tab-bar'), { dataTransfer: dt });
    fireEvent.drop(screen.getByTestId('tab-bar'), { dataTransfer: dt });
    expect(onDropOpenPath).toHaveBeenCalledWith('q.md');
  });

  it('ignores a drop that carries no recognised path (no open, no reorder)', () => {
    const onReorder = vi.fn();
    const onDropOpenPath = vi.fn();
    const dt = { effectAllowed: '', setData: vi.fn(), getData: () => '' };
    render(<TabBar {...base} onReorder={onReorder} onDropOpenPath={onDropOpenPath} tabs={['a.md', 'b.md']} active="a.md" />);
    fireEvent.dragOver(screen.getByTestId('tab-bar'), { dataTransfer: dt });
    fireEvent.drop(screen.getByTestId('tab-bar'), { dataTransfer: dt });
    expect(onReorder).not.toHaveBeenCalled();
    expect(onDropOpenPath).not.toHaveBeenCalled();
  });
});
