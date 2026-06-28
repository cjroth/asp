// Presentational tests for the tab strip: rendering, active highlight, select vs
// close (and that closing doesn't also select), middle-click close, pretty names,
// and the hidden-when-empty case.
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
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

  it('reorders by dragging one tab onto another (from index → to index)', () => {
    const onReorder = vi.fn();
    const dt = { effectAllowed: '', setData: vi.fn(), getData: vi.fn(() => '') };
    render(<TabBar {...base} onReorder={onReorder} tabs={['a.md', 'b.md', 'c.md']} active="a.md" />);
    const tabs = screen.getAllByTestId('tab');
    fireEvent.dragStart(tabs[0], { dataTransfer: dt });
    fireEvent.drop(tabs[2], { dataTransfer: dt });
    expect(onReorder).toHaveBeenCalledWith(0, 2);
  });

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

  it('renders an inline rename input for the renaming tab', () => {
    const onRenameChange = vi.fn();
    render(<TabBar {...base} tabs={['a.md', 'b.md']} active="a.md" renamingPath="a.md" renameValue="a.md" onRenameChange={onRenameChange} />);
    const input = screen.getByTestId('tab-rename-input') as HTMLInputElement;
    expect(input.value).toBe('a.md');
    fireEvent.change(input, { target: { value: 'a2.md' } });
    expect(onRenameChange).toHaveBeenCalledWith('a2.md');
  });

  it('dropping a dragged tab on blank strip space reorders it to the end', () => {
    const onReorder = vi.fn();
    const dt = { effectAllowed: '', setData: vi.fn(), getData: () => '' };
    render(<TabBar {...base} onReorder={onReorder} tabs={['a.md', 'b.md', 'c.md']} active="a.md" />);
    fireEvent.dragStart(screen.getAllByTestId('tab')[0], { dataTransfer: dt });
    // The strip (not a tab) receives the drop → reorder to the last slot.
    fireEvent.dragOver(screen.getByTestId('tab-bar'), { dataTransfer: dt });
    fireEvent.drop(screen.getByTestId('tab-bar'), { dataTransfer: dt });
    expect(onReorder).toHaveBeenCalledWith(0, 2);
  });

  it('opens a tree-dragged file dropped on blank strip space', () => {
    const onDropOpenPath = vi.fn();
    const dt = { effectAllowed: '', setData: vi.fn(), getData: (t: string) => (t === 'application/x-asp-path' ? 'q.md' : '') };
    render(<TabBar {...base} onDropOpenPath={onDropOpenPath} tabs={['a.md']} active="a.md" />);
    fireEvent.dragOver(screen.getByTestId('tab-bar'), { dataTransfer: dt });
    fireEvent.drop(screen.getByTestId('tab-bar'), { dataTransfer: dt });
    expect(onDropOpenPath).toHaveBeenCalledWith('q.md');
  });

  it('clears the drag state on dragEnd (a later blank drop does nothing)', () => {
    const onReorder = vi.fn();
    const dt = { effectAllowed: '', setData: vi.fn(), getData: () => '' };
    render(<TabBar {...base} onReorder={onReorder} tabs={['a.md', 'b.md']} active="a.md" />);
    const t0 = screen.getAllByTestId('tab')[0];
    fireEvent.dragStart(t0, { dataTransfer: dt });
    fireEvent.dragOver(t0, { dataTransfer: dt });
    fireEvent.dragEnd(t0);
    fireEvent.drop(screen.getByTestId('tab-bar'), { dataTransfer: dt });
    expect(onReorder).not.toHaveBeenCalled();
  });
});
