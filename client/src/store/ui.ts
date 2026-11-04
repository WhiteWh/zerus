import { createMemo, createSignal } from "solid-js";
import { createStore } from "solid-js/store";
import { catalog, computerFresh, orderedRows } from "./catalog";
import { documentVisibility } from "./visibility";
import { isActive, isWorking, needsAttention } from "./attention";
import { settings, settingsReady, setSetting, num } from "./settings";
import { createRowDensity, ROW_DENSITY_KEY } from "./rowDensity";
import type { Session, Status } from "../ui/sessions/types";
import type { FolderScope } from "../ui/sessions/scope-filters";
import { trNoop } from "../i18n";
export { isActive, needsAttention };

// Interface state of the first screen: page, selection, filters, list layout,
// menus and toasts. Data lives in store/catalog.ts and store/activity.ts.

export type MenuEntry =
  | { label: string; icon?: string; agent?: string; kbd?: string; danger?: boolean; disabled?: boolean; hint?: string; checked?: boolean | "mixed"; action?: () => void }
  | { separator: true }
  | { heading: string };

export type Page = "dashboard" | "sessions" | "projects" | "machines" | "accounts" | "settings";
export type StateFilter = "all" | "attention" | "active" | "paused" | "archived";
/** The Zerus mark's motion; the owner chose the two-turn spin-up (2026-10-27). */
export type LogoMotion = "spin";

const params = new URLSearchParams(location.search);

const PAGES: Page[] = ["dashboard", "sessions", "projects", "machines", "accounts", "settings"];
// `?page=settings` opens a screen (headless screenshots), as in the mockup.
export const [page, setPage] = createSignal<Page>(PAGES.find((p) => p === params.get("page")) ?? "sessions");
export const [selectedKey, setSelectedKey] = createSignal<string | undefined>();
export const [stateFilter, setStateFilterRaw] = createSignal<StateFilter>("all");
/** Qt clears an exact-folder scope when All sessions is chosen. */
export function setStateFilter(filter: StateFilter) { if (filter === "all") setFolderFilter(undefined); return setStateFilterRaw(filter); }
export const [machineFilter, setMachineFilter] = createSignal<string[]>([]);
export const [projectFilter, setProjectFilter] = createSignal<string[]>([]);
/** Machine-local binding keys, not display account ids. */
export const [accountFilter, setAccountFilter] = createSignal<string[]>([]);
export const accountFilterSet = createMemo(() => new Set(accountFilter()));
export const [folderFilter, setFolderFilter] = createSignal<FolderScope>();
export const [multi, setMulti] = createStore<Record<string, boolean>>({});
export const [peek, setPeek] = createSignal(false);
export type DetailTab = "activity" | "terminal" | "native" | "processes" | "taskview";
export const [detailTab, setDetailTab] = createSignal<DetailTab>("activity");
export const [subagentOpen, setSubagentOpen] = createSignal<string | undefined>();
export const [search, setSearch] = createSignal(params.get("q") ?? "");
export const [expandedChildren, setExpandedChildren] = createStore<Record<string, boolean>>({});
const rowDensity = createRowDensity({
  stored: () => settings[ROW_DENSITY_KEY], ready: settingsReady,
  save: (keys) => setSetting(ROW_DENSITY_KEY, keys),
  keys: (key) => (catalog.rows[key] ?? archivedRow(key))?.compactKeys,
});
export const compactRows = new Proxy<Record<string, boolean>>({}, {
  get: (_target, key) => typeof key === "string" ? rowDensity.get(key) : undefined,
});
export const setCompactRows = rowDensity.set;
export const expandCompactRows = rowDensity.expandAll;
export const hasCompactRows = rowDensity.any;
export const [stripMode, setStripMode] = createSignal(params.get("strip") === "1");
// sessionKey is a transient highlight, independent of conversation selection.
type Menu = { x: number; y: number; items: MenuEntry[]; sessionKey?: string };
// Keep the logical snapshot synchronous for guarded asynchronous menu reads.
// Only its painted layer waits for the native child to acknowledge hiding.
export const [menu, rememberMenu] = createSignal<Menu | undefined>();
export const [visibleMenu, publishMenu] = createSignal<Menu | undefined>();
let menuBarrier: ((scope: string, opening: boolean) => Promise<void> | undefined) | undefined;
let menuRevision = 0;
let menuPreparationTimer: ReturnType<typeof setTimeout> | undefined;
export const MENU_PREPARATION_MS = 180;
/** A native child must acknowledge hiding before an app menu can cover it. */
export function bindMenuBarrier(barrier: typeof menuBarrier) { menuBarrier = barrier; }
export const nativeOverlayBarrier = (scope: string, opening: boolean) => menuBarrier?.(scope, opening);
/** Open after preparation or its budget, retaining the native hide barrier.
 * The returned updater accepts partial replies only for this exact opening. */
export function setMenu(value: Menu | undefined, ready?: Promise<MenuEntry[]>) {
  clearTimeout(menuPreparationTimer);
  menuPreparationTimer = undefined;
  const revision = ++menuRevision;
  rememberMenu(value);
  const wait = nativeOverlayBarrier("menu", !!value);
  if (!value) { publishMenu(undefined); return; }
  let current = value, shown = false;
  // Late reads belong to this opening, even after the preparation budget.
  const update = (items: MenuEntry[]) => {
    if (revision !== menuRevision) return;
    current = { ...current, items };
    rememberMenu(current);
    if (shown) publishMenu(current);
  };
  const show = () => {
    if (revision !== menuRevision) return;
    shown = true;
    publishMenu(current);
  };
  if (!wait && !ready) { show(); return update; }
  // Bound only data preparation. Native-child hiding still requires its
  // actual acknowledgement before any menu can be painted over the child.
  if (ready) publishMenu(undefined);
  const preparation = ready && new Promise<void>((resolve) => {
    const timer = setTimeout(resolve, MENU_PREPARATION_MS);
    menuPreparationTimer = timer;
    void ready.then(update).catch(() => {}).finally(() => {
      clearTimeout(timer);
      if (menuPreparationTimer === timer) menuPreparationTimer = undefined;
      resolve();
    });
  });
  void Promise.all([wait, preparation]).then(show).catch(() => {});
  return update;
}
export const [contentScale, setContentScale] = createSignal(1);
/** A row the session list scrolls to once it lists it (Qt's `revealRow`); a new selection drops it. */
export const [revealing, setRevealing] = createSignal<string | undefined>();
export const revealRow = (id: string) => setRevealing(id);
export const logoMotion = (): LogoMotion => "spin";

/**
 * Seconds since the Unix epoch, ticking once a second: only Working clocks read
 * it. The clock stops while the window is hidden or minimised, as Qt stops its
 * elapsed timer in hideEvent, and catches up when the window shows again.
 */
export const [now, setNow] = createSignal(Date.now() / 1000);
let clock: ReturnType<typeof setInterval> | undefined;
const tick = () => setNow(Date.now() / 1000);
const syncClock = () => {
  if (documentVisibility.hidden()) { clearInterval(clock); clock = undefined; }
  else if (clock === undefined) { tick(); clock = setInterval(tick, 1000); }
};
syncClock();
documentVisibility.onChange(syncClock);

// Qt's docked splitter and Next's former localStorage choice now share Settings.
const [dragSessionsWidth, setDragSessionsWidth] = createSignal<number>();
export const sessionsWidth = () => dragSessionsWidth() ?? num("workspace/sessionsWidth");
export const setSessionsWidth = setDragSessionsWidth;
export function commitSessionsWidth() {
  const width = dragSessionsWidth();
  setDragSessionsWidth(undefined);
  if (width !== undefined) void setSetting("workspace/sessionsWidth", width);
}

/** Toasts go to the window's one notice bus (store/notices.ts). */
export { toast } from "./notices";

let archivedRow: (key: string) => Session | undefined = () => undefined;
let searchRow: (key: string) => Session | undefined = () => undefined;
/** The search keeps at most its one selected archived row, independently of archive pages. */
export function bindSearchRow(lookup: (key: string) => Session | undefined) { searchRow = lookup; }
/** Hands the selection the loaded archived rows (store/archive.ts), which this module cannot import: archive.ts reads the filters here. */
export function bindArchivedRows(lookup: (key: string) => Session | undefined) { archivedRow = lookup; }

export const selected = createMemo<Session | undefined>(() => {
  const key = selectedKey();
  return key ? catalog.rows[key] ?? archivedRow(key) ?? searchRow(key) : undefined;
});

/** The window's online state of a row: its computer's rows are current (Qt's `entry.online`). */
const online = (s: Session) => computerFresh(s.computer);

export function matchesFilter(s: Session, f: StateFilter): boolean {
  switch (f) {
    case "all": return true;
    case "attention": return needsAttention(s, online(s));
    case "active": return isActive(s, online(s));
    case "paused": return s.status === "paused";
    case "archived": return s.state === "archived";
  }
}

/**
 * The selected row whose reply was just read (Qt's m_readSelectionKey): it
 * stays listed under Needs attention and Active until another row is selected,
 * so reading it never replaces the open conversation. session.ts clears it on
 * every selection change.
 */
export const [readSelectionKey, setReadSelectionKey] = createSignal<string | undefined>();

/** The state filter with the open row kept after its read (SessionsWindowRows.cpp:272-275). */
export function listedByFilter(s: Session, f: StateFilter): boolean {
  if (matchesFilter(s, f)) return true;
  return (f === "attention" || f === "active") && s.state !== "archived" && s.key === selectedKey() && s.key === readSelectionKey();
}

export const counts = createMemo(() => {
  let all = 0, active = 0, attention = 0, working = 0, paused = 0;
  for (const s of orderedRows()) {
    all++;
    const up = online(s);
    if (isActive(s, up)) active++;
    if (needsAttention(s, up) && !s.muted) attention++;
    if (isWorking(s, up)) working++;
    if (s.status === "paused") paused++;
  }
  return { all, active, attention, working, paused };
});

export const STATUS_LABEL: Record<Status, string> = {
  attention: trNoop("Needs attention"),
  working: "Working",
  background: "Background",
  scheduled: "Scheduled",
  ready: "Ready",
  error: "Error",
  offline: "Offline",
  paused: "Paused",
  waiting: trNoop("Waiting for result"),
  starting: "Starting",
  ended: "Archived",
};
