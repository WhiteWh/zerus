import { groupRows, type Row } from "./group-rows";
export { groupRows, withContainers, endedParent, type Row, type ListInput, type Container } from "./group-rows";
import { createMemo } from "solid-js";
import { archive, archiveRows, fleetComputers } from "../../store/archive";
import { catalog, computerFresh, hasDraftKey, orderedRows } from "../../store/catalog";
import { machineCards } from "../../store/machines";
import { archiveMachineChoices, archiveProjectChoices } from "./archive-filters";
import { defaultProject, grouping, groups } from "../../store/organization";
import { stateFilter, listedByFilter, type StateFilter, search,machineFilter, projectFilter, accountFilter, folderFilter, expandedChildren, compactRows, selected, selectedKey, subagentOpen } from "../../store/ui";
import { teamCollapsed } from "../../store/teamCollapse";
import { compileScopes, scopeCounts } from "./scope-filters";
import { withCreations } from "../../store/creations";
import { countedAttention, isWorking } from "../../store/attention";
import { followedFor } from "../../store/actions";
import { flag } from "../../store/settings";
import { rosterFor } from "./subagents";
import type { Session } from "./types";

// Flattened list model. Heights match tray/src/SessionCardDelegate.h
// sizeHint at the compact density, the only one in the web client: group
// header 40, card 94, native child 40 (a compact row), collapsed row 40. Fixed heights per row
// type let the virtualizer place rows without measuring the DOM.

export const ROW_HEIGHT = { header: 40, card: 94, child: 40, collapsedRow: 40, collapsedTeam: 62 } as const;

/** In the strip every row keeps its height except compact rows, which show as tiles. */
export function rowHeight(row: Row, strip = false): number {
  if (row.type === "header") return ROW_HEIGHT.header;
  if (row.type === "child") return ROW_HEIGHT.child;
  if (row.type === "creation") return ROW_HEIGHT.card;
  // An ended parent's container is a dimmed compact row with its children's line (zerus-sa9qn.17).
  if (row.type === "container") return row.team ? ROW_HEIGHT.collapsedTeam : ROW_HEIGHT.collapsedRow;
  // A compact parent takes a second line with its child sessions' summary.
  if (row.compactRow && !strip) return row.team ? ROW_HEIGHT.collapsedTeam : ROW_HEIGHT.collapsedRow;
  return ROW_HEIGHT.card;
}

export function sessionMatchesSearch(s: Session, q: string): boolean {
  if (!q) return true;
  const needle = q.toLowerCase();
  return s.name.toLowerCase().includes(needle) || s.title.toLowerCase().includes(needle) || s.summary.toLowerCase().includes(needle)
    || s.machine.toLowerCase().includes(needle) || (s.branch ?? "").toLowerCase().includes(needle);
}

/**
 * Each project's rows in list order: by rank, ties in arrival order (the
 * sort is stable); a row without an assignment is in `fallback`. It reads
 * only row ids and assignments, so a status upsert does not run it again.
 */
export function groupMembers(order: readonly string[], rows: Record<string, Session>,
                             assigned: Record<string, { group: string; rank: number } | undefined>, fallback: string): Map<string, Session[]> {
  const map = new Map<string, Session[]>();
  const rank = new Map<string, number>();
  for (const id of order) {
    const s = rows[id];
    if (!s) continue;
    const a = assigned[id];
    rank.set(id, a?.rank ?? Number.MAX_SAFE_INTEGER);
    const group = a?.group ?? fallback;
    const list = map.get(group);
    if (list) list.push(s);
    else map.set(group, [s]);
  }
  for (const list of map.values()) list.sort((x, y) => rank.get(x.key)! - rank.get(y.key)!);
  return map;
}

/** The window's online state of a row (Qt's `entry.online`). */
const online = (s: Session) => computerFresh(s.computer);
const projectOf = (s: Session) => (s as { group?: string }).group ?? groups[s.key]?.group ?? defaultProject();
const scopes = () => ({ machines: machineFilter(), projects: projectFilter(), accounts: accountFilter(), folder: folderFilter() });
const scoped = createMemo(() => compileScopes(scopes()));

/** The list's rows by project, from the row ids and the assignments only. */
export const membersByGroup = createMemo(() => groupMembers(catalog.order, catalog.rows, groups, defaultProject()));

/**
 * A live row of state tab `f` before the text search: the one rule the list and
 * every tab count share (zerus-sa9qn.18), so each tab lists exactly what its
 * count counts. The machine scope is part of `scoped`.
 */
export const inStateTab = (s: Session, f: StateFilter) => listedByFilter(s, f) && scoped()(s, projectOf(s));
const matchesLiveFilters = (s: Session, f: StateFilter, q: string) => inStateTab(s, f) && sessionMatchesSearch(s, q);

/** Whether the filters list an owner once its collapsed project is revealed. */
export function listedSessionIgnoringCollapse(id: string): boolean {
  if (stateFilter() === "archived") return false;
  const s = catalog.rows[id];
  return !!s && listedByFilter(s, stateFilter()) && sessionMatchesSearch(s, search().trim())
    && compileScopes(scopes())(s, groups[id]?.group ?? defaultProject());
}

/**
 * Each listed row by its computer's machine UUID and durable session ID, and
 * by conversation for a parent an older hgs listed without one (spec 1.4).
 * Rebuilt with the catalog in one pass; names never match.
 */
export function parentIndex(rows: Iterable<Session>, machineOf: (computer: string) => string | undefined) {
  const bySession = new Map<string, string>(), byConversation = new Map<string, Session>();
  for (const s of rows) {
    const machine = machineOf(s.computer);
    if (!machine) continue;
    if (s.sessionId) bySession.set(`${machine}\n${s.sessionId}`, s.key);
    if (s.conversationId) byConversation.set(`${machine}\n${s.conversationId}`, s);
  }
  return (s: Session): string | undefined => {
    const p = s.parent;
    if (!p) return undefined;
    const found = p.session_id ? bySession.get(`${p.machine}\n${p.session_id}`) : undefined;
    if (found) return found;
    const old = p.conversation_id ? byConversation.get(`${p.machine}\n${p.conversation_id}`) : undefined;
    // An identified link only falls back to a parent row that lists no session ID.
    return old && (!p.session_id || !old.sessionId) ? old.key : undefined;
  };
}
/** A listed row's parent key: set only when its parent row is listed too (a child session). */
export const parentKeyOf = createMemo(() => parentIndex(orderedRows(), (computer) => machineCards[computer]?.machine_id));

/** The live list's inputs: the filters, grouping and row state the list reads. */
function listScope() {
  const f = stateFilter();
  const q = search().trim();
  const hosts = machineFilter();
  const visible = (s: Session) => matchesLiveFilters(s, f, q);
  return {
    grouping: grouping(), members: membersByGroup(), visible, online, projects: projectFilter(),
    expanded: (k: string) => !!expandedChildren[k], compact: (k: string) => !!compactRows[k], filter: f, hosts, query: q,
    accounts: accountFilter(), folder: folderFilter(),
    includeCompleted: flag("workspace/showCompletedSubagents"),
    selected: { key: selectedKey(), subagent: subagentOpen(), conversation: selected()?.conversationId },
    roster: (s: Session) => rosterFor(s, s.key === selectedKey() ? followedFor(s.key).value : undefined),
    childDraft: (s: Session, child: { id: string }) => hasDraftKey(`${s.computer}\n${s.name}\n${s.conversationId ?? ""}\n${child.id}`),
    parentKey: parentKeyOf(), teamCollapsed,
    containers: true, machineOf: (s: Session) => machineCards[s.computer]?.machine_id,
  };
}

export const listRows = createMemo<Row[]>(() => {
  if (stateFilter() === "archived") return archiveRows();
  const scope = listScope();
  const out = groupRows(scope);
  return withCreations(out, scope);
});

/**
 * The live list's rows with every project and parent team open: the logical
 * order that attention navigation searches, including the sessions a
 * collapsed project or team hides, each child still under its parent
 * (window-commands.ts). Computed on demand, never kept.
 */
export function rowsAllOpen(): Row[] {
  if (stateFilter() === "archived") return [];
  const scope = listScope();
  const grouping = { ...scope.grouping, headers: scope.grouping.headers.map((header) => ({ ...header, collapsed: false })) };
  return groupRows({ ...scope, grouping, teamCollapsed: () => false });
}

/** The list footer's counts of the cards shown (SessionsWindowRows.cpp:468-474). */
export function summarize(rows: readonly Row[], up: (s: Session) => boolean) {
  let shown = 0, working = 0, attention = 0;
  for (const r of rows) {
    if (r.type !== "card") continue;
    shown++;
    const s = r.session;
    if (isWorking(s, up(s))) working++;
    if (countedAttention(s, up(s))) attention++;
  }
  return { shown, working, attention };
}

export const shownSummary = createMemo(() => summarize(listRows(), online));

/**
 * Each state tab's count: the rows it lists before the text search, as
 * inStateTab composes them (the scope, then listedByFilter), with the scope
 * read once per row.
 */
export const filterCounts = createMemo(() => {
  let all = 0, active = 0, attention = 0, working = 0, paused = 0;
  const inScope = scoped();
  for (const s of orderedRows()) {
    if (!inScope(s, projectOf(s))) continue;
    if (listedByFilter(s, "all")) all++;
    if (listedByFilter(s, "active")) active++;
    if (listedByFilter(s, "attention")) attention++;
    if (listedByFilter(s, "all") && isWorking(s, online(s))) working++;
    if (listedByFilter(s, "paused")) paused++;
  }
  return { all, active, attention, working, paused };
});

export const pickerCounts = createMemo(() => scopeCounts(stateFilter() === "archived" ? archive.rows() : orderedRows(), projectOf, scopes(),
  (s) => stateFilter() === "archived" ? archive.includes((s as { archivedAt?: number }).archivedAt ?? 0) : listedByFilter(s, stateFilter())));

/** Machines with their session counts, in first-seen order (the machine filter's choices). */
export const machineChoices = createMemo(() => {
  return archiveMachineChoices(fleetComputers(catalog.computers, orderedRows(), machineCards), pickerCounts().machine);
});

/** Projects with members and their member counts, in the organization's order (the project filter's choices, by id). */
export const projectChoices = createMemo(() => {
  const g = grouping();
  return archiveProjectChoices(g, pickerCounts().project, projectFilter());
});
