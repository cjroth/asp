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
//
// Two DISTINCT drag systems coexist here, on purpose:
//   1. REORDER — @dnd-kit/sortable (POINTER events). Smooth animated reordering
//      of tabs within the strip; commits via onReorder on drag end. Works in the
//      Tauri WebKit/WebView2 webview.
//   2. EXTERNAL OPEN — NATIVE HTML5 drag (`onDragOver`/`onDrop` + TAB_DND_PATH).
//      A file dragged out of the tree is dropped on the strip to open it. @dnd-kit
//      uses pointer events, native DnD uses drag events, so the two never collide
//      — the tabs are NOT `draggable`, so dragging a tab never starts native DnD,
//      and dragging a file never triggers @dnd-kit (no pointerdown on a tab).
import React from 'react';
import {
  DndContext,
  KeyboardSensor,
  PointerSensor,
  closestCenter,
  useSensor,
  useSensors,
  type DragEndEvent,
} from '@dnd-kit/core';
import { restrictToHorizontalAxis } from '@dnd-kit/modifiers';
import {
  SortableContext,
  horizontalListSortingStrategy,
  sortableKeyboardCoordinates,
  useSortable,
} from '@dnd-kit/sortable';
import { CSS } from '@dnd-kit/utilities';
import { basename } from './format';
import { prettyName } from './prettyNames';
import { reorderFromDragEnd } from './tabDnd';

// dataTransfer MIME tag the tree stamps on a dragged file so this (separate)
// component can recognise an "open as a tab" native drop.
export const TAB_DND_PATH = 'application/x-asp-path';

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

// Props handed to each sortable tab. Kept flat (not the whole TabBarProps) so the
// child only depends on what it actually renders/reports.
interface SortableTabProps {
  path: string;
  isActive: boolean;
  isRenaming: boolean;
  prettyNames: boolean;
  accent: string;
  onSelect: (path: string) => void;
  onClose: (path: string) => void;
  onContext?: (path: string, e: React.MouseEvent) => void;
  onRequestRename?: (path: string) => void;
  renameValue?: string;
  onRenameChange?: (v: string) => void;
  onRenameKeyDown?: (e: React.KeyboardEvent, path: string) => void;
  onRenameCommit?: (path: string) => void;
}

function SortableTab(p: SortableTabProps) {
  const { path, isActive, isRenaming, prettyNames, accent, onSelect, onClose } = p;
  // useSortable wires this node into the DndContext: setNodeRef registers it as a
  // sortable item, `attributes`/`listeners` carry the pointer + keyboard activator
  // (we disable them while renaming so the text input owns all input), and
  // transform/transition drive the neighbour-slide + dragged-tab-follows-pointer
  // animation. `id` is the path (stable) — what onReorder's index math resolves.
  const { attributes, listeners, setNodeRef, transform, transition, isDragging } = useSortable({
    id: path,
    disabled: isRenaming,
  });
  return (
    <div
      ref={setNodeRef}
      {...attributes}
      {...listeners}
      // Our semantics win over @dnd-kit's defaults: this is a selectable tab, not
      // a generic button. (Listed AFTER the spreads so JSX's later props override.)
      role="tab"
      aria-selected={isActive}
      data-testid="tab"
      data-path={path}
      title={path}
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
        // Ignore while this tab is already editing; single-click still activates
        // (the dblclick is preceded by the usual click→select).
        if (!isRenaming) p.onRequestRename?.(path);
      }}
      onContextMenu={(e) => p.onContext?.(path, e)}
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
        // The active tab adopts the editor background + a top accent so it reads
        // as visually connected to the document below the header.
        background: isActive ? 'var(--bg)' : 'transparent',
        color: isActive ? 'var(--text)' : 'var(--text3)',
        fontSize: 12.5,
        fontWeight: isActive ? 600 : 500,
        boxShadow: isActive ? `inset 0 2px 0 ${accent}` : 'none',
        whiteSpace: 'nowrap',
        // @dnd-kit transform/transition: neighbours slide as the dragged tab moves
        // and the lifted tab follows the pointer; it also dims while dragging.
        transform: CSS.Transform.toString(transform),
        transition,
        opacity: isDragging ? 0.55 : 1,
        zIndex: isDragging ? 1 : undefined,
        touchAction: 'none',
      }}
    >
      {isRenaming ? (
        <input
          data-testid="tab-rename-input"
          autoFocus
          value={p.renameValue ?? ''}
          spellCheck={false}
          onClick={(e) => e.stopPropagation()}
          onChange={(e) => p.onRenameChange?.(e.target.value)}
          onKeyDown={(e) => p.onRenameKeyDown?.(e, path)}
          onBlur={() => p.onRenameCommit?.(path)}
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
}

export default function TabBar(props: TabBarProps) {
  const { tabs, active, prettyNames, accent, onSelect, onClose } = props;

  // PointerSensor with a 4px activation distance so a plain click still selects a
  // tab (and dbl-click renames, right-click menus) without ever starting a drag —
  // only a deliberate 4px drag picks the tab up. KeyboardSensor gives accessible
  // (and deterministic, test-friendly) reordering via Space + Arrow keys.
  const sensors = useSensors(
    useSensor(PointerSensor, { activationConstraint: { distance: 4 } }),
    useSensor(KeyboardSensor, { coordinateGetter: sortableKeyboardCoordinates }),
  );

  // Translate @dnd-kit's (activeId, overId) into App's (from, to) index contract
  // and fire onReorder exactly once — never for a no-op (dropped on itself / no
  // target). The tabs prop stays the source of truth; App owns the committed order.
  const handleDragEnd = (e: DragEndEvent) => {
    const move = reorderFromDragEnd(tabs, e.active.id, e.over?.id);
    if (move) props.onReorder?.(move.from, move.to);
  };

  // Read the file path off a NATIVE drop that originated in the file tree.
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
    <DndContext
      sensors={sensors}
      collisionDetection={closestCenter}
      modifiers={[restrictToHorizontalAxis]}
      onDragEnd={handleDragEnd}
    >
      <SortableContext items={tabs} strategy={horizontalListSortingStrategy}>
        <div
          data-testid="tab-bar"
          role="tablist"
          className="tab-strip"
          style={{
            flex: 1,
            minWidth: 0,
            alignSelf: 'stretch',
            display: 'flex',
            alignItems: 'stretch',
            overflowX: 'auto',
            overflowY: 'hidden',
          }}
          // NATIVE HTML5 drop target for "open a file dragged from the tree". This
          // is independent of @dnd-kit (drag events vs pointer events); a per-tab
          // drop bubbles here too, so the whole strip opens tree-dragged files.
          onDragOver={(e) => {
            if (props.onDropOpenPath && e.dataTransfer) e.preventDefault();
          }}
          onDrop={(e) => {
            if (openDroppedPath(e)) e.preventDefault();
          }}
        >
          {tabs.map((path) => (
            <SortableTab
              key={path}
              path={path}
              isActive={path === active}
              isRenaming={props.renamingPath != null && props.renamingPath === path}
              prettyNames={prettyNames}
              accent={accent}
              onSelect={onSelect}
              onClose={onClose}
              onContext={props.onContext}
              onRequestRename={props.onRequestRename}
              renameValue={props.renameValue}
              onRenameChange={props.onRenameChange}
              onRenameKeyDown={props.onRenameKeyDown}
              onRenameCommit={props.onRenameCommit}
            />
          ))}
        </div>
      </SortableContext>
    </DndContext>
  );
}
