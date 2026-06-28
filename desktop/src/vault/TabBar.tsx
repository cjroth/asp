// The open-files tab strip. Lives INLINE in the editor's header row (left side,
// flex:1, horizontally scrollable); the save/count/font/theme cluster sits to its
// right. Presentational only — App owns which tabs are open / active / renaming.
//
// Interactions reported up to App:
//   onSelect / onClose            — click a tab / its × (× stops propagation)
//   onContext(path, e)            — right-click a tab → App shows Rename/Close/Delete
//   onReorder(from, to)           — drag a tab within the strip to reorder
//   onDropOpenPath(path)          — a FILE dragged from the tree onto the strip
//                                   (opens it as a tab; it is NOT moved)
// Inline rename (driven by App via renamingPath/renameValue) reuses App's
// commitRename — the same flow the old breadcrumb used.
import React, { useRef, useState } from 'react';
import { basename } from './format';
import { prettyName } from './prettyNames';

// dataTransfer MIME tags. The tree tags dragged files so this (separate)
// component can tell "open as a tab" drops apart from internal tab reorders.
export const TAB_DND_PATH = 'application/x-asp-path';
const TAB_DND_REORDER = 'application/x-asp-tab';

export interface TabBarProps {
  tabs: string[];
  active: string | null;
  prettyNames: boolean;
  accent: string;
  accentSoft: string;
  onSelect: (path: string) => void;
  onClose: (path: string) => void;
  onContext?: (path: string, e: React.MouseEvent) => void;
  // Double-click a tab → start an inline rename (same flow as the context-menu
  // Rename). App seeds renamingPath/renameValue in response.
  onRequestRename?: (path: string) => void;
  onReorder?: (from: number, to: number) => void;
  onDropOpenPath?: (path: string) => void;
  renamingPath?: string | null;
  renameValue?: string;
  onRenameChange?: (v: string) => void;
  onRenameKeyDown?: (e: React.KeyboardEvent, path: string) => void;
  onRenameCommit?: (path: string) => void;
}

function labelFor(path: string, pretty: boolean): string {
  const base = basename(path);
  return pretty ? prettyName(base, false).label : base;
}

// Pure helper: return a copy of `arr` with the item at `from` moved to `to`.
// Used to derive the live drag-preview order from the canonical tabs prop.
function moveItem<T>(arr: T[], from: number, to: number): T[] {
  if (from === to) return arr;
  const copy = arr.slice();
  const [it] = copy.splice(from, 1);
  copy.splice(to, 0, it);
  return copy;
}

// Live drag state: original index of the dragged tab + the index it would land
// on right now. Both are indices into the canonical `tabs` prop.
interface DragState {
  from: number;
  to: number;
}

export default function TabBar(props: TabBarProps) {
  const { tabs, active, prettyNames, accent, onSelect, onClose } = props;
  // Index of the tab currently being dragged for an internal reorder (null when
  // the drag came from the file tree instead). Kept in a ref so drop handlers
  // can read it synchronously (jsdom dataTransfer is unreliable).
  const dragIndex = useRef<number | null>(null);
  // Live preview of where the dragged tab would land. Drives a reordered render
  // so the user SEES the tab slide into place during the drag; reset on
  // drop/dragEnd. The tabs prop stays the source of truth — App owns the order.
  const [drag, setDrag] = useState<DragState | null>(null);

  // The order to actually render: the previewed order while dragging, else the
  // canonical tabs. Each tab keeps key={path}, so React moves the existing DOM
  // nodes (rather than rebuilding) and the CSS transitions animate the shift.
  const previewTabs = drag ? moveItem(tabs, drag.from, drag.to) : tabs;
  const draggedPath = drag ? tabs[drag.from] : null;

  const resetDrag = () => {
    dragIndex.current = null;
    setDrag(null);
  };

  // Read the file path off a drop that originated in the file tree.
  const openDroppedPath = (e: React.DragEvent): boolean => {
    let p = '';
    try {
      p = e.dataTransfer?.getData(TAB_DND_PATH) ?? '';
    } catch {
      /* jsdom / restricted dataTransfer */
    }
    if (p) {
      props.onDropOpenPath?.(p);
      return true;
    }
    return false;
  };

  if (tabs.length === 0) return null;
  return (
    <div
      data-testid="tab-bar"
      role="tablist"
      className="asp-scroll"
      style={{
        flex: 1,
        minWidth: 0,
        alignSelf: 'stretch',
        display: 'flex',
        alignItems: 'stretch',
        overflowX: 'auto',
        overflowY: 'hidden',
      }}
      // Allow drops anywhere on the strip (reorder-to-end or open-from-tree).
      onDragOver={(e) => {
        if (dragIndex.current != null || (props.onDropOpenPath && e.dataTransfer)) e.preventDefault();
      }}
      onDrop={(e) => {
        // Tab drops stopPropagation, so this only fires over blank strip space.
        if (dragIndex.current != null) {
          e.preventDefault();
          props.onReorder?.(dragIndex.current, tabs.length - 1);
          resetDrag();
        } else if (openDroppedPath(e)) {
          e.preventDefault();
        }
      }}
    >
      {previewTabs.map((path) => {
        // Index into the canonical tabs prop (stable regardless of the live
        // preview position) — what App's onReorder contract speaks in.
        const i = tabs.indexOf(path);
        const isActive = path === active;
        const isDragging = path === draggedPath;
        const isRenaming = props.renamingPath != null && props.renamingPath === path;
        return (
          <div
            key={path}
            role="tab"
            aria-selected={isActive}
            data-testid="tab"
            data-path={path}
            title={path}
            draggable={!isRenaming}
            onMouseDown={(e) => {
              if (e.button === 1) {
                // Middle-click closes (button 1).
                e.preventDefault();
                onClose(path);
              }
            }}
            onClick={() => {
              if (!isRenaming) onSelect(path);
            }}
            onDoubleClick={() => {
              // Ignore while this tab is already editing; single-click still
              // activates (the dblclick is preceded by the usual click→select).
              if (!isRenaming) props.onRequestRename?.(path);
            }}
            onContextMenu={(e) => props.onContext?.(path, e)}
            onDragStart={(e) => {
              if (isRenaming) return;
              dragIndex.current = i;
              setDrag({ from: i, to: i });
              if (e.dataTransfer) {
                e.dataTransfer.effectAllowed = 'move';
                try {
                  e.dataTransfer.setData(TAB_DND_REORDER, String(i));
                } catch {
                  /* jsdom */
                }
              }
            }}
            onDragEnd={() => {
              resetDrag();
            }}
            onDragOver={(e) => {
              if (dragIndex.current != null) {
                // Reordering: live-preview the dragged tab landing in this slot.
                e.preventDefault();
                if (drag == null || drag.to !== i) setDrag({ from: dragIndex.current, to: i });
              } else if (props.onDropOpenPath && e.dataTransfer) {
                // A file dragged from the tree — allow the open-as-tab drop.
                e.preventDefault();
              }
            }}
            onDrop={(e) => {
              e.stopPropagation();
              if (dragIndex.current != null) {
                e.preventDefault();
                props.onReorder?.(dragIndex.current, i);
                resetDrag();
              } else if (openDroppedPath(e)) {
                e.preventDefault();
              }
            }}
            style={{
              position: 'relative',
              display: 'flex',
              alignItems: 'center',
              gap: 7,
              flex: 'none',
              maxWidth: 220,
              padding: '0 8px 0 12px',
              cursor: 'pointer',
              borderRight: '1px solid var(--line)',
              // The active tab adopts the editor background + a top accent so it
              // reads as visually connected to the document below the header.
              background: isActive ? 'var(--bg)' : 'transparent',
              color: isActive ? 'var(--text)' : 'var(--text3)',
              fontSize: 12.5,
              fontWeight: isActive ? 600 : 500,
              boxShadow: isActive ? `inset 0 2px 0 ${accent}` : 'none',
              whiteSpace: 'nowrap',
              // Live-reorder feel: the dragged tab dims + shrinks slightly while
              // neighbours slide into their new slots (the reordered render moves
              // the keyed nodes; this transition animates the shift).
              transition: 'transform 0.16s ease, opacity 0.16s ease',
              opacity: isDragging ? 0.55 : 1,
              transform: isDragging ? 'scale(0.96)' : 'none',
            }}
          >
            {isRenaming ? (
              <input
                data-testid="tab-rename-input"
                autoFocus
                value={props.renameValue ?? ''}
                spellCheck={false}
                onClick={(e) => e.stopPropagation()}
                onChange={(e) => props.onRenameChange?.(e.target.value)}
                onKeyDown={(e) => props.onRenameKeyDown?.(e, path)}
                onBlur={() => props.onRenameCommit?.(path)}
                style={{
                  width: 130,
                  fontFamily: 'inherit',
                  fontSize: 12.5,
                  color: 'var(--text)',
                  background: 'var(--bg)',
                  border: `1px solid ${accent}`,
                  borderRadius: 4,
                  padding: '1px 5px',
                  outline: 'none',
                }}
              />
            ) : (
              <span style={{ overflow: 'hidden', textOverflow: 'ellipsis' }}>{labelFor(path, prettyNames)}</span>
            )}
            <button
              data-testid="tab-close"
              data-path={path}
              aria-label={`Close ${labelFor(path, prettyNames)}`}
              title="Close"
              onClick={(e) => {
                e.stopPropagation();
                onClose(path);
              }}
              onMouseDown={(e) => e.stopPropagation()}
              style={{
                display: 'flex',
                alignItems: 'center',
                justifyContent: 'center',
                width: 17,
                height: 17,
                flex: 'none',
                border: 'none',
                background: 'transparent',
                color: 'inherit',
                borderRadius: 4,
                cursor: 'pointer',
                padding: 0,
                fontSize: 14,
                lineHeight: 1,
                opacity: 0.7,
              }}
            >
              ×
            </button>
          </div>
        );
      })}
    </div>
  );
}
