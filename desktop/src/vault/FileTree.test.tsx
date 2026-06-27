// FileTree virtualization + scroll behavior.
import { cleanup, fireEvent, render } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { FileEntry } from '../lib/api';
import FileTree from './FileTree';
import { buildTree, flatten } from './tree';

afterEach(cleanup);

const files = (paths: string[]): FileEntry[] => paths.map((p) => ({ path: p, file_id: p, is_dir: !p.includes('.'), merge_class: 'text' }));
const rowsFor = (paths: string[], expanded: Record<string, boolean>) => flatten(buildTree(files(paths)), expanded);
const noop = () => {};
const props = (rows: ReturnType<typeof rowsFor>, selectedPath: string, expanded: Record<string, boolean>, over: Record<string, unknown> = {}) => ({
  rows,
  selectedPath,
  expanded,
  renaming: null as string | null,
  renameValue: '',
  accent: '#000',
  accentSoft: '#0001',
  prettyNames: false,
  ctxTargetPath: null as string | null,
  onEmptyContext: noop,
  onRowClick: noop,
  onRowContext: noop,
  onRenameChange: noop,
  onRenameKey: noop,
  onRenameCommit: noop,
  ...over,
});

describe('FileTree', () => {
  it('renders only a bounded window of rows for a huge tree (virtualization)', () => {
    const paths = Array.from({ length: 2000 }, (_, i) => `note-${String(i).padStart(5, '0')}.md`);
    const { container } = render(<FileTree {...props(rowsFor(paths, {}), paths[0], {})} />);
    expect(container.querySelectorAll('.asp-hover-row').length).toBeLessThan(80);
  });

  it('does NOT scroll when only the row set changes (expand/collapse) — selection unchanged', () => {
    const paths = Array.from({ length: 60 }, (_, i) => `note-${i}.md`).concat(['zfolder/deep.md']);
    const { container, rerender } = render(<FileTree {...props(rowsFor(paths, {}), 'note-59.md', {})} />);
    const scroller = container.querySelector('.asp-scroll') as HTMLElement;
    // Simulate the user having scrolled somewhere deliberately.
    scroller.scrollTop = 200;
    // Expand a folder (rows change) WITHOUT changing the selection.
    rerender(<FileTree {...props(rowsFor(paths, { zfolder: true }), 'note-59.md', { zfolder: true })} />);
    expect(scroller.scrollTop).toBe(200); // must not jump to the selected file
    // Collapse again — still no jump.
    rerender(<FileTree {...props(rowsFor(paths, {}), 'note-59.md', {})} />);
    expect(scroller.scrollTop).toBe(200);
  });

  it('DOES scroll the newly-selected file into view when the selection changes', () => {
    const paths = Array.from({ length: 60 }, (_, i) => `note-${i}.md`);
    const { container, rerender } = render(<FileTree {...props(rowsFor(paths, {}), 'note-30.md', {})} />);
    const scroller = container.querySelector('.asp-scroll') as HTMLElement;
    Object.defineProperty(scroller, 'clientHeight', { value: 300, configurable: true });
    scroller.scrollTop = 1000;
    // Select a file far ABOVE the viewport → scrolls up (rowTop < viewTop).
    rerender(<FileTree {...props(rowsFor(paths, {}), 'note-0.md', {})} />);
    expect(scroller.scrollTop).toBeLessThan(1000);
    // Now select a file far BELOW → scrolls down (rowBottom > viewBottom).
    rerender(<FileTree {...props(rowsFor(paths, {}), 'note-59.md', {})} />);
    expect(scroller.scrollTop).toBeGreaterThan(0);
  });

  it('renders pretty/hidden/active/context-ring styling, fires row + rename handlers', () => {
    const onRowClick = vi.fn();
    const onRowContext = vi.fn();
    const onRenameChange = vi.fn();
    const onRenameKey = vi.fn();
    const onRenameCommit = vi.fn();
    const paths = ['README.md', '.hidden.md', 'notes.md'];
    const rows = rowsFor(paths, {});
    const { container, rerender } = render(
      <FileTree {...props(rows, 'notes.md', {}, { prettyNames: true, ctxTargetPath: 'README.md', onRowClick, onRowContext })} />,
    );
    expect(container.textContent).toContain('Readme'); // pretty
    expect(container.querySelector('[style*="inset 0 0 0 1.5px"]')).not.toBeNull(); // ring
    const row = container.querySelector('.asp-hover-row') as HTMLElement;
    fireEvent.click(row);
    fireEvent.contextMenu(row);
    expect(onRowClick).toHaveBeenCalled();
    expect(onRowContext).toHaveBeenCalled();

    rerender(<FileTree {...props(rows, 'notes.md', {}, { renaming: 'notes.md', renameValue: 'x', onRenameChange, onRenameKey, onRenameCommit })} />);
    const input = container.querySelector('input') as HTMLInputElement;
    expect(input).not.toBeNull();
    fireEvent.click(input);
    fireEvent.change(input, { target: { value: 'y' } });
    fireEvent.keyDown(input, { key: 'Enter' });
    fireEvent.blur(input);
    expect(onRenameChange).toHaveBeenCalled();
    expect(onRenameKey).toHaveBeenCalled();
    expect(onRenameCommit).toHaveBeenCalled();
  });

  it('highlights every file in selectedPaths and passes the mouse event to onRowClick', () => {
    const onRowClick = vi.fn();
    const paths = ['a.md', 'b.md', 'c.md'];
    const rows = rowsFor(paths, {});
    const { container } = render(
      <FileTree {...props(rows, 'a.md', {}, { selectedPaths: new Set(['a.md', 'c.md']), accentSoft: '#abcdef', onRowClick })} />,
    );
    const highlighted = (Array.from(container.querySelectorAll('.asp-hover-row')) as HTMLElement[]).filter(
      (r) => r.style.background === 'rgb(171, 205, 239)', // #abcdef
    );
    // a.md (active) + c.md (selected) highlighted; b.md is not.
    expect(highlighted.map((r) => r.textContent)).toEqual(['a.md', 'c.md']);

    // A modifier-click forwards the original MouseEvent (so App can read meta/shift).
    fireEvent.click(container.querySelector('.asp-hover-row') as HTMLElement, { metaKey: true });
    expect(onRowClick).toHaveBeenCalled();
    expect(onRowClick.mock.calls[0][1]).toBeTruthy(); // the event object
    expect(onRowClick.mock.calls[0][1].metaKey).toBe(true);
  });

  it('uses ResizeObserver when available (observe + disconnect on unmount)', () => {
    const observe = vi.fn();
    const disconnect = vi.fn();
    class RO {
      observe = observe;
      disconnect = disconnect;
      unobserve = vi.fn();
    }
    vi.stubGlobal('ResizeObserver', RO);
    const { unmount } = render(<FileTree {...props(rowsFor(['a.md'], {}), 'a.md', {})} />);
    expect(observe).toHaveBeenCalled();
    unmount();
    expect(disconnect).toHaveBeenCalled();
    vi.unstubAllGlobals();
  });

  it('falls back to a window resize listener when ResizeObserver is unavailable', () => {
    // jsdom has no ResizeObserver, so the default render path already exercises the
    // fallback; assert it tolerates a resize event without error.
    render(<FileTree {...props(rowsFor(['a.md'], {}), 'a.md', {})} />);
    expect(() => window.dispatchEvent(new Event('resize'))).not.toThrow();
  });

  // ---- drag-and-drop move ----
  const dnd = () => ({ effectAllowed: '', setData: vi.fn(), getData: vi.fn() });
  const rowOf = (container: HTMLElement, name: string) =>
    (Array.from(container.querySelectorAll('.asp-hover-row')) as HTMLElement[]).find((r) => r.textContent === name)!;

  it('drags a file onto a folder and calls onMove with that destination dir', () => {
    const onMove = vi.fn();
    const { container } = render(<FileTree {...props(rowsFor(['a.md', 'folder/b.md'], {}), 'a.md', {}, { onMove })} />);
    fireEvent.dragStart(rowOf(container, 'a.md'), { dataTransfer: dnd() });
    fireEvent.dragOver(rowOf(container, 'folder'));
    fireEvent.drop(rowOf(container, 'folder'));
    expect(onMove).toHaveBeenCalledWith(['a.md'], 'folder');
  });

  it('tags the dragged path on dataTransfer so the tab strip can open it', () => {
    const onMove = vi.fn();
    const dt = dnd();
    const { container } = render(<FileTree {...props(rowsFor(['a.md', 'folder/b.md'], {}), 'a.md', {}, { onMove })} />);
    fireEvent.dragStart(rowOf(container, 'a.md'), { dataTransfer: dt });
    expect(dt.setData).toHaveBeenCalledWith('application/x-asp-path', 'a.md');
  });

  it('toggles the drop-target highlight on dragOver / dragLeave', () => {
    const onMove = vi.fn();
    const { container } = render(<FileTree {...props(rowsFor(['a.md', 'folder/b.md'], {}), 'a.md', {}, { onMove })} />);
    fireEvent.dragStart(rowOf(container, 'a.md'), { dataTransfer: dnd() });
    expect(rowOf(container, 'folder').style.boxShadow).not.toContain('2px');
    fireEvent.dragOver(rowOf(container, 'folder'));
    expect(rowOf(container, 'folder').style.boxShadow).toContain('inset 0 0 0 2px');
    fireEvent.dragLeave(rowOf(container, 'folder'));
    expect(rowOf(container, 'folder').style.boxShadow).not.toContain('2px');
  });

  it('drops on the empty tree area to move a path to the root', () => {
    const onMove = vi.fn();
    const { container } = render(<FileTree {...props(rowsFor(['a.md', 'folder/b.md'], {}), 'a.md', {}, { onMove })} />);
    const scroll = container.querySelector('.asp-scroll') as HTMLElement;
    fireEvent.dragStart(rowOf(container, 'a.md'), { dataTransfer: dnd() });
    fireEvent.dragOver(scroll);
    expect(scroll.style.boxShadow).toContain('inset 0 0 0 2px');
    fireEvent.drop(scroll);
    expect(onMove).toHaveBeenCalledWith(['a.md'], '');
  });

  it('drags a multi-selection — moves every selected path', () => {
    const onMove = vi.fn();
    const { container } = render(
      <FileTree {...props(rowsFor(['a.md', 'c.md', 'folder/b.md'], {}), 'a.md', {}, { selectedPaths: new Set(['a.md', 'c.md']), onMove })} />,
    );
    fireEvent.dragStart(rowOf(container, 'a.md'), { dataTransfer: dnd() });
    fireEvent.drop(rowOf(container, 'folder'));
    expect(onMove).toHaveBeenCalledWith(['a.md', 'c.md'], 'folder');
  });

  it('does not start a drag while a row is being renamed', () => {
    const onMove = vi.fn();
    const { container } = render(
      <FileTree {...props(rowsFor(['a.md', 'folder/b.md'], {}), 'a.md', {}, { renaming: 'a.md', renameValue: 'a.md', onMove })} />,
    );
    const renameRow = (container.querySelector('input') as HTMLInputElement).closest('.asp-hover-row') as HTMLElement;
    fireEvent.dragStart(renameRow, { dataTransfer: dnd() });
    fireEvent.drop(rowOf(container, 'folder'));
    expect(onMove).not.toHaveBeenCalled();
  });
});
