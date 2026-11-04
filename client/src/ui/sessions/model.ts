import { groupRows, type Row } from "./group-rows";
export { groupRows, type Row, type ListInput } from "./group-rows";
import { createMemo } from "solid-js";
import { archive, archiveRows, fleetComputers } from "../../store/archive";
import { catalog, computerFresh, hasDraftKey, orderedRows } from "../../store/catalog";
import { machineCards } from "../../store/machines";
import { archiveMachineChoices, archiveProjectChoices } from "./archive-filters";
import { defaultProject, grouping, groups } from "../../store/organization";
import { stateFilter, matchesFilter, listedByFilter, search,machineFilter, projectFilter, accountFilter, folderFilter, expandedChildren, compactRows, selected, selectedKey, subagentOpen } from "../../store/ui";
import { compileScopes, scopeCounts } from "./scope-filters";
import { withCreations } from "../../store/creations";
import { isWorking, needsAttention } from "../../store/attention";
import { followedFor } from "../../store/actions";
import { flag } from "../../store/settings";
import { rosterFor } from "./subagents";
import type { Session } from "./types";

// Flattened list model. Heights match tray/src/SessionCardDelegate.h
// sizeHint at the compact density, the only one in the web client: group
// header 40, card 94, native child 64, collapsed row 40. Fixed heights per row
// type let the virtualizer place rows without measuring the DOM.

export const ROW_HEIGHT = { header: 40, card: 94, child: 64, collapsedRow: 40 } as const;

/** In the strip every row keeps its height except compact rows, which show as tiles. */
export function rowHeight(row: Row, strip = false): number {
  if (row.type === "header") return ROW_HEIGHT.header;
  if (row.type === "child") return ROW_HEIGHT.child;
  if (row.type === "creation") return ROW_HEIGHT.card;
  if (row.compactRow && !strip) return ROW_HEIGHT.collapsedRow;
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

const matchesLiveFilters = (s: Session, f: Parameters<typeof matchesFilter>[1], hosts: readonly string[], q: string) =>
  listedByFilter(s, f) && (!hosts.length || hosts.includes(s.machine)) && scoped()(s, projectOf(s)) && sessionMatchesSearch(s, q);

/** Whether the filters list an owner once its collapsed project is revealed. */
export function listedSessionIgnoringCollapse(id: string): boolean {
  if (stateFilter() === "archived") return false;
  const s = catalog.rows[id];
  return !!s && listedByFilter(s, stateFilter()) && sessionMatchesSearch(s, search().trim())
    && compileScopes(scopes())(s, groups[id]?.group ?? defaultProject());
}

export const listRows = createMemo<Row[]>(() => {
  if (stateFilter() === "archived") return archiveRows();
  const f = stateFilter();
  const q = search().trim();
  const hosts = machineFilter();
  const visible = (s: Session) => matchesLiveFilters(s, f, hosts, q);
  const scope = {
    grouping: grouping(), members: membersByGroup(), visible, online, projects: projectFilter(),
    expanded: (k: string) => !!expandedChildren[k], compact: (k: string) => !!compactRows[k], filter: f, hosts, query: q,
    accounts: accountFilter(), folder: folderFilter(),
    includeCompleted: flag("workspace/showCompletedSubagents"),
    selected: { key: selectedKey(), subagent: subagentOpen(), conversation: selected()?.conversationId },
    roster: (s: Session) => rosterFor(s, s.key === selectedKey() ? followedFor(s.key).value : undefined),
    childDraft: (s: Session, child: { id: string }) => hasDraftKey(`${s.computer}\n${s.name}\n${s.conversationId ?? ""}\n${child.id}`),
  };
  const out = groupRows(scope);
  return withCreations(out, scope);
});

/** The list footer's counts of the cards shown (SessionsWindowRows.cpp:468-474). */
export function summarize(rows: readonly Row[], up: (s: Session) => boolean) {
  let shown = 0, working = 0, attention = 0;
  for (const r of rows) {
    if (r.type !== "card") continue;
    shown++;
    const s = r.session;
    if (isWorking(s, up(s))) working++;
    if (!s.muted && needsAttention(s, up(s))) attention++;
  }
  return { shown, working, attention };
}

export const shownSummary = createMemo(() => summarize(listRows(), online));

export const filterCounts = createMemo(() => {
  let all = 0, active = 0, attention = 0, working = 0, paused = 0;
  for (const s of orderedRows()) {
    if (!scoped()(s, projectOf(s))) continue;
    all++;
    if (matchesFilter(s, "active")) active++;
    if (matchesFilter(s, "attention") && !s.muted) attention++;
    if (isWorking(s, online(s))) working++;
    if (s.status === "paused") paused++;
  }
  return { all, active, attention, working, paused };
});

export const pickerCounts = createMemo(() => scopeCounts(stateFilter() === "archived" ? archive.rows() : orderedRows(), projectOf, scopes(),
  (s) => stateFilter() === "archived" ? archive.includes((s as { archivedAt?: number }).archivedAt ?? 0) : matchesFilter(s, stateFilter())));

/** Machines with their session counts, in first-seen order (the machine filter's choices). */
export const machineChoices = createMemo(() => {
  return archiveMachineChoices(fleetComputers(catalog.computers, orderedRows(), machineCards), pickerCounts().machine);
});

/** Projects with members and their member counts, in the organization's order (the project filter's choices, by id). */
export const projectChoices = createMemo(() => {
  const g = grouping();
  return archiveProjectChoices(g, pickerCounts().project, projectFilter());
});
