import type { MenuEntry, StateFilter } from "../../store/ui";
import { headerAttention, isWorking } from "../../store/attention";
import type { CreationCard, GroupingOut, HeaderOut } from "../../types";
import type { ListInput, Row } from "./model";
import type { Session } from "./types";
import { acceptsAccount, sameFolderForCreation, type FolderScope } from "./scope-filters";
import { trNoop, tr } from "../../i18n";

// The creation cards' view rules, which Qt keeps in its Sessions window
// (SessionsWindow.cpp 463-480, 3729-3756; SessionsWindowRows.cpp 259-341,
// 376-397, 474-479; SessionCardDelegate.h): their rows in the list, the
// progress view's buttons and texts, the status menu and the footer. The
// state lives in store/creations.ts; the captions and details come from the
// core. Pure: no store, no bridge.

const ROW = "#launch\n";
/** At most this many cards (the journal's 128 operations). */
export const CARD_LIMIT = 128;
/** The notice memory (1583). */
export const NOTICE_LIMIT = 128;
/** A heading before every further 50 entries of the status menu (471, D6). */
export const MENU_PAGE = 50;

export const PREPARING = trNoop("Preparing the session in the background");
export const HIDE_LAUNCH_STATUS = trNoop("Hide launch status");
export const OLDER = trNoop("Older creations…");
export const RESOLVED = trNoop("This creation has already been resolved.");
export const NO_PENDING = trNoop("No pending session creations");
export const PROGRESS = trNoop("Session creation progress");
export const BUTTON = {
  resume: trNoop("Continue setup"), check: trNoop("Check result"), stop: trNoop("Stop creating"), stopAfter: trNoop("Stop after preparation"),
  edit: trNoop("Edit choices…"), hide: trNoop("Hide status"), back: trNoop("Return to filters"),
} as const;

/** A creation's row key in the list. */
export const creationRowId = (id: string) => `${ROW}${id}`;
/** The creation a list key names, if it names one. */
export const creationOfRow = (key: string | undefined) => (key?.startsWith(ROW) ? key.slice(ROW.length) : undefined);

/** Newest first by `created`, then by id (the status menu's order, 467). */
export function newestFirst(a: CreationCard, b: CreationCard): number {
  if (a.created !== b.created) return b.created - a.created;
  return a.id < b.id ? -1 : a.id > b.id ? 1 : 0;
}

/** The project a card shows under: its own when the organization lists it, else the default (259-261, 320). */
export function projectOf(card: Pick<CreationCard, "group">, g: GroupingOut): string {
  return g.loaded && g.headers.some((h) => h.id === card.group) ? card.group : g.default_project;
}

/** The machine filter's name for a card's computer, as a session row of that computer names it (catalog `toSession`). */
export const machineOf = (c: Pick<CreationCard, "machine" | "host">) => c.machine || c.host || "this computer";
/** The computer a card shows: the alias, or this computer's host name (SessionsWindowRows.cpp:380). */
export const computerOf = (c: Pick<CreationCard, "machine" | "host">) => c.host || c.machine || "this computer";
/** The row's accessible text (390). */
export const accessibleText = (c: CreationCard) => `${c.tag}, ${computerOf(c)}, ${c.caption}`;

/** The row's detail line: the error, else "Preparing the session in the background" only before the first stage writes the error field (384). */
export function rowDetail(c: Pick<CreationCard, "error" | "caption" | "state">): string {
  const detail = c.error || (c.state === "saving" ? tr(PREPARING) : "");
  if (detail === c.caption) return "";
  return detail.startsWith(`${c.caption}: `) ? detail.slice(c.caption.length + 2) : detail;
}

export interface DetailsBox {
  value: string;
  scrollTop: number;
}

/** The progress details' request bookkeeping: only the newest answer for the card still shown renders, with its scroll kept. */
export function createDetailsView(box: DetailsBox) {
  let asked = 0, shown = 0;
  let current: string | undefined;
  return {
    read(id: string | undefined): (value: string | null) => void {
      if (id !== current) {
        current = id;
        shown = ++asked;
        box.value = "";
        box.scrollTop = 0;
      }
      if (id === undefined) return () => {};
      const mine = ++asked;
      return (value) => {
        if (mine < shown || current !== id || value == null) return;
        shown = mine;
        if (box.value === value) return;
        const top = box.scrollTop;
        box.value = value;
        box.scrollTop = top;
      };
    },
  };
}

export interface PanelButtons {
  resume: boolean;
  check: boolean;
  stop: boolean;
  stopEnabled: boolean;
  stopText: string;
  edit: boolean;
}

/** The progress view's buttons (renderCreation 3749-3755); Hide status and Return to filters always show. */
export function panelButtons(c: CreationCard): PanelButtons {
  const s = c.state;
  return {
    resume: c.can_resume,
    check: s === "unknown" || s === "recover" || s === "confirming",
    stop: !c.launch_dispatched && s !== "stopped" && s !== "failed" && s !== "storage_error",
    stopEnabled: !c.stop_requested,
    stopText: s === "cloning" || (s === "worktree" && !c.worktree_receipts) ? BUTTON.stopAfter : BUTTON.stop,
    edit: c.can_edit,
  };
}

/** The elapsed time `{m}:{ss}` since the creation (3741-3743). */
export function elapsed(c: Pick<CreationCard, "created">, now: number): string {
  const seconds = Math.max(0, Math.floor(now) - c.created);
  return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, "0")}`;
}

/** The line under the panel's title (D6: Qt's title joins the tag, the caption and the time with dots; the tag stays the title). */
export const panelStatus = (c: CreationCard, now: number) => `${c.caption} (${elapsed(c, now)})`;

/**
 * The status menu (463-480): one entry per unbound creation, newest first;
 * a heading "Older creations…" before every further 50 (Qt's submenu, D6);
 * "{tag}: {caption}" (D6). Empty, one disabled "No pending session creations".
 */
export function statusMenu(cards: readonly CreationCard[], choose: (id: string) => void): MenuEntry[] {
  const out: MenuEntry[] = [];
  cards.forEach((c, i) => {
    if (i && i % MENU_PAGE === 0) out.push({ heading: tr(OLDER) });
    out.push({ label: `${c.tag}: ${c.caption}`, action: () => choose(c.id) });
  });
  return out.length ? out : [{ label: tr(NO_PENDING), disabled: true }];
}

/** The footer's creations (474-479): after the sessions' text ", 1 creation", alone "1 creation"; "" without one. */
export function footerText(count: number, afterSessions: boolean): string {
  if (!count) return "";
  const text = count === 1 ? "1 creation" : `${count} creations`;
  return afterSessions ? `, ${text}` : text;
}

/** The sticky notice for a failed, unknown or unsaved creation (1579-1585), and the memory key it is shown once for. */
export function noticeOf(c: CreationCard): { key: string; text: string } | undefined {
  if (c.state !== "failed" && c.state !== "unknown" && c.state !== "storage_error") return undefined;
  return { key: `${c.state}\n${c.error}`, text: tr("Creation %1: %2\n%3", c.tag, c.caption, c.error) };
}

/** What the list's filters and organization are (model.ts `listRows`). */
export interface ListScope extends ListInput {
  accounts?: readonly string[];
  folder?: FolderScope;
  filter: StateFilter;
  /** The machine filter's names. */
  hosts: readonly string[];
  /** The search, trimmed. */
  query: string;
}
export type CardScope = Pick<ListScope, "grouping" | "filter" | "hosts" | "query" | "projects" | "accounts" | "folder">;

export interface Placement {
  /** Every unbound card. */
  cards: readonly CreationCard[];
  /** The revealed card (m_revealCreation). */
  revealed?: string;
  /** The adopted session's row (m_creationSessionToSelect), while it is listed. */
  adopted?: Session;
  /** A session row's project (Qt's groupFor). */
  groupOf(key: string): string;
  /** A card's details text, once read (search). */
  details(id: string): string | undefined;
}

/**
 * The cards the list shows, by project (SessionsWindowRows.cpp 300-322), in
 * Qt's order: the operations' ids (a QJsonObject walks its keys in order).
 * The revealed card shows through the state, machine and search filters; a
 * hidden card never shows. The project filter keeps the adopted session's
 * and the revealed card's projects. Under a search, a card shows when its
 * details contain the query, ignoring case (SessionSearch.cpp:287-298, D6).
 */
export function shownCards(scope: CardScope, state: Pick<Placement, "cards" | "revealed" | "details"> & { adoptedGroup?: string }): Map<string, CreationCard[]> {
  const out = new Map<string, CreationCard[]>();
  const revealed = state.revealed === undefined ? undefined : state.cards.find((c) => c.id === state.revealed);
  const revealedGroup = revealed?.show_pending ? projectOf(revealed, scope.grouping) : undefined;
  const allowed = (project: string) => !scope.projects.length || scope.projects.includes(project) || project === state.adoptedGroup || project === revealedGroup;
  const needle = scope.query.toLowerCase();
  const accounts = new Set(scope.accounts);
  const ordered = [...state.cards].sort((a, b) => (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
  for (const c of ordered) {
    if (!c.show_pending) continue;
    if (c.id !== state.revealed) {
      if (scope.filter !== "all" && scope.filter !== "active") continue;
      if (scope.hosts.length && !scope.hosts.includes(machineOf(c))) continue;
      if (!acceptsAccount({ computer: c.host, cmd: c.cmd, accountId: c.account }, accounts) || !sameFolderForCreation(c, scope.folder)) continue;
      if (needle && !(state.details(c.id) ?? "").toLowerCase().includes(needle)) continue;
    }
    const project = projectOf(c, scope.grouping);
    if (!allowed(project)) continue;
    const list = out.get(project);
    if (list) list.push(c);
    else out.set(project, [c]);
  }
  return out;
}

type HeaderRow = Extract<Row, { type: "header" }>;

/** A header for a project the sessions' rows left out: its counters copy `model.ts` `groupRows`' loop over every member (100-106; Qt 398-404). */
function headerRow(h: HeaderOut, all: readonly Session[], online: (s: Session) => boolean): HeaderRow {
  let attention = 0, unread = 0, working = 0;
  for (const s of all) {
    const up = online(s);
    if (!s.muted && headerAttention(s, up)) attention++;
    if (!s.muted && s.unread) unread++;
    if (isWorking(s, up)) working++;
  }
  return { type: "header", id: `h:${h.id}`, project: h.id, name: h.name, color: h.color, palette: h.palette, collapsed: h.collapsed, total: 0, attention, unread, working };
}

/** The adopted session's row (and its open subagents) among its project's rows, at its rank (`groupRows`' cards). */
function withSession(rows: readonly Row[], s: Session, members: readonly Session[], scope: Pick<ListInput, "expanded" | "compact">): Row[] {
  const rank = new Map(members.map((m, i) => [m.key, i]));
  const mine = rank.get(s.key) ?? Number.MAX_SAFE_INTEGER;
  let at = rows.findIndex((r) => r.type === "card" && (rank.get(r.id) ?? -1) > mine);
  if (at < 0) at = rows.length;
  const kids = s.subagents ?? [];
  const expanded = kids.length > 0 && scope.expanded(s.key);
  const add: Row[] = [{ type: "card", id: s.key, session: s, expandable: kids.length > 0, expanded, compactRow: scope.compact(s.key) }];
  if (expanded) kids.forEach((child, i) => add.push({ type: "child", id: `${s.key}/${child.id}`, parent: s, child, first: i === 0, last: i === kids.length - 1 }));
  return [...rows.slice(0, at), ...add, ...rows.slice(at)];
}

/**
 * The sessions' rows with the creation rows and the adopted session
 * (SessionsWindowRows.cpp 269, 300-341, 397). Each project's cards follow
 * its sessions; a project with only cards gets its header; a collapsed
 * project hides its cards, and its header's total counts them. The adopted
 * session shows through every filter. Without cards to show and an adopted
 * session to add, the rows come back unchanged. Linear in the rows.
 */
export function placeCreations(rows: Row[], scope: ListScope, state: Placement): Row[] {
  const adopted = state.adopted && state.adopted.state !== "archived" ? state.adopted : undefined;
  const adoptedGroup = adopted ? state.groupOf(adopted.key) : undefined;
  const shown = shownCards(scope, { ...state, adoptedGroup });
  const missing = adopted && !rows.some((r) => r.id === adopted.key) ? adopted : undefined;
  if (!shown.size && !missing) return rows;
  const cardRows = (list: readonly CreationCard[]): Row[] => list.map((c) => ({ type: "creation", id: creationRowId(c.id), creation: c }));
  const g = scope.grouping;
  const headers = g.loaded ? g.headers : [];
  if (headers.length <= 1) {
    // One project, or none presented yet: no header and nothing collapses (`groupRows`).
    const out = missing ? withSession(rows, missing, scope.members.get(adoptedGroup!) ?? [], scope) : [...rows];
    for (const list of shown.values()) out.push(...cardRows(list));
    return out;
  }
  const segments = new Map<string, { header: HeaderRow; rows: Row[] }>();
  let current: Row[] | undefined;
  for (const r of rows) {
    if (r.type === "header") segments.set(r.project, { header: r, rows: (current = []) });
    else current?.push(r);
  }
  const out: Row[] = [];
  for (const h of headers) {
    const segment = segments.get(h.id);
    const cards = shown.get(h.id) ?? [];
    const adds = missing && adoptedGroup === h.id ? missing : undefined;
    // Only a collapsed header can already count an adopted member whose card row is absent.
    const counted = h.collapsed && adds && scope.visible(adds) && (!scope.projects.length || scope.projects.includes(h.id));
    if (!segment && !cards.length && !adds) continue;
    const header = segment?.header ?? headerRow(h, scope.members.get(h.id) ?? [], scope.online);
    out.push({ ...header, total: header.total + cards.length + (adds && !counted ? 1 : 0) });
    if (h.collapsed) continue;
    const members = segment?.rows ?? [];
    out.push(...(adds ? withSession(members, adds, scope.members.get(h.id) ?? [], scope) : members), ...cardRows(cards));
  }
  return out;
}
