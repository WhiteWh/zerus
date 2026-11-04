import { batch, createEffect, createMemo, createRoot, createSignal, on, untrack } from "solid-js";
import * as bridge from "../bridge";
import { bindArchivedRows as bindActionRows } from "./actions";
import { catalog, toSession, type ComputerState } from "./catalog";
import type { RowMarks } from "./marks";
import { bindArchivedRows as bindSelectedRows, compactRows, machineFilter, page, projectFilter, accountFilterSet, folderFilter, search, stateFilter } from "./ui";
import { acceptsAccount, acceptsFolder } from "../ui/sessions/scope-filters";
import { initialRange, rangeBounds, type DateRange } from "../ui/sessions/archive-dates";
import { bindArchivedGroups, emptyGrouping, grouping } from "./organization";
import { groupRows, type Row } from "../ui/sessions/group-rows";
import { machineCards } from "./machines";
import type { ArchivePage, ArchiveRow, ComputerId, GroupingOut, GroupOut, MachineMeta, ViewRow } from "../types";
import type { Session } from "../ui/sessions/types";
import { trNoop, tr } from "../i18n";

// The Archive view's pages, after tray/src/ArchiveBrowser.cpp: one bounded
// page and two cursor anchors per computer, never a growing history. Older
// loads a page's cursor and Newer its previous cursor, each replacing the
// page. Pages load only while the Archive filter shows the list. Refresh, a
// query change (after 300 ms), a date-range change and a change of the paging
// computers drop every page; each request carries the store's own token
// (the page's request id), so an answer for a dropped or cancelled page is
// discarded, as Qt's cancelArchivePages. Leaving the Archive filter discards
// the answers in flight and keeps the loaded pages (Qt's configure).

export const ARCHIVE_CHANGED = trNoop("Archive changed. Refresh to start a new view.");
export const QUERY_DELAY_MS = 300;
/** Errors show at most this many characters; the hint has them all. */
export const ERROR_CHARS = 240;
/** Older and Newer lose their text below this width. */
export const NARROW_PX = 310;
/** A newer request of the same computer replaced this one (src-tauri `archive_page`). */
const SUPERSEDED = "superseded";

/** A computer whose archive is paged: its id and its machine name as its rows name it. */
export interface PagingComputer { computer: ComputerId; machine: string }

/** An archived row: a Session keyed by computer and archive id, and when it was archived. */
export type ArchivedSession = Session & { archivedAt: number; group: string; rank: number };

export interface Page {
  rows: ArchivedSession[];
  cursor: string;
  previous: string;
  error: string;
  /** The request in flight (the store's token), 0 when none. */
  request: number;
  /** The reply whose rows are shown, also while Archive changed is shown. */
  shownRequest: number;
  total: number;
  /** The window's first row in the whole archive, -1 when not reported. */
  position: number;
  loaded: boolean;
  newer: boolean;
}

export const emptyPage = (): Page => ({ rows: [], cursor: "", previous: "", error: "", request: 0, shownRequest: 0, total: 0, position: -1, loaded: false, newer: false });

export interface ArchiveStatus {
  summary: string;
  /** Each computer's window, one per line. */
  hint: string;
  /** "{machine}: {error}" per computer, at most ERROR_CHARS characters. */
  error: string;
  errorHint: string;
  older: { visible: boolean; enabled: boolean };
  newer: { visible: boolean; enabled: boolean };
  refreshEnabled: boolean;
}

type Pages = Readonly<Record<string, Page>>;

/** The pages in Qt's map order (by computer id). */
const pageIds = (pages: Pages) => Object.keys(pages).sort();
const machineOf = (hosts: readonly PagingComputer[], computer: string) => hosts.find((h) => h.computer === computer)?.machine ?? computer;

export const totalOf = (pages: Pages) => Object.values(pages).reduce((n, p) => n + p.total, 0);

/** Every paging computer's page has loaded once. */
export const totalsKnownOf = (pages: Pages, hosts: readonly PagingComputer[]) => hosts.length > 0 && hosts.every((h) => pages[h.computer]?.loaded);

/** Some computer has rows beyond its page, a page still to load, or an error. */
export const hasMoreOf = (pages: Pages, hosts: readonly PagingComputer[]) => hosts.some((h) => {
  const p = pages[h.computer] ?? emptyPage();
  return !p.loaded || !!p.cursor || !!p.previous || p.rows.length < p.total || !!p.error;
});

/** The Archive filter's count (SessionsWindowRows.cpp 563-565): "…" until the totals are known with none counted, "+" while more remains. */
export function countText(count: number, pages: Pages, hosts: readonly PagingComputer[]): string {
  const paged = hosts.length > 0;
  const known = totalsKnownOf(pages, hosts);
  if (paged && !known && !count) return "…";
  return `${count}${paged && (!known || hasMoreOf(pages, hosts)) ? "+" : ""}`;
}

/** The Archive filter's tooltip (SessionsWindowRows.cpp 564): "{caption} / {n} loaded", and whether more can be loaded; the plain count without paging. */
export function tipText(caption: string, count: number, pages: Pages, hosts: readonly PagingComputer[]): string {
  if (!hosts.length) return `${caption} / ${count}`;
  const partial = !totalsKnownOf(pages, hosts) || hasMoreOf(pages, hosts);
  return `${caption} / ${count} loaded${partial ? "; more can be loaded in Archive" : ""}`;
}

/** ArchiveBrowser::updateStatus. `valid`: the range's from is below its until. */
export function statusOf(pages: Pages, hosts: readonly PagingComputer[], valid: boolean): ArchiveStatus {
  let busy = false, more = false, newer = false, single = "", shown = 0;
  const errors: string[] = [], windows: string[] = [];
  for (const id of pageIds(pages)) {
    const p = pages[id];
    busy ||= p.request !== 0;
    more ||= !p.loaded || !!p.cursor;
    newer ||= !!p.previous;
    if (p.error) errors.push(`${machineOf(hosts, id)}: ${p.error}`);
    if (p.loaded) {
      single = p.position >= 0 && p.rows.length ? `${p.position + 1}–${p.position + p.rows.length} of ${p.total}` : `${p.rows.length} shown of ${p.total}`;
      windows.push(`${machineOf(hosts, id)}: ${single}`);
    }
    shown += p.rows.length;
  }
  const summary = busy ? tr("Loading archive…") : !hosts.length ? tr("Loaded sessions") : hosts.length === 1 && single ? single
    : totalsKnownOf(pages, hosts) ? `${shown} loaded of ${totalOf(pages)}` : `${shown} loaded`;
  const errorHint = errors.join("\n");
  return {
    summary, hint: windows.join("\n"), error: errorHint.slice(0, ERROR_CHARS), errorHint,
    older: { visible: hosts.length > 0 && (more || busy), enabled: valid && !busy },
    newer: { visible: newer, enabled: valid && !busy },
    refreshEnabled: !busy,
  };
}

/** The Older or Newer text: none below NARROW_PX (ArchiveBrowser::resizeEvent). */
export const buttonText = (width: number, text: "Older" | "Newer") => (width < NARROW_PX ? "" : text);

/** Archived rows take no local marks: the host's values as they are (Qt keeps them out of FleetState). */
const HOST_VALUES: Pick<RowMarks, "overlay"> = {
  overlay: (s) => ({ status: s.status, caption: s.caption, attention: s.attention, unread: s.unread, reviewLater: s.reviewLater, summary: s.summary, muted: s.muted, notificationConflict: s.notificationConflict }),
};

/** One archived row as a Session: keyed `archive:<archive id>` on its computer, named as the paging computer names it. */
export function archivedSession(computer: ComputerId, machine: string, row: ViewRow & Partial<Pick<ArchiveRow, "group" | "rank">>): ArchivedSession {
  const s = toSession(computer, `archive:${row.archive_id ?? ""}`, { ...row, machine: row.machine || machine }, HOST_VALUES);
  return Object.assign(s, { archivedAt: typeof row.archived_at === "number" ? row.archived_at : 0, group: row.group ?? "ungrouped", rank: row.rank ?? 0 });
}

/** Newest first; then by computer, name and archive id (Qt's entry key). */
function newestFirst(a: ArchivedSession, b: ArchivedSession): number {
  if (a.archivedAt !== b.archivedAt) return b.archivedAt - a.archivedAt;
  const x = `${a.computer}\n${a.name}\n${a.archiveId ?? ""}`, y = `${b.computer}\n${b.name}\n${b.archiveId ?? ""}`;
  return x < y ? -1 : x > y ? 1 : 0;
}

/** Configured fleet choices, named from rows or the hello metadata even with an empty catalog. */
export function fleetComputers(computers: Readonly<Record<string, ComputerState>>, rows: Iterable<Pick<Session, "computer" | "machine">>,
                               metadata: Readonly<Record<string, Partial<Pick<MachineMeta, "host" | "local_sessions">>>> = {}): PagingComputer[] {
  const ids = Object.keys(computers).filter((id) => computers[id]?.status !== "ended" && (id !== "" || metadata[id]?.local_sessions !== false)).sort();
  const names = new Map<string, string>();
  const wanted = new Set(ids);
  for (const row of rows) {
    if (wanted.has(row.computer) && !names.has(row.computer)) names.set(row.computer, row.machine);
    if (names.size === wanted.size) break;
  }
  return ids.map((computer) => ({ computer, machine: names.get(computer) || computer || metadata[computer]?.host || "this computer" }));
}

export function pagingComputers(computers: Readonly<Record<string, ComputerState>>, rows: Iterable<Pick<Session, "computer" | "machine">>, machines: readonly string[],
                                metadata: Readonly<Record<string, Partial<Pick<MachineMeta, "host" | "local_sessions">>>> = {}): PagingComputer[] {
  return fleetComputers(computers, rows, metadata).filter((h) => computers[h.computer]?.status === "live" && (!machines.length || machines.includes(h.machine)));
}

/** The machine, project and date scopes of the loaded archive window. */
export interface ListScope {
  machines: readonly string[];
  projects?: readonly string[];
  includes(archivedAt: number): boolean;
  compact(key: string): boolean;
  accepts?(s: Session): boolean;
}

export const inScope = (s: Pick<Session, "machine">, scope: Pick<ListScope, "machines">) =>
  !scope.machines.length || scope.machines.includes(s.machine);

/** Qt's archived grouping uses the archive identity, never the tag's project (SessionsWindowRows.cpp:266-300). */
export function archiveListRows(rows: readonly ArchivedSession[], scope: ListScope, organization: GroupingOut = emptyGrouping()): Row[] {
  const members = new Map<string, Session[]>();
  // The store supplies newest archived first (SessionsWindow.cpp:3238-3242).
  for (const s of rows) {
    const list = members.get(s.group);
    if (list) list.push(s); else members.set(s.group, [s]);
  }
  return groupRows({
    grouping: organization, members, projects: scope.projects ?? [],
    visible: (s) => scope.includes((s as ArchivedSession).archivedAt) && inScope(s, scope) && (scope.accepts?.(s) ?? true), online: () => false,
    expanded: () => false, compact: scope.compact, children: false,
  });
}

export interface ArchiveDeps {
  /**
   * One page of a computer's archive; `request` is the store's request
   * number (its token). Rejects with Qt's text, "superseded" or the hgs failure.
   */
  read(computer: ComputerId, from: number, until: number, cursor: string, query: string, request: number): Promise<ArchivePage>;
  /**
   * Ends every read numbered `through` or lower, also one not started yet
   * (Qt's cancelArchivePages); their answers are discarded by token anyway.
   */
  cancel(through: number): void;
  /** Clears the backend's retained identities as well as cancelling reads. */
  reset(through: number): void;
  /** Changing the project scope starts a new archive window. */
  projects?(): readonly string[];
  /** The Archive filter shows the list. */
  active(): boolean;
  /** The search text; the archive query is it trimmed. */
  query(): string;
  computers(): PagingComputer[];
  now?(): Date;
  /** Request numbers start after this; the app passes its start time, so a reload never reuses a cancelled number. */
  firstRequest?: number;
}

const sameHosts = (a: readonly PagingComputer[], b: readonly PagingComputer[]) =>
  a.length === b.length && a.every((h, i) => h.computer === b[i].computer && h.machine === b[i].machine);
const errorText = (error: unknown) => (typeof error === "string" ? error : error instanceof Error ? error.message : String(error));

/** The archive store; call it inside a reactive root. */
export function createArchive(deps: ArchiveDeps) {
  const now = deps.now ?? (() => new Date());
  const [pages, setPages] = createSignal<Pages>({});
  const [hosts, setHosts] = createSignal<readonly PagingComputer[]>([]);
  const [range, setRangeValue] = createSignal<DateRange>(initialRange(now()));
  /** Counts accepted pages (Qt's windowChanged: the list scrolls to the top). */
  const [windowChanges, setWindowChanges] = createSignal(0);
  let active = false;
  let query = "";
  let delay: ReturnType<typeof setTimeout> | undefined;
  /** The highest request number issued (Qt's m_archiveRequest). */
  let requests = deps.firstRequest ?? 0;
  // Only the current in-flight request per computer can hold deltas: at most
  // one page (50 identities), newest per key. Shown rows own their placements.
  const held = new Map<ComputerId, Map<string, Pick<GroupOut, "group" | "rank">>>();

  const bounds = () => rangeBounds(range(), now());
  const includes = (at: number) => { const [from, until] = bounds(); return at >= from && at < until; };
  const put = (computer: string, page: Page) => setPages((all) => ({ ...all, [computer]: page }));
  /** Drops every page; their answers in flight no longer match. */
  const dropPages = () => { held.clear(); setPages({}); };

  function applyGroups(groups: readonly GroupOut[]) {
    batch(() => {
      for (const g of groups) {
        if (g.archive_request === undefined) continue;
        const p = untrack(pages)[g.computer];
        if (!p) continue;
        if (g.archive_request === p.shownRequest && p.shownRequest !== 0) {
          const rows = p.rows.map((s) => s.catalogKey === g.key ? { ...s, group: g.group, rank: g.rank } : s);
          put(g.computer, { ...p, rows });
        } else if (g.archive_request === p.request && p.request !== 0) {
          let pending = held.get(g.computer);
          if (!pending) held.set(g.computer, pending = new Map());
          if (pending.has(g.key) || pending.size < 50) pending.set(g.key, { group: g.group, rank: g.rank });
        }
      }
    });
  }

  /** Older (false) or Newer (true) for every paging computer that has a page that way. */
  function navigate(newer: boolean) {
    untrack(() => {
      if (!active) return;
      const [from, until] = bounds();
      if (from >= until) return;
      const all = { ...pages() };
      const calls: [ComputerId, number, string][] = [];
      for (const { computer } of hosts()) {
        const p = all[computer] ?? emptyPage();
        all[computer] = p;
        const cursor = newer ? p.previous : p.cursor;
        if (p.request || (newer && !cursor) || (!newer && p.loaded && !cursor)) continue;
        const id = ++requests;
        held.delete(computer);
        all[computer] = { ...p, error: "", newer, request: id };
        calls.push([computer, id, cursor]);
      }
      setPages(all);
      for (const [computer, id, cursor] of calls) {
        let reply: Promise<ArchivePage>;
        try { reply = deps.read(computer, from, until, cursor, query, id); } catch (error) { reply = Promise.reject(error); }
        reply.then((page) => answer(computer, id, page), (error) => fail(computer, id, error));
      }
    });
  }

  function answer(computer: ComputerId, id: number, page: ArchivePage) {
    const p = untrack(pages)[computer];
    if (!p || p.request !== id) return;
    const machine = machineOf(untrack(hosts), computer);
    const pending = held.get(computer);
    held.delete(computer);
    // Rust computes the reply placement before retaining identities. A later
    // delta can arrive before this RPC reply; merge it after that snapshot.
    const rows = page.rows.map((row) => {
      const s = archivedSession(computer, machine, row);
      return Object.assign(s, pending?.get(s.catalogKey));
    });
    if (!rows.length && p.rows.length) { put(computer, { ...p, request: 0, error: tr(ARCHIVE_CHANGED) }); return; }
    batch(() => {
      put(computer, {
        ...p, request: 0, shownRequest: id, error: "", loaded: true, rows, cursor: page.next_cursor ?? "", previous: page.previous_cursor ?? "",
        total: page.total ?? 0, position: typeof page.position === "number" ? page.position : -1,
      });
      setWindowChanges((n) => n + 1);
    });
  }

  function fail(computer: ComputerId, id: number, error: unknown) {
    const p = untrack(pages)[computer];
    if (!p || p.request !== id) return;
    held.delete(computer);
    const text = errorText(error);
    put(computer, { ...p, request: 0, error: text === SUPERSEDED ? "" : text });
  }

  function refresh() {
    clearTimeout(delay);
    delay = undefined;
    deps.reset(requests);
    dropPages();
    navigate(false);
  }

  function setQuery(text: string) {
    if (text === query) return;
    query = text;
    deps.reset(requests);
    dropPages();
    clearTimeout(delay);
    delay = setTimeout(() => { delay = undefined; refresh(); }, QUERY_DELAY_MS);
  }

  /**
   * ArchiveBrowser::configure: the paging computers and whether the Archive
   * filter shows the list. A computer or a machine name that changed drops
   * every page, so no row keeps an old name.
   */
  function configure(next: readonly PagingComputer[], show: boolean) {
    const different = !sameHosts(next, untrack(hosts));
    batch(() => {
      if (!show && active) {
        deps.cancel(requests);
        held.clear();
        setPages((all) => Object.fromEntries(Object.entries(all).map(([id, p]) => [id, p.request ? { ...p, request: 0 } : p])));
      }
      active = show;
      if (different) {
        deps.reset(requests);
        setHosts(next);
        dropPages();
      }
    });
    const all = untrack(pages);
    const initial = next.some((h) => { const p = all[h.computer]; return !p || (!p.loaded && !p.request && !p.error); });
    if (active && delay === undefined && initial) navigate(false);
  }

  /** Apply in the date picker: a changed range reloads (ArchiveDatePicker::apply). */
  function setRange(next: DateRange) {
    const current = untrack(range);
    const changed = current.period !== next.period || (next.period === 4 && (current.first !== next.first || current.last !== next.last));
    setRangeValue(next);
    if (changed) refresh();
  }

  createEffect(on(() => deps.query().trim(), (text) => setQuery(text)));
  const scope = createMemo(() => deps.computers(), [], { equals: sameHosts });
  const showing = createMemo(() => deps.active());
  createEffect(on([scope, showing], ([next, show]) => configure(next, show)));

  const projects = createMemo(() => (deps.projects?.() ?? []).join("\n"));
  createEffect(on(projects, () => refresh(), { defer: true }));

  const rows = createMemo(() => {
    const all = pages();
    const list: ArchivedSession[] = [];
    for (const id in all) list.push(...all[id].rows);
    return list.sort(newestFirst);
  });
  const byKey = createMemo(() => new Map(rows().map((s) => [s.key, s])));
  const status = createMemo(() => { const [from, until] = bounds(); return statusOf(pages(), hosts(), from < until); });

  return {
    pages,
    applyGroups,
    /** Every loaded archived row, newest first. */
    rows,
    /** A loaded archived row by key. */
    row: (key: string): ArchivedSession | undefined => byKey().get(key),
    range,
    setRange,
    bounds,
    includes,
    refresh,
    older: () => navigate(false),
    newer: () => navigate(true),
    status,
    total: () => totalOf(pages()),
    totalsKnown: () => totalsKnownOf(pages(), hosts()),
    hasMore: () => hasMoreOf(pages(), hosts()),
    /** Some computer pages its archive. */
    paged: () => hosts().length > 0,
    countText: (count: number) => countText(count, pages(), hosts()),
    tipText: (caption: string, count: number) => tipText(caption, count, pages(), hosts()),
    window: windowChanges,
    computers: hosts,
  };
}

/** The live rows, in display order, for the machine names. */
function* catalogRows(): Iterable<Session> {
  for (const id of catalog.order) {
    const row = catalog.rows[id];
    if (row) yield row;
  }
}

/**
 * The app's archive: pages while the Archive filter shows the Sessions list,
 * the search text as the query, the live computers inside the machine filter.
 * `archiveRows` is the Archive filter's list (model.ts), `archiveCount` its
 * count chip and `archiveTip` the chip's tooltip; the selection and the
 * actions also find the loaded archived rows.
 */
export const { archive, archiveRows, archiveCount, archiveTip } = createRoot(() => {
  const archive = createArchive({
    read: (computer, from, until, cursor, query, request) => bridge.archivePage(computer, from, until, cursor, query, request),
    cancel: (through) => { void bridge.cancelArchivePages(through); },
    reset: (through) => { void bridge.resetArchivePages(through); },
    projects: projectFilter,
    firstRequest: Date.now(),
    active: () => stateFilter() === "archived" && page() === "sessions",
    query: search,
    computers: () => pagingComputers(catalog.computers, catalogRows(), machineFilter(), machineCards),
  });
  const archiveRows = createMemo(() => archiveListRows(archive.rows(), {
    machines: machineFilter(), projects: projectFilter(), includes: archive.includes, compact: (key) => !!compactRows[key],
    accepts: (s) => acceptsAccount(s, accountFilterSet()) && acceptsFolder(s, folderFilter()),
  }, grouping()));
  const counted = createMemo(() => {
    const scope = { machines: machineFilter() };
    const projects = projectFilter();
    return archive.rows().filter((s) => inScope(s, scope) && (!projects.length || projects.includes(s.group))
      && acceptsAccount(s, accountFilterSet()) && acceptsFolder(s, folderFilter())).length;
  });
  return { archive, archiveRows, archiveCount: () => archive.countText(counted()), archiveTip: () => archive.tipText("Archive", counted()) };
});

let retained: (key: string) => Session | undefined = () => undefined;
/**
 * Hands the selection the archived rows of pending restores (store/restores.ts),
 * which stay the selected row while the archive reloads (Qt's selected()).
 * The actions find loaded rows only.
 */
export function bindRetainedRows(lookup: (key: string) => Session | undefined) { retained = lookup; }

bindSelectedRows((key) => archive.row(key) ?? retained(key));
bindActionRows(archive.row);

bindArchivedGroups((key) => archive.row(key)?.group);
