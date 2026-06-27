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
  expanded: Record<string, boolean>;
  renaming: string | null;
  renameValue: string;
  accent: string;
  accentSoft: string;
  prettyNames: boolean;
  ctxTargetPath: string | null;
  onEmptyContext?: (e: React.MouseEvent) => void;
  onRowClick: (row: FlatRow) => void;
  onRowContext: (e: React.MouseEvent, node: { path: string; isDir: boolean; name: string }) => void;
  onRenameChange: (v: string) => void;
  onRenameKey: (e: React.KeyboardEvent, path: string) => void;
  onRenameCommit: (path: string) => void;
}

export default function FileTree(props: FileTreeProps) {
  const { rows, selectedPath, expanded, renaming, renameValue, accent, accentSoft, prettyNames, ctxTargetPath } = props;
  const containerRef = useRef<HTMLDivElement | null>(null);
  const [scrollTop, setScrollTop] = useState(0);
  const [height, setHeight] = useState(400);

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
      style={{ flex: 1, overflowY: 'auto', padding: '2px 8px 12px' }}
      onScroll={(e) => setScrollTop(e.currentTarget.scrollTop)}
      onContextMenu={props.onEmptyContext}
    >
      <div style={{ height: total * ROW_H, position: 'relative' }}>
        {visible.map(({ node, depth }, i) => {
          const top = (start + i) * ROW_H;
          const isDir = node.type === 'dir';
          const isActive = !isDir && node.path === selectedPath;
          const isRenaming = renaming === node.path;
          const hidden = isHidden(node.name);
          const pretty = prettyNames ? prettyName(node.name, isDir) : { label: node.name, italic: false };
          const color = hidden ? 'var(--faint2)' : isDir ? 'var(--text2)' : isActive ? 'var(--text)' : 'var(--text2)';
          return (
            <div
              key={node.path}
              className="asp-hover-row"
              onClick={() => props.onRowClick({ node, depth })}
              onContextMenu={(e) => props.onRowContext(e, { path: node.path, isDir, name: node.name })}
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
                background: isActive ? accentSoft : 'transparent',
                boxShadow: node.path === ctxTargetPath ? `inset 0 0 0 1.5px ${accent}` : 'none',
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
