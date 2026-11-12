import { batch, createSignal } from "solid-js";
import { listRows, type Row } from "./model";
import { archive } from "../../store/archive";
import { catalog } from "../../store/catalog";
import { grouping, groupOf, headerOf, INBOX, moveProject, moveToProject } from "../../store/organization";
import { multi, setMulti, selectedKey } from "../../store/ui";
import { projectOf } from "./creations-ui";
import { nestable, setParentDialog } from "../../store/parentLinks";

// Bulk selection and drag-and-drop organisation of the session list, after
// tray/src/SessionList.cpp (mousePressEvent, selectRange, placement, dropEvent).

// --- Bulk selection --------------------------------------------------------

/** Where a Shift range starts, and its far end, which Shift+arrows move (Qt's m_rangeAnchor, m_rangeCursor). */
let anchor: string | undefined, cursor: string | undefined;
const cardKeys = () => listRows().filter((r) => r.type === "card").map((r) => r.id);

export const multiCount = () => Object.values(multi).filter(Boolean).length;
/** Ends a bulk selection, and its range: the next Shift key starts again from the open conversation (Qt's clearBulkSelection, zerus-m0dem). */
export const clearMulti = () => batch(() => { anchor = cursor = undefined; for (const k of Object.keys(multi)) if (multi[k]) setMulti(k, false); });
/** A plain click or arrow on a card: a later range starts there. */
export const setAnchor = (key: string) => { anchor = cursor = key; };

export type RangeKey = "ArrowUp" | "ArrowDown" | "Home" | "End";

/**
 * The card a Shift key extends the range to (SessionList.cpp:589-602): from
 * the range's far end, else the open conversation; Home and End go to the
 * first and last card; past either end, none.
 */
export function rangeTarget(keys: readonly string[], from: string | undefined, open: string | undefined, key: RangeKey): number | undefined {
  if (!keys.length) return undefined;
  let at = from === undefined ? -1 : keys.indexOf(from);
  if (at < 0 && open !== undefined) at = keys.indexOf(open);
  const target = key === "Home" ? 0 : key === "End" ? keys.length - 1
    : key === "ArrowDown" ? (at < 0 ? 0 : at + 1) : (at < 0 ? keys.length - 1 : at - 1);
  return target >= 0 && target < keys.length ? target : undefined;
}

/** Shift+Up, Down, Home or End (with Ctrl/Cmd, added to the selection): the card the range reached, if any. */
export function rangeStep(key: RangeKey, additive: boolean): string | undefined {
  const keys = cardKeys();
  const target = rangeTarget(keys, cursor, selectedKey(), key);
  if (target === undefined) return undefined;
  bulkSelect(keys[target], additive, true);
  return keys[target];
}

/** Ctrl/Cmd toggles one card; Shift selects the range from the anchor (with
 *  Ctrl/Cmd, added to the current selection). The open conversation stays. */
export function bulkSelect(key: string, additive: boolean, range: boolean) {
  batch(() => {
    if (range) {
      const keys = cardKeys();
      const from = anchor && keys.includes(anchor) ? anchor : selectedKey() ?? key;
      const j = keys.indexOf(key);
      const i = keys.indexOf(from) < 0 ? j : keys.indexOf(from);
      const span = new Set(keys.slice(Math.min(i, j), Math.max(i, j) + 1));
      if (!additive) for (const k of Object.keys(multi)) if (multi[k] && !span.has(k)) setMulti(k, false);
      span.forEach((k) => setMulti(k, true));
      // Qt: an anchor that is no longer listed restarts at the target.
      anchor = i >= 0 ? keys[i] : from;
      cursor = key;
      return;
    }
    // A toggle starts from the open conversation, which is already selected.
    const open = selectedKey();
    if (!multiCount() && open && open !== key) setMulti(open, true);
    setMulti(key, !multi[key]);
    anchor = cursor = key;
  });
}

// --- Drag and drop ---------------------------------------------------------

export type DragItem = { kind: "session" | "group"; id: string };
/** Where a drop lands: a group (project) and the session or group it goes
 *  before (none = the end), drawn as a line or as a frame around a header. */
export type Drop = { group: string; before?: string; line?: number; inset?: number; box?: { top: number; height: number };
  /** Drop onto this session's card to make the dragged session its child (zerus-sa9qn.9). */
  nest?: string };

export const SESSION_MIME = "application/x-zerus-session";
export const [dragItem, setDragItem] = createSignal<DragItem>();
export const [drop, setDrop] = createSignal<Drop>();
export const endDrag = () => batch(() => { setDragItem(undefined); setDrop(undefined); });

/** SessionList::placement for a pointer at content offset `y` over row `overId`. */
export function placement(rows: Row[], offsets: number[], heights: number[], overId: string | undefined, y: number, item: DragItem): Drop {
  const end = rows.length ? offsets[rows.length - 1] + heights[rows.length - 1] : 0;
  let i = overId ? rows.findIndex((r) => r.id === overId) : -1;
  // The blank area after the list is an explicit move to the end of the Inbox, whichever project is the default (SessionList.cpp:482-484).
  if (i < 0) return item.kind === "group" ? { group: "", line: end, inset: 8 } : { group: INBOX, line: end, inset: 12 };
  // A subagent row is not a target of its own: the whole family is one session.
  while (i > 0 && rows[i].type === "child") i--;
  const over = rows[i];
  const group = over.type === "header" || over.type === "container" ? over.project : over.type === "card" ? groupOf(over.session.key)
    : over.type === "creation" ? projectOf(over.creation, grouping()) : groupOf(over.parent.key);
  const top = offsets[i];
  let bottom = top + heights[i];
  if (item.kind === "session") for (let n = i + 1; n < rows.length && rows[n].type === "child"; n++) bottom = offsets[n] + heights[n];
  const nextHeader = () => { let n = i + 1; while (n < rows.length && rows[n].type !== "header") n++; return n; };
  if (item.kind === "group") {
    // The upper half of a header places before its group; anything else, after it.
    if (over.type === "header" && y <= (top + bottom) / 2) return { group, before: group, line: top, inset: 8 };
    const n = nextHeader();
    const next = rows[n];
    return next?.type === "header" ? { group, before: next.project, line: offsets[n], inset: 8 } : { group, line: end, inset: 8 };
  }
  // A session over a header moves into that project (frame around the band).
  if (over.type === "header") return { group, box: { top: top + 6, height: 30 } };
  // The middle of another card on the same computer nests the dragged session under it.
  const moving = catalog.rows[item.id];
  if (over.type === "card" && moving && nestable(moving, over.session)) {
    const height = heights[i];
    if (y > top + height * 0.3 && y < top + height * 0.7) return { group, nest: over.session.key, box: { top: top + 4, height: height - 10 } };
  }
  // An ended parent's container (zerus-sa9qn.17) is no rank position: the session moves into its project.
  if (over.type === "container") return { group, line: y <= (top + bottom) / 2 ? top : bottom, inset: 12 };
  if (y <= (top + bottom) / 2) return { group, before: over.id, line: top, inset: 12 };
  let n = i + 1;
  while (n < rows.length && rows[n].type === "child") n++;
  const next = rows[n];
  return { group, before: next?.type === "card" && groupOf(next.session.key) === group ? next.id : undefined, line: bottom, inset: 12 };
}

/**
 * Applies a drop: a project moves before another (SessionList::groupMoved,
 * moveGroup), a session into a project before a row or at its end, inside
 * its own project too (sessionMoved, moveSession; SessionsWindow.cpp:556-561).
 */
export function applyDrop(item: DragItem, d: Drop) {
  if (d.nest) {
    // A link change is confirmed first; the host checks it.
    const child = catalog.rows[item.id], parent = catalog.rows[d.nest];
    if (child && parent && nestable(child, parent)) setParentDialog({ kind: "set", child, parent });
    return;
  }
  if (d.before === item.id) return;
  if (item.kind === "group") { void moveProject(item.id, d.before); return; }
  const moving = catalog.rows[item.id] ?? archive.row(item.id);
  // Qt's moveSession ignores a project that is not listed (SessionOrganization.cpp:188-189), such as the Inbox once it is deleted.
  if (!moving || (grouping().loaded && !headerOf(d.group))) return;
  void moveToProject([moving], d.group, d.before ? catalog.rows[d.before] ?? archive.row(d.before) : undefined);
}
