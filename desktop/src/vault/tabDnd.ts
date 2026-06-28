// Pure index-mapping helper for the tab strip's @dnd-kit reordering.
//
// @dnd-kit reports a drag end as (activeId, overId) — the *stable ids* of the
// dragged item and the item it was dropped over (we use each tab's path as its
// id). App's reorder contract, however, speaks in numeric indices into the
// canonical `tabs` array: `onReorder(from, to)`. This translates the former into
// the latter, returning null when there is nothing to do so callers can fire
// `onReorder` exactly once (and never for a no-op drop).
export interface ReorderMove {
  from: number;
  to: number;
}

export function reorderFromDragEnd(
  tabs: readonly string[],
  activeId: string | number | null | undefined,
  overId: string | number | null | undefined,
): ReorderMove | null {
  // No drop target (released outside any sortable) or dropped on itself.
  if (overId == null || activeId == null || activeId === overId) return null;
  const from = tabs.indexOf(String(activeId));
  const to = tabs.indexOf(String(overId));
  // Either id is unknown, or the positions already match → nothing to reorder.
  if (from < 0 || to < 0 || from === to) return null;
  return { from, to };
}
