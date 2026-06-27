// FileTree virtualization + scroll behavior.
import { cleanup, render } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { FileEntry } from '../lib/api';
import FileTree from './FileTree';
import { buildTree, flatten } from './tree';

afterEach(cleanup);

const files = (paths: string[]): FileEntry[] => paths.map((p) => ({ path: p, file_id: p, is_dir: !p.includes('.'), merge_class: 'text' }));
const rowsFor = (paths: string[], expanded: Record<string, boolean>) => flatten(buildTree(files(paths)), expanded);
const noop = () => {};
const props = (rows: ReturnType<typeof rowsFor>, selectedPath: string, expanded: Record<string, boolean>) => ({
  rows,
  selectedPath,
  expanded,
  renaming: null,
  renameValue: '',
  accent: '#000',
  accentSoft: '#0001',
  onRowClick: noop,
  onRowContext: noop,
  onRenameChange: noop,
  onRenameKey: noop,
  onRenameCommit: noop,
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
    const { container, rerender } = render(<FileTree {...props(rowsFor(paths, {}), 'note-00.md', {})} />);
    const scroller = container.querySelector('.asp-scroll') as HTMLElement;
    scroller.scrollTop = 0;
    // Select a file far down → the effect should scroll to bring it into view.
    rerender(<FileTree {...props(rowsFor(paths, {}), 'note-59.md', {})} />);
    expect(scroller.scrollTop).toBeGreaterThan(0);
  });
});
