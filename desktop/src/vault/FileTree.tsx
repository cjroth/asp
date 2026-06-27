// Virtualized file tree. Vaults can hold thousands of files, so rendering every
// row as a DOM node freezes the webview. This renders only the rows in (and just
// around) the viewport, with spacer divs preserving scroll height. Fixed row
// height keeps the math simple and the scrollbar accurate. Styling and behavior
// (chevron rotation, accent icon, context-target ring, hidden/pretty italics)
// match the new design; all color is theme-driven via CSS variables.
import React, { useEffect, useRef, useState } from 'react';
import { ChevronRight, FileIcon } from './icons';
import { isHidden, prettyName } from './prettyNames';
import type { FlatRow } from './tree';

const ROW_H = 29;
const OVERSCAN = 8;

export interface FileTreeProps {
  rows: FlatRow[];
  selectedPath: string | null;
  // Every file path in the current multi-selection (includes selectedPath). The
  // single-select case is just a one-element set. Optional so callers that only
  // care about the active file (and tests) can omit it.
  selectedPaths?: Set<string>;
  expanded: Record<string, boolean>;
  renaming: string | null;
  renameValue: string;
  accent: string;
  accentSoft: string;
  prettyNames: boolean;
  ctxTargetPath: string | null;
  // Move dragged paths into destDir ('' = vault root). Omitted in tests/callers
  // that don't enable drag-and-drop.
  onMove?: (srcPaths: string[], destDir: string) => void;
  onEmptyContext?: (e: React.MouseEvent) => void;
  onRowClick: (row: FlatRow, e: React.MouseEvent) => void;
  onRowContext: (e: React.MouseEvent, node: { path: string; isDir: boolean; name: string }) => void;
  onRenameChange: (v: string) => void;
  onRenameKey: (e: React.KeyboardEvent, path: string) => void;
  onRenameCommit: (path: string) => void;
}

const EMPTY_SET: ReadonlySet<string> = new Set();

export default function FileTree(props: FileTreeProps) {
  const { rows, selectedPath, expanded, renaming, renameValue, accent, accentSoft, prettyNames, ctxTargetPath } = props;
  const selectedPaths = props.selectedPaths ?? EMPTY_SET;
  const containerRef = useRef<HTMLDivElement | null>(null);
  const [scrollTop, setScrollTop] = useState(0);
  const [height, setHeight] = useState(400);

  // Drag-and-drop move state. `dragPath` is the row the user grabbed; the actual
  // source set is resolved at drop time (the whole multi-selection if the grabbed
  // row is part of it, else just that row). `dropTarget` is the folder path under
  // the cursor (null = none); `rootOver` highlights the empty area = drop-to-root.
  const dragPathRef = useRef<string | null>(null);
  const [dropTarget, setDropTarget] = useState<string | null>(null);
  const [rootOver, setRootOver] = useState(false);

  const srcPathsFor = (path: string): string[] =>
    selectedPaths.has(path) && selectedPaths.size > 1 ? Array.from(selectedPaths) : [path];

  const endDrag = () => {
    dragPathRef.current = null;
    setDropTarget(null);
    setRootOver(false);
  };
  const doMove = (destDir: string) => {
    const src = dragPathRef.current;
    if (src != null) props.onMove?.(srcPathsFor(src), destDir);
    endDrag();
  };

  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    const measure = () => setHeight(el.clientHeight || 400);
    measure();
    // ResizeObserver isn't available in every environment (e.g. jsdom); fall
    // back to a window resize listener so the tree still renders.
    if (typeof ResizeObserver !== 'undefined') {
      const ro = new ResizeObserver(measure);
      ro.observe(el);
      return () => ro.disconnect();
    }
    window.addEventListener('resize', measure);
    return () => window.removeEventListener('resize', measure);
  }, []);

  // Keep the selected file visible, but ONLY when the selection itself changes —
  // not when the row set changes (expanding/collapsing a folder must not yank the
  // scroll to the selected file). Without the first part, creating a file in a
  // huge vault selects a row that's scrolled off-screen ("nothing happened").
  const lastScrolledSel = useRef<string | null>(null);
  useEffect(() => {
    if (!selectedPath || selectedPath === lastScrolledSel.current) return;
    lastScrolledSel.current = selectedPath;
    const el = containerRef.current;
    if (!el) return;
    const idx = rows.findIndex((r) => r.node.type === 'file' && r.node.path === selectedPath);
    if (idx < 0) return;
    const rowTop = idx * ROW_H;
    const rowBottom = rowTop + ROW_H;
    const viewTop = el.scrollTop;
    const viewBottom = viewTop + (el.clientHeight || height);
    if (rowTop < viewTop) {
      el.scrollTop = rowTop;
      setScrollTop(rowTop);
    } else if (rowBottom > viewBottom) {
      const nt = rowBottom - (el.clientHeight || height);
      el.scrollTop = nt;
      setScrollTop(nt);
    }
  }, [selectedPath, rows, height]);

  const total = rows.length;
  const start = Math.max(0, Math.floor(scrollTop / ROW_H) - OVERSCAN);
  const end = Math.min(total, Math.ceil((scrollTop + height) / ROW_H) + OVERSCAN);
  const visible = rows.slice(start, end);

  return (
    <div
      ref={containerRef}
      className="asp-scroll"
      style={{
        flex: 1,
        overflowY: 'auto',
        padding: '2px 8px 12px',
        boxShadow: rootOver ? `inset 0 0 0 2px ${accent}` : 'none',
      }}
      onScroll={(e) => setScrollTop(e.currentTarget.scrollTop)}
      onContextMenu={props.onEmptyContext}
      // Empty-area / root drop. A folder row stops propagation in its own
      // dragOver, so this only fires over blank space or non-folder rows.
      onDragOver={(e) => {
        if (!props.onMove || dragPathRef.current == null) return;
        e.preventDefault();
        if (!rootOver) setRootOver(true);
        if (dropTarget !== null) setDropTarget(null);
      }}
      onDragLeave={(e) => {
        if (e.currentTarget === e.target) setRootOver(false);
      }}
      onDrop={(e) => {
        if (dragPathRef.current == null) return;
        e.preventDefault();
        doMove('');
      }}
    >
      <div style={{ height: total * ROW_H, position: 'relative' }}>
        {visible.map(({ node, depth }, i) => {
          const top = (start + i) * ROW_H;
          const isDir = node.type === 'dir';
          const isActive = !isDir && node.path === selectedPath;
          // Highlighted if it's the active file OR any member of the multi-selection.
          // Active stays "primary" (drives text color / weight / icon accent below).
          const isSelected = isActive || (!isDir && selectedPaths.has(node.path));
          const isRenaming = renaming === node.path;
          const hidden = isHidden(node.name);
          const pretty = prettyNames ? prettyName(node.name, isDir) : { label: node.name, italic: false };
          const color = hidden ? 'var(--faint2)' : isDir ? 'var(--text2)' : isActive ? 'var(--text)' : 'var(--text2)';
          const isDropTarget = isDir && node.path === dropTarget;
          return (
            <div
              key={node.path}
              className="asp-hover-row"
              draggable={!isRenaming}
              onClick={(e) => props.onRowClick({ node, depth }, e)}
              onContextMenu={(e) => props.onRowContext(e, { path: node.path, isDir, name: node.name })}
              onDragStart={(e) => {
                if (!props.onMove || isRenaming) return;
                dragPathRef.current = node.path;
                if (e.dataTransfer) {
                  e.dataTransfer.effectAllowed = 'move';
                  try { e.dataTransfer.setData('text/plain', node.path); } catch { /* jsdom */ }
                }
              }}
              onDragEnd={endDrag}
              // Folder rows are the move targets; stop propagation so the root
              // (container) handler doesn't also claim the drop.
              onDragOver={
                isDir && props.onMove
                  ? (e) => {
                      if (dragPathRef.current == null) return;
                      e.preventDefault();
                      e.stopPropagation();
                      if (dropTarget !== node.path) setDropTarget(node.path);
                      if (rootOver) setRootOver(false);
                    }
                  : undefined
              }
              onDragLeave={
                isDir && props.onMove
                  ? () => { if (dropTarget === node.path) setDropTarget(null); }
                  : undefined
              }
              onDrop={
                isDir && props.onMove
                  ? (e) => {
                      if (dragPathRef.current == null) return;
                      e.preventDefault();
                      e.stopPropagation();
                      doMove(node.path);
                    }
                  : undefined
              }
              style={{
                position: 'absolute',
                top,
                left: 0,
                right: 0,
                height: ROW_H,
                display: 'flex',
                alignItems: 'center',
                gap: 6,
                paddingRight: 8,
                paddingLeft: 7 + depth * 15,
                borderRadius: 7,
                cursor: 'pointer',
                userSelect: 'none',
                fontSize: 13.5,
                fontWeight: isDir ? 500 : isActive ? 500 : 400,
                fontStyle: hidden || pretty.italic ? 'italic' : 'normal',
                boxSizing: 'border-box',
                color,
                background: isDropTarget ? accentSoft : isSelected ? accentSoft : 'transparent',
                boxShadow: isDropTarget
                  ? `inset 0 0 0 2px ${accent}`
                  : node.path === ctxTargetPath
                    ? `inset 0 0 0 1.5px ${accent}`
                    : 'none',
              }}
            >
              <span style={{ width: 16, display: 'inline-flex', justifyContent: 'center', flex: 'none' }}>
                {isDir && (
                  <span style={{ display: 'inline-flex', color: 'var(--faint)', transition: 'transform .14s', transform: expanded[node.path] ? 'rotate(90deg)' : 'rotate(0deg)' }}>
                    <ChevronRight />
                  </span>
                )}
              </span>
              {!isDir && (
                <span style={{ display: 'inline-flex', flex: 'none', color: hidden ? 'var(--faint2)' : isActive ? accent : 'var(--faint2)' }}>
                  <FileIcon />
                </span>
              )}
              {isRenaming ? (
                <input
                  autoFocus
                  value={renameValue}
                  spellCheck={false}
                  onChange={(e) => props.onRenameChange(e.target.value)}
                  onKeyDown={(e) => props.onRenameKey(e, node.path)}
                  onBlur={() => props.onRenameCommit(node.path)}
                  onClick={(e) => e.stopPropagation()}
                  style={{ flex: 1, minWidth: 0, fontFamily: 'inherit', fontSize: 13.5, border: `1px solid ${accent}`, borderRadius: 4, padding: '1px 5px', outline: 'none', background: 'var(--bg)', color: 'var(--text)' }}
                />
              ) : (
                <span style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', flex: 1 }}>{pretty.label}</span>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}
