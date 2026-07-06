// Pure geometry for the unified history-timeline / branch-network view. The
// bottom timeline IS the network graph now: events are dots positioned by time
// (x) on their branch's lane (y), with curved fork edges where a branch diverged
// from its parent, and tag flags at named moments. Kept DOM-free so the lane
// assignment, fork edges and tag placement are unit-testable; the React
// component (HistoryBar) does only SVG/DOM from these results. The time math
// (toPct, zoom, clampView, axisTicks) stays in history.ts and is reused as-is.

import type { BranchGraphData, GraphBranch } from '../lib/api';
import type { TrackEvent } from './history';

export const MAIN_BRANCH_ID = 'main';

// Adaptive row-height bounds (px). Lanes always fit the track: rowH shrinks from
// IDEAL_ROW toward MIN_ROW as the packed lane count grows, and never scrolls.
export const MIN_ROW = 3;
export const IDEAL_ROW = 16;

export interface Lane {
  branchId: string;
  name: string;
  lane: number; // 0 = main, top lane
  current: boolean;
  parent: string | null;
}

export interface ForkEdge {
  branchId: string;
  fromLane: number; // parent lane
  toLane: number; // child (branch) lane
  ts: number; // epoch ms where the branch's history starts (the divergence)
}

/** A stable, distinct colour per lane (main = the accent, others cycle a palette).
 *  Kept identical to the old network graph so the visual language is unchanged. */
export function laneColor(lane: number, accent: string): string {
  if (lane === 0) return accent;
  const palette = ['#e5484d', '#f76b15', '#46a758', '#8e4ec6', '#0091ff', '#e93d82', '#12a594', '#f5d90a'];
  return palette[(lane - 1) % palette.length];
}

/** Ordered lanes from the graph's branch list (main always lane 0, then by lane
 *  index). A graph is optional — with none, a single `main` lane is returned so the
 *  timeline still renders on a pre-branching / empty vault. */
export function lanesFromGraph(graph: BranchGraphData | null): Lane[] {
  const branches: GraphBranch[] = graph?.branches ?? [];
  if (branches.length === 0) {
    return [{ branchId: MAIN_BRANCH_ID, name: 'main', lane: 0, current: true, parent: null }];
  }
  return [...branches]
    .sort((a, b) => a.lane - b.lane)
    .map((b) => ({ branchId: b.id, name: b.name, lane: b.lane, current: b.current, parent: b.parent }));
}

/** branchId -> lane index, for placing an event/tag on its lane (unknown → 0). */
export function laneIndex(lanes: Lane[]): Map<string, number> {
  const m = new Map<string, number>();
  for (const l of lanes) m.set(l.branchId, l.lane);
  return m;
}

/** The lane an event sits on: its own branch's lane, else main (0). */
export function laneForEvent(e: TrackEvent, idx: Map<string, number>): number {
  return idx.get(e.branchId) ?? 0;
}

/** Fork edges: for every non-main lane with a parent, an edge from the parent lane
 *  to the branch lane at the branch's earliest event time (its divergence point on
 *  the timeline). Branches with no events yet are skipped (nothing to point at).
 *  Deterministic and order-independent. */
export function forkEdges(lanes: Lane[], events: TrackEvent[]): ForkEdge[] {
  const idx = laneIndex(lanes);
  // Earliest event ts per branch.
  const earliest = new Map<string, number>();
  for (const e of events) {
    const cur = earliest.get(e.branchId);
    if (cur == null || e.ts < cur) earliest.set(e.branchId, e.ts);
  }
  const out: ForkEdge[] = [];
  for (const l of lanes) {
    if (l.branchId === MAIN_BRANCH_ID || l.parent == null) continue;
    const ts = earliest.get(l.branchId);
    if (ts == null) continue; // no commits on this branch yet
    const fromLane = idx.get(l.parent) ?? 0;
    out.push({ branchId: l.branchId, fromLane, toLane: l.lane, ts });
  }
  return out;
}

/** Fallback dots from the coarsened graph commits, for surfaces where the full
 *  per-event `history()` isn't available (web degrades it to empty). Each commit
 *  becomes a track event on its branch lane so the timeline is never blank. */
export function nodesToEvents(graph: BranchGraphData | null): TrackEvent[] {
  const nodes = graph?.nodes ?? [];
  return nodes
    .map((n) => ({ id: n.commit_id, ts: n.ts * 1000, kind: 'edit', path: n.label, branchId: n.branch_id }))
    .sort((a, b) => a.ts - b.ts);
}

/** Vertical geometry for `laneCount` lanes inside a track of `height` px: the y of
 *  each lane and a per-lane row height. Lanes ALWAYS fit the track — there is no
 *  scroll (the wheel/drag vocabulary is reserved for the time axis). rowH shrinks
 *  adaptively from IDEAL_ROW toward MIN_ROW as lanes grow:
 *  `rowH = clamp(height / laneCount, MIN_ROW, IDEAL_ROW)`. With a handful of lanes
 *  they stay centred exactly as before (single lane → the centred row, and every
 *  dot sits at height/2 regardless of rowH); at genuinely high packed counts the
 *  rows become thin threads (fisheye magnification arrives in wave B).
 *
 *  `contentH` always equals `height`: the surface fills the track and the track
 *  clips both axes. `y` is the single seam a wave-B magnification transform wraps.
 */
export function laneGeometry(laneCount: number, height: number): { rowH: number; y: (lane: number) => number; top: number; contentH: number } {
  const n = Math.max(1, laneCount);
  const rowH = Math.max(MIN_ROW, Math.min(IDEAL_ROW, height / n));
  const used = rowH * n;
  // Centre while the stack fits; top-anchor once it can't (only at extreme counts,
  // where the track clip trims the overflow — nothing paints out of bounds).
  const top = used <= height ? (height - used) / 2 + rowH / 2 : rowH / 2;
  return { rowH, top, contentH: height, y: (lane: number) => top + lane * rowH };
}

// ---- wave B: 1-D vertical fisheye (lane magnification) ----------------------
// When packed rows are thin, hovering the track vertically magnifies the lanes
// nearest the cursor — the macOS dock rotated 90°. The hovered lane swells to
// ~IDEAL_ROW, its neighbours taper via a cosine bump, and the rest compress so
// the whole stack stays EXACTLY `height` tall (no clip change, no scroll). It is
// a strict no-op (identity) when rows are already comfortable or nothing is
// hovered — normal vaults see zero change. Pure so waves B/C can reason about it.

/** rowH below this (px) counts as "thin" → fisheye engages. At/above it, the
 *  layout is already legible and fisheye is an identity transform. */
export const FISHEYE_THRESHOLD = 10;
/** Falloff reach of the magnification, in lanes (each side of the focus). */
export const FISHEYE_RADIUS = 3.5;
/** Floor (px) guaranteed to pinned anchor lanes (main, current, selected) even
 *  when they're far from the focus, so they stay readable under magnification. */
export const FISHEYE_PIN_MIN = 8;

export interface FisheyeGeom {
  /** Centre-line y (px) of a lane — same seam semantics as laneGeometry.y. */
  y: (lane: number) => number;
  /** The lane's row height (px) under the current magnification. */
  rowH: (lane: number) => number;
  /** Surface height — always the track height (clip contract unchanged). */
  contentH: number;
  /** Lane nearest the cursor (the "focused" lane), or null when inactive. */
  focusLane: number | null;
  /** True only when the magnification is actually applied. */
  active: boolean;
}

/** 1-D vertical fisheye over `laneCount` lanes in a `height`px track, focused at
 *  `focusY` px (null = no hover). Invariants (see timeline.test.ts):
 *   - focusY null OR uniform rowH >= threshold OR laneCount<=1 ⇒ EXACT identity
 *     with laneGeometry (same y for every lane; zero visual change).
 *   - monotonic: y(i+1) > y(i) always; the stack fits [0, height].
 *   - focused (nearest) lane gets >= IDEAL_ROW*0.9; pinned lanes get >= PIN_MIN.
 *   - continuous in focusY (no jumps), deterministic, never NaN.
 *  `pinned` defaults to lanes {0,1} — main and the wave-A current-branch pin.
 */
export function fisheyeLaneYs(
  laneCount: number,
  height: number,
  focusY: number | null,
  opts?: { pinned?: Iterable<number>; radius?: number; threshold?: number; pinMin?: number; ideal?: number },
): FisheyeGeom {
  const n = Math.max(1, laneCount);
  const base = laneGeometry(n, height);
  const threshold = opts?.threshold ?? FISHEYE_THRESHOLD;
  // Identity: comfortable rows, no hover, or a single lane → the uniform layout,
  // byte-for-byte (return the same y closure so equality is exact).
  if (focusY == null || base.rowH >= threshold || n <= 1) {
    return { y: base.y, rowH: () => base.rowH, contentH: base.contentH, focusLane: null, active: false };
  }
  const radius = opts?.radius ?? FISHEYE_RADIUS;
  const ideal = opts?.ideal ?? IDEAL_ROW;
  const pinMin = opts?.pinMin ?? FISHEYE_PIN_MIN;
  const pinned = new Set<number>(opts?.pinned ?? [0, 1]);

  // Continuous focus position in lane space: invert the uniform seam so the
  // magnification tracks the cursor smoothly. The nearest integer lane is "the"
  // focused lane (label priority, guaranteed height).
  const fLane = Math.max(0, Math.min(n - 1, (focusY - base.top) / base.rowH));
  const focusLane = Math.round(fLane);

  const kernel = (lane: number): number => {
    const d = Math.abs(lane - fLane) / radius;
    return d >= 1 ? 0 : 0.5 * (1 + Math.cos(Math.PI * d));
  };

  // Desired height per lane: a cosine bump peaking at `ideal` on the focus lane,
  // floored to pinMin for anchors and to a hair above 0 for everyone (keeps the
  // stack strictly monotonic even where the bump is zero). baseMin is kept
  // negligible so it never inflates the desired total into the down-scale branch
  // (which would relax the pin/focus floors) — for any feasible pane the bump
  // total (~ideal*2*radius) plus two pin floors stays well under `height`.
  const baseMin = 1e-4;
  const desired = new Array<number>(n);
  let sumD = 0;
  for (let i = 0; i < n; i++) {
    desired[i] = Math.max(kernel(i) * ideal, pinned.has(i) ? pinMin : 0, baseMin);
    sumD += desired[i];
  }

  const h = new Array<number>(n);
  if (sumD <= height) {
    // Headroom: spread it uniformly so the stack is EXACTLY `height`. Because we
    // only ADD a constant, the focus lane keeps its full `ideal*kernel` bump
    // (>= 0.9*ideal at the nearest lane) and pinned lanes stay >= pinMin.
    const c = (height - sumD) / n;
    for (let i = 0; i < n; i++) h[i] = desired[i] + c;
  } else {
    // Tiny pane: floors+bump already exceed the track. Scale to fit — graceful,
    // rare (needs height < ~72px); the >=0.9*ideal guarantee relaxes here.
    const s = height / sumD;
    for (let i = 0; i < n; i++) h[i] = desired[i] * s;
  }

  // Cumulative centre-line per lane; fills [0, height] (sum h === height).
  const centre = new Array<number>(n);
  let acc = 0;
  for (let i = 0; i < n; i++) {
    centre[i] = acc + h[i] / 2;
    acc += h[i];
  }
  const at = (lane: number) => Math.max(0, Math.min(n - 1, Math.round(lane)));
  return {
    y: (lane: number) => centre[at(lane)],
    rowH: (lane: number) => h[at(lane)],
    contentH: base.contentH,
    focusLane,
    active: true,
  };
}

// ---- interval lane packing -------------------------------------------------
// Pack branches onto as few display lanes as possible by treating each branch's
// activity window as an interval and running greedy interval partitioning
// (provably minimal lane count). This is what makes a 128-branch `--all-branches`
// clone legible: most branches are stale, their intervals are disjoint, so they
// collapse onto a handful of shared lanes. Pure + deterministic + cheap, so waves
// B/C can re-run it client-side over a subset/grouping of branches.

/** A branch's activity window on the timeline. `start`/`end` are epoch ms;
 *  `end` is the branch's LAST COMMIT (never "now") — that's what frees a stale
 *  branch's lane. `pinned` anchors the checked-out branch to a stable low lane. */
export interface LaneSpan {
  branchId: string;
  start: number;
  end: number;
  pinned?: boolean;
}

// Deterministic span ordering: by start, then end, then id (id tiebreak so the
// output map is independent of input array order).
function cmpSpan(a: LaneSpan, b: LaneSpan): number {
  if (a.start !== b.start) return a.start - b.start;
  if (a.end !== b.end) return a.end - b.end;
  return a.branchId < b.branchId ? -1 : a.branchId > b.branchId ? 1 : 0;
}

// A tiny binary min-heap of open lanes keyed by (end, lane) — the earliest-ending
// reusable lane, index tiebreak for determinism.
class LaneHeap {
  private a: { end: number; lane: number }[] = [];
  get size() { return this.a.length; }
  peek() { return this.a[0]; }
  private lt(x: { end: number; lane: number }, y: { end: number; lane: number }) {
    return x.end !== y.end ? x.end < y.end : x.lane < y.lane;
  }
  push(v: { end: number; lane: number }) {
    const a = this.a;
    a.push(v);
    let i = a.length - 1;
    while (i > 0) {
      const p = (i - 1) >> 1;
      if (this.lt(a[i], a[p])) { [a[i], a[p]] = [a[p], a[i]]; i = p; } else break;
    }
  }
  pop() {
    const a = this.a;
    const top = a[0];
    const last = a.pop()!;
    if (a.length) {
      a[0] = last;
      let i = 0;
      for (;;) {
        const l = 2 * i + 1, r = 2 * i + 2;
        let s = i;
        if (l < a.length && this.lt(a[l], a[s])) s = l;
        if (r < a.length && this.lt(a[r], a[s])) s = r;
        if (s === i) break;
        [a[i], a[s]] = [a[s], a[i]];
        i = s;
      }
    }
    return top;
  }
}

/** Greedy interval partitioning → branchId → display lane. Rules:
 *  - `main` is ALWAYS lane 0 and never shares it (pinned exclusively).
 *  - the checked-out branch (a `pinned` span that isn't main) is anchored to lane
 *    1 so "the branch you're on" stays put; if several are flagged, the lowest
 *    branchId wins (deterministic).
 *  - every other branch is packed into the lowest lane whose last end (+epsilon)
 *    clears this span's start, else a fresh lane. This yields the minimal number
 *    of lanes for the non-pinned set.
 *  - `epsilon` (default ≈1% of the total time range) inflates each interval's tail
 *    so adjacent branches never visually touch. Deterministic: sorts internally,
 *    so the same spans in any order produce the same map. */
export function packLanes(spans: LaneSpan[], opts?: { epsilon?: number }): Map<string, number> {
  const map = new Map<string, number>();
  if (spans.length === 0) return map;

  let minStart = Infinity, maxEnd = -Infinity;
  for (const s of spans) {
    if (s.start < minStart) minStart = s.start;
    if (s.end > maxEnd) maxEnd = s.end;
  }
  const range = maxEnd - minStart;
  const epsilon = opts?.epsilon ?? (range > 0 ? range * 0.01 : 0);

  const reserved = new Set<number>();
  const mainSpan = spans.find((s) => s.branchId === MAIN_BRANCH_ID);
  if (mainSpan) { map.set(MAIN_BRANCH_ID, 0); reserved.add(0); }

  const currentPin = spans
    .filter((s) => s.pinned && s.branchId !== MAIN_BRANCH_ID)
    .sort((a, b) => (a.branchId < b.branchId ? -1 : a.branchId > b.branchId ? 1 : 0))[0];
  if (currentPin) { map.set(currentPin.branchId, 1); reserved.add(1); }

  const rest = spans
    .filter((s) => s.branchId !== MAIN_BRANCH_ID && s !== currentPin)
    .sort(cmpSpan);

  const heap = new LaneHeap();
  let nextLane = 0;
  const allocLane = () => {
    while (reserved.has(nextLane)) nextLane++;
    const l = nextLane;
    nextLane++;
    return l;
  };
  for (const s of rest) {
    if (heap.size && heap.peek().end + epsilon <= s.start) {
      const { lane } = heap.pop();
      map.set(s.branchId, lane);
      heap.push({ end: s.end, lane });
    } else {
      const lane = allocLane();
      map.set(s.branchId, lane);
      heap.push({ end: s.end, lane });
    }
  }
  return map;
}

/** Activity span per branch from the (capped) graph nodes. A branch's window is
 *  min/max `ts` over its OWN nodes (`n.branch_id === id`), converted to ms so it
 *  lines up with the timeline's TrackEvent ms. An open branch therefore ends at
 *  its last commit, not "now" — the point of packing. A branch with no nodes in
 *  the capped graph gets a zero-length span at its head commit's time (else the
 *  graph's floor, so it doesn't distort the range). `pinned` mirrors `current`. */
export function branchSpans(graph: BranchGraphData | null): LaneSpan[] {
  const branches = graph?.branches ?? [];
  const nodes = graph?.nodes ?? [];
  const byBranch = new Map<string, { mn: number; mx: number }>();
  const tsByCommit = new Map<string, number>();
  let floor = Infinity;
  for (const n of nodes) {
    const ms = n.ts * 1000;
    tsByCommit.set(n.commit_id, ms);
    if (ms < floor) floor = ms;
    const cur = byBranch.get(n.branch_id);
    if (cur) { if (ms < cur.mn) cur.mn = ms; if (ms > cur.mx) cur.mx = ms; }
    else byBranch.set(n.branch_id, { mn: ms, mx: ms });
  }
  if (!Number.isFinite(floor)) floor = 0;
  return branches.map((b) => {
    const own = byBranch.get(b.id);
    if (own) return { branchId: b.id, start: own.mn, end: own.mx, pinned: b.current };
    const headTs = b.head_commit != null ? tsByCommit.get(b.head_commit) : undefined;
    const at = headTs ?? floor;
    return { branchId: b.id, start: at, end: at, pinned: b.current };
  });
}

/** Branches with packed display lanes (main always 0). Ordered by lane then id.
 *  Empty graph → a single `main` lane so the timeline still renders. This replaces
 *  `lanesFromGraph` at render time (which used the Rust creation-order lane). */
export function packedLanes(graph: BranchGraphData | null): Lane[] {
  const branches = graph?.branches ?? [];
  if (branches.length === 0) {
    return [{ branchId: MAIN_BRANCH_ID, name: 'main', lane: 0, current: true, parent: null }];
  }
  const map = packLanes(branchSpans(graph));
  return branches
    .map((b) => ({ branchId: b.id, name: b.name, lane: map.get(b.id) ?? 0, current: b.current, parent: b.parent }))
    .sort((a, b) => a.lane - b.lane || (a.branchId < b.branchId ? -1 : a.branchId > b.branchId ? 1 : 0));
}

/** Number of distinct display lanes (max lane + 1) — what laneGeometry needs. */
export function laneCountOf(lanes: Lane[]): number {
  let mx = 0;
  for (const l of lanes) if (l.lane > mx) mx = l.lane;
  return mx + 1;
}

// ---- wave C: collapsible prefix groups (accordion) -------------------------
// Repos cloned with --all-branches carry branch-name farms:
// `dependabot/npm_and_yarn/…`, `cursor/…`, `claude/…`. We group branches by their
// FIRST path segment (text before the first `/`). A group with enough members can
// COLLAPSE to a single display lane — its members' union activity window becomes
// ONE span that packs (wave A) and magnifies (wave B) exactly like any branch —
// then EXPAND back into individual spans on demand. `main` and the checked-out
// branch are NEVER grouped: they keep their pinned lanes (0 / 1). Pure +
// deterministic (sorts by prefix) so the render layer stays DOM-only.

/** Default: a prefix needs at least this many members to become a group. */
export const DEFAULT_MIN_GROUP_SIZE = 3;
/** Default accordion policy: if the ungrouped packed-lane count exceeds this, the
 *  groups start COLLAPSED (a branch-name farm is drowning the pane); at/below it
 *  the whole graph is small enough that grouping would only add noise, so groups
 *  default to all-expanded and the pane renders byte-identical to wave B. */
export const GROUP_LANE_THRESHOLD = 12;

/** The synthetic span/lane id a collapsed group packs under. Distinct from any real
 *  branch id (which are content hashes) by the `group:` sentinel. */
export function groupSpanId(prefix: string): string {
  return `group:${prefix}`;
}

/** A prefix group: its members and the union activity window they collapse to. */
export interface BranchGroup {
  prefix: string;
  branchIds: string[]; // members, sorted (deterministic)
  span: LaneSpan; // union of member spans, keyed by groupSpanId(prefix)
}

/** Group branches by their first path segment. Only prefixes with >= minGroupSize
 *  members (after excluding `main` and the current branch, which are never grouped)
 *  become groups; branches without a `/`, or in a too-small prefix, stay individual.
 *  The group's `span` is the union (min start … max end) of its members' activity
 *  windows. Deterministic: groups sorted by prefix, members sorted by id. */
export function groupBranches(graph: BranchGraphData | null, opts?: { minGroupSize?: number }): BranchGroup[] {
  const branches = graph?.branches ?? [];
  const minSize = opts?.minGroupSize ?? DEFAULT_MIN_GROUP_SIZE;
  const spanById = new Map(branchSpans(graph).map((s) => [s.branchId, s]));
  const buckets = new Map<string, string[]>();
  for (const b of branches) {
    if (b.id === MAIN_BRANCH_ID || b.current) continue; // pins are never grouped
    const slash = b.name.indexOf('/');
    if (slash <= 0) continue; // no prefix (or a leading slash) → singleton
    const prefix = b.name.slice(0, slash);
    const bucket = buckets.get(prefix);
    if (bucket) bucket.push(b.id);
    else buckets.set(prefix, [b.id]);
  }
  const out: BranchGroup[] = [];
  for (const [prefix, ids] of buckets) {
    if (ids.length < minSize) continue;
    const branchIds = [...ids].sort((a, b) => (a < b ? -1 : a > b ? 1 : 0));
    let start = Infinity, end = -Infinity;
    for (const id of branchIds) {
      const s = spanById.get(id);
      if (!s) continue;
      if (s.start < start) start = s.start;
      if (s.end > end) end = s.end;
    }
    if (!Number.isFinite(start)) { start = 0; end = 0; }
    out.push({ prefix, branchIds, span: { branchId: groupSpanId(prefix), start, end, pinned: false } });
  }
  return out.sort((a, b) => (a.prefix < b.prefix ? -1 : a.prefix > b.prefix ? 1 : 0));
}

/** A collapsed group's display lane (its union span's packed lane + members). */
export interface GroupLane {
  prefix: string;
  lane: number;
  branchIds: string[];
  span: LaneSpan;
}

/** The full grouped layout: what HistoryBar renders from. Collapsed groups fold
 *  their members' spans into ONE union span before packing (wave-A re-pack
 *  composability) so a 12-branch `dependabot/*` farm costs a single lane; expanded
 *  groups contribute their members as ordinary individual spans (identical to the
 *  ungrouped layout). Pins (main/current) are untouched either way — re-packing
 *  with a different span list may legitimately reshuffle everything else. */
export interface GroupedLayout {
  /** Individual branch lanes (ungrouped branches + expanded-group members) — drives
   *  polylines and per-branch labels, exactly like the pre-wave-C `lanes`. */
  lanes: Lane[];
  /** One entry per COLLAPSED group (chip + micro-dots + guide). */
  groupLanes: GroupLane[];
  /** EVERY branch id → its display lane (a collapsed member resolves to its group's
   *  lane, so its events land there as micro-dots). Feeds laneForEvent/forkEdges. */
  laneOfBranch: Map<string, number>;
  /** Collapsed member branchId → its group prefix (micro-dot styling + expand click). */
  memberGroup: Map<string, string>;
  /** Distinct display-lane count (incl. group lanes) — what laneGeometry needs. */
  laneCount: number;
}

/** Packed display lanes with the accordion applied. `expanded` holds the prefixes
 *  currently expanded; every other group collapses to one lane. With no groups (or
 *  every group expanded) the result is byte-identical to `packedLanes(graph)`. */
export function packedLanesGrouped(
  graph: BranchGraphData | null,
  groups: BranchGroup[],
  expanded: Set<string>,
): GroupedLayout {
  const branches = graph?.branches ?? [];
  if (branches.length === 0) {
    return {
      lanes: [{ branchId: MAIN_BRANCH_ID, name: 'main', lane: 0, current: true, parent: null }],
      groupLanes: [],
      laneOfBranch: new Map([[MAIN_BRANCH_ID, 0]]),
      memberGroup: new Map(),
      laneCount: 1,
    };
  }

  const collapsed = groups.filter((g) => !expanded.has(g.prefix));
  const memberGroup = new Map<string, string>(); // branchId → prefix
  const groupIdOf = new Map<string, string>(); // branchId → groupSpanId
  for (const g of collapsed) {
    for (const id of g.branchIds) { memberGroup.set(id, g.prefix); groupIdOf.set(id, groupSpanId(g.prefix)); }
  }

  // Span list to pack: one union span per collapsed group, plus every branch that
  // isn't folded into a collapsed group.
  const packSpans: LaneSpan[] = collapsed.map((g) => g.span);
  for (const s of branchSpans(graph)) {
    if (memberGroup.has(s.branchId)) continue;
    packSpans.push(s);
  }
  const map = packLanes(packSpans);

  const laneOfBranch = new Map<string, number>();
  const lanes: Lane[] = [];
  for (const b of branches) {
    const gid = groupIdOf.get(b.id);
    const lane = gid != null ? map.get(gid) ?? 0 : map.get(b.id) ?? 0;
    laneOfBranch.set(b.id, lane);
    if (!memberGroup.has(b.id)) {
      lanes.push({ branchId: b.id, name: b.name, lane, current: b.current, parent: b.parent });
    }
  }
  lanes.sort((a, b) => a.lane - b.lane || (a.branchId < b.branchId ? -1 : a.branchId > b.branchId ? 1 : 0));

  const groupLanes: GroupLane[] = collapsed
    .map((g) => ({ prefix: g.prefix, lane: map.get(groupSpanId(g.prefix)) ?? 0, branchIds: g.branchIds, span: g.span }))
    .sort((a, b) => a.lane - b.lane || (a.prefix < b.prefix ? -1 : a.prefix > b.prefix ? 1 : 0));

  let mx = 0;
  for (const v of map.values()) if (v > mx) mx = v;
  return { lanes, groupLanes, laneOfBranch, memberGroup, laneCount: mx + 1 };
}

// ---- label decluttering ----------------------------------------------------

/** A candidate branch-name chip in pixel space. */
export interface LabelBox {
  id: string;
  x: number;
  y: number;
  w: number;
  h: number;
  priority: number;
}

/** Greedy pixel-space collision pass: keep the highest-priority labels whose AABB
 *  (padded) doesn't overlap an already-kept one. At 128 branch tips this thins the
 *  chips to a readable set; hidden ones fall back to a dot + native `title` hover.
 *  Deterministic (id tiebreak on equal priority). Returns the ids to render. */
export function declutterLabels(items: LabelBox[], pad = 2): Set<string> {
  const sorted = [...items].sort(
    (a, b) => b.priority - a.priority || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0),
  );
  const kept: LabelBox[] = [];
  const keptSet = new Set<string>();
  const hits = (a: LabelBox, b: LabelBox) =>
    a.x - pad < b.x + b.w + pad &&
    a.x + a.w + pad > b.x - pad &&
    a.y - pad < b.y + b.h + pad &&
    a.y + a.h + pad > b.y - pad;
  for (const it of sorted) {
    if (kept.some((k) => hits(it, k))) continue;
    kept.push(it);
    keptSet.add(it.id);
  }
  return keptSet;
}
