import { batch, createEffect, createMemo, createRoot, createSignal, on, untrack } from "solid-js";
import { tr } from "../i18n";
import { createStore, produce, reconcile } from "solid-js/store";
import * as bridge from "../bridge";
import { catalog, computerFresh } from "./catalog";
import { errorText, notify } from "./actions";
import { groupOf, grouping, setProjectCollapsed } from "./organization";
import {
  machineFilter, projectFilter, accountFilter, folderFilter, search, selectedKey, setPage, setRevealing, setSelectedKey, setSubagentOpen, stateFilter,
} from "./ui";
import {
  CARD_LIMIT, NOTICE_LIMIT, RESOLVED, creationOfRow, creationRowId, footerText, newestFirst, noticeOf, placeCreations, shownCards, statusMenu,
  type CardScope, type ListScope, type Placement,
} from "../ui/sessions/creations-ui";
import type { CreationAction, CreationCard, CreationEvent, GroupingOut, LaunchPlan } from "../types";
import type { Row } from "../ui/sessions/model";
import { loadNewSession } from "../ui/sessions/newsession-view";
import type { Session } from "../ui/sessions/types";

// The creation cards: every admitted, unbound background creation as Qt's
// Sessions window shows it (SessionsWindow.cpp 463-511, 1509-1586,
// 2911-2919, 3020-3075, 3434-3442, 3717-3756; SessionsWindowRows.cpp
// 259-341, 376-397, 474-479). The host (src-tauri `creations.rs`) owns the
// operations; this store keeps their cards from `creations_view` and the
// `creations` event, the card selection, the reveal and the adopted session,
// and the error notices. Every action names its exact operation and is sent
// once; Check result only reads. A card never opens a composer or touches
// drafts: selecting one leaves no session selected, and the store sends only
// the `creation_*` commands.
//
// A card's selection is the list's selection, under the row key Qt gives a
// creation row ("#launch\n" + id, SessionsWindowRows.cpp:343, 546), so the
// list's keys and reveal treat a card as a row and a session selection
// replaces it.

/** What the store reads and drives; the app passes the bridge and the stores, tests their own. */
export interface CreationsDeps {
  bridge: Pick<typeof bridge, "creationsView" | "creationDetails" | "creationAction" | "creationPlan" | "onCreations">;
  notify(tone: "error" | "warn" | "info", text: string, sticky?: boolean): void;
  /** New session, loaded on demand (Edit choices…). */
  openNewSession(options: { agent: string; project: string; computer: string; account: string; recipe: Record<string, unknown> }): Promise<void>;
  /** The list's selected row key (store/ui.ts). */
  selectedKey(): string | undefined;
  setSelectedKey(key: string | undefined): void;
  /** Closes the open subagent. */
  closeSubagent(): void;
  /** Shows the Sessions page (showSessionList). */
  showSessions(): void;
  /** Scrolls the list to this row once it lists it, while it is selected (revealRow). */
  reveal(key: string): void;
  grouping(): GroupingOut;
  /** A session row's project (Qt's groupFor). */
  groupOf(key: string): string;
  setProjectCollapsed(project: string, collapsed: boolean): void;
  /** A listed session row. */
  session(key: string): Session | undefined;
  /** The listed session rows' keys in arrival order (a bound creation's session). */
  sessionKeys(): readonly string[];
  /** Whether the computer's rows are online, as Qt's binding requires. */
  online(computer: string): boolean;
  /** The list's filters, as `listRows` reads them. */
  scope(): CardScope;
  /** Changes when a filter or the search changes (385, 408, 416-419). */
  filterKey(): string;
}

/** A bound creation's exact session: an online, live row of its computer with the host's full bound name. */
function findSession(deps: CreationsDeps, want: { host: string; bound_session: string }): string | undefined {
  const keys = deps.sessionKeys();
  for (let i = keys.length - 1; i >= 0; i--) {
    const s = deps.session(keys[i]);
    if (s && s.computer === want.host && s.name === want.bound_session && s.state !== "archived" && deps.online(s.computer)) return s.key;
  }
  return undefined;
}

export function createCreations(deps: CreationsDeps) {
  const [cards, setCards] = createStore<Record<string, CreationCard>>({});
  /** Each card's change count, for its details. */
  const [versions, setVersions] = createStore<Record<string, number>>({});
  /** The cards' details while a search reads them. */
  const [details, setDetails] = createStore<Record<string, string>>({});
  const [revealed, setRevealed] = createSignal<string>();
  /** m_creationSessionToSelect: the adopted session's row key. */
  const [adopted, setAdopted] = createSignal<string>();
  /** A bound creation whose session is not listed yet: it is adopted once it is. */
  const [waiting, setWaiting] = createSignal<{ host: string; bound_session: string }>();
  const notices = new Map<string, string>();
  /** Show requests in flight: a hidden card's older answers do not end its reveal. */
  const showing = new Set<string>();
  /** Ids an event named before the first read answered; that read never overwrites them. */
  const early = new Set<string>();
  let loaded = false;
  /** The version each card's details were read for. */
  const read = new Map<string, number>();

  const creations = createMemo(() => Object.values(cards).sort(newestFirst));
  const selectedCreation = createMemo(() => creationOfRow(deps.selectedKey()));
  const selectedCard = () => { const id = selectedCreation(); return id === undefined ? undefined : cards[id]; };

  function notice(c: CreationCard) {
    const n = noticeOf(c);
    if (!n || notices.get(c.id) === n.key) return;
    notices.delete(c.id);
    notices.set(c.id, n.key);
    if (notices.size > NOTICE_LIMIT) notices.delete(notices.keys().next().value!);
    deps.notify("error", n.text, true);
  }

  /** Selects a card: the session selection and an open subagent end (3717-3728). */
  function select(id: string) {
    batch(() => {
      deps.setSelectedKey(creationRowId(id));
      deps.closeSubagent();
    });
  }

  function forget(id: string) {
    batch(() => {
      setCards(produce((s) => { delete s[id]; }));
      setVersions(produce((s) => { delete s[id]; }));
      setDetails(produce((s) => { delete s[id]; }));
      read.delete(id);
      if (revealed() === id) setRevealed(undefined);
      if (selectedCreation() === id) deps.setSelectedKey(undefined);
    });
  }

  /** The changed handler (1571-1586): a hidden card loses its reveal and selection; failures show once. */
  function changed(c: CreationCard) {
    batch(() => {
      setCards(c.id, cards[c.id] ? reconcile(c) : c);
      setVersions(c.id, (v) => (v ?? 0) + 1);
      const all = Object.values(cards);
      if (all.length > CARD_LIMIT) for (const old of all.sort(newestFirst).slice(CARD_LIMIT)) forget(old.id);
      if (!c.show_pending && !showing.has(c.id)) {
        if (revealed() === c.id) setRevealed(undefined);
        if (selectedCreation() === c.id) deps.setSelectedKey(undefined);
      }
    });
    notice(c);
  }

  /** Bound: its card goes; a selected one hands the selection to its exact session (3036-3039, 3253-3255). */
  function bound(c: CreationCard) {
    const selectedNow = selectedCreation() === c.id;
    forget(c.id);
    if (!selectedNow) return;
    setWaiting(undefined);
    if (!c.bound_session) return;
    const want = { host: c.host, bound_session: c.bound_session };
    const key = findSession(deps, want);
    if (key) adopt(key);
    else setWaiting(want);
  }

  function adopt(key: string) {
    batch(() => {
      setWaiting(undefined);
      setAdopted(key);
      deps.setSelectedKey(key);
    });
    deps.reveal(key);
  }

  function apply(e: CreationEvent) {
    if (!loaded) early.add(e.id);
    if ("removed" in e) { forget(e.id); return; }
    if (e.state === "bound") { bound(e); return; }
    changed(e);
  }

  /** Listens, then reads every card; events that came first win over the read. */
  async function start(): Promise<() => void> {
    const stop = await deps.bridge.onCreations(apply);
    let view: CreationCard[] = [];
    try { view = await deps.bridge.creationsView(); } catch { /* the browser build has no host */ }
    for (const c of view.slice(0, CARD_LIMIT)) if (!early.has(c.id) && !cards[c.id] && c.state !== "bound") changed(c);
    loaded = true;
    early.clear();
    return stop;
  }

  /** selectCreation (3717-3719): only an unbound card of this host. */
  function selectCreation(id: string) {
    const c = cards[id];
    if (!c || c.state === "bound") return;
    select(id);
  }

  /** The status menu's choice (473-477). */
  function choose(id: string) {
    const c = cards[id];
    if (!c || c.state === "bound") { deps.notify("info", tr(RESOLVED)); return; }
    batch(() => {
      setRevealed(id);
      setAdopted(undefined);
      setWaiting(undefined);
      select(id);
    });
    showing.add(id);
    void act(id, "show").finally(() => showing.delete(id));
    deps.showSessions();
    deps.reveal(creationRowId(id));
  }

  /** One action on the exact operation; a gone or bound creation is silent, while a lost call shows its text and is never sent again. */
  async function act(id: string, action: CreationAction): Promise<void> {
    try { await deps.bridge.creationAction(id, action); } catch (failure) {
      const text = errorText(failure);
      if (text === RESOLVED || !cards[id]) return;
      deps.notify("error", text, true);
    }
  }

  /** Edit choices… (1525-1528): New session with the operation as its recipe, while it can still be edited. */
  async function editChoices(id: string): Promise<void> {
    if (!cards[id]?.can_edit) return;
    let plan: Record<string, unknown> | null;
    try { plan = await deps.bridge.creationPlan(id); } catch (failure) { deps.notify("error", errorText(failure), true); return; }
    if (!plan) return;
    const text = (key: string) => (typeof plan![key] === "string" ? (plan![key] as string) : "");
    await deps.openNewSession({ agent: text("cmd"), project: text("group"), computer: text("host"), account: text("account"), recipe: plan });
  }

  /** Return to filters (1529-1532): the reveal and the adoption end. */
  function returnToFilters() {
    batch(() => { setRevealed(undefined); setAdopted(undefined); setWaiting(undefined); });
  }

  /**
   * An admitted creation (2914-2919): an editable predecessor hides, the
   * new card is selected and revealed, its project expands, and Sessions
   * shows it.
   */
  function admitted(id: string, plan: Pick<LaunchPlan, "group" | "predecessor">) {
    const predecessor = plan.predecessor ?? "";
    if (predecessor && cards[predecessor]?.can_edit) void act(predecessor, "hide");
    batch(() => { setRevealed(id); select(id); });
    if (deps.grouping().headers.find((h) => h.id === plan.group)?.collapsed) deps.setProjectCollapsed(plan.group, false);
    deps.showSessions();
    deps.reveal(creationRowId(id));
  }

  const adoptedRow = () => { const key = adopted(); return key === undefined ? undefined : deps.session(key); };
  const placement = (): Placement => ({ cards: creations(), revealed: revealed(), adopted: adoptedRow(), groupOf: deps.groupOf, details: (id) => details[id] });

  /** The list's rows with the creation rows (model.ts `listRows`). */
  const withCreations = (rows: Row[], scope: ListScope): Row[] => placeCreations(rows, scope, placement());

  const shownCount = createMemo(() => {
    const scope = deps.scope();
    if (scope.filter === "archived") return 0;
    const state = placement();
    const adoptedGroup = state.adopted && state.adopted.state !== "archived" ? deps.groupOf(state.adopted.key) : undefined;
    let n = 0;
    for (const list of shownCards(scope, { ...state, adoptedGroup }).values()) n += list.length;
    return n;
  });
  /** The footer's creations, after the sessions' text or alone. */
  const creationFooter = (afterSessions: boolean) => footerText(shownCount(), afterSessions);
  const creationStatusMenu = () => statusMenu(creations(), choose);

  // A session selection ends the adoption unless it is the adopted row; a card's selection closes the subagent (3441-3442, 3720-3721).
  createEffect(on(deps.selectedKey, (key) => {
    if (key === undefined) return;
    if (creationOfRow(key) !== undefined) {
      setWaiting(undefined);
      deps.closeSubagent();
      return;
    }
    batch(() => {
      setWaiting(undefined);
      if (key !== untrack(adopted)) setAdopted(undefined);
    });
  }, { defer: true }));

  // A filter or search change ends the reveal and the adoption (385, 408, 416-419).
  createEffect(on(deps.filterKey, () => batch(() => { setRevealed(undefined); setAdopted(undefined); setWaiting(undefined); }), { defer: true }));

  // A bound creation's session that was not listed yet is adopted once it is, unless another row was selected meanwhile.
  createEffect(() => {
    const want = waiting();
    if (!want) return;
    deps.sessionKeys().length;
    deps.online(want.host);
    const key = untrack(() => findSession(deps, want));
    if (key && untrack(deps.selectedKey) === undefined) adopt(key);
  });

  // Under a search, each card's details are read once per change (deviation 6); a new search reads them again.
  createEffect(on(() => deps.scope().query !== "", (searching) => {
    if (!searching) { read.clear(); return; }
  }));
  createEffect(() => {
    if (!deps.scope().query) return;
    for (const id of Object.keys(versions)) {
      const v = versions[id];
      if (read.get(id) === v) continue;
      read.set(id, v);
      deps.bridge.creationDetails(id).then((text) => { if (untrack(() => versions[id]) === v) setDetails(id, text ?? ""); }, () => {});
    }
  });

  return {
    creations, selectedCreation, selectedCard, revealed, adopted, cardVersion: (id: string) => versions[id] ?? 0,
    start, apply, selectCreation, choose, act, editChoices, returnToFilters, admitted, adopt,
    withCreations, creationCount: shownCount, creationFooter, creationStatusMenu,
    hideCreation: (id: string) => act(id, "hide"),
    details: (id: string) => deps.bridge.creationDetails(id),
  };
}

export type Creations = ReturnType<typeof createCreations>;

const app = createRoot(() => createCreations({
  bridge,
  notify: (tone, text, sticky) => notify(tone, text, sticky),
  // New session and its store load on demand, through the entry points' loader (App.tsx then mounts the dialog).
  openNewSession: (options) => loadNewSession().then((m) => m.openNewSession(options)),
  selectedKey,
  setSelectedKey,
  closeSubagent: () => setSubagentOpen(undefined),
  showSessions: () => setPage("sessions"),
  reveal: (key) => setRevealing(key),
  grouping,
  groupOf,
  setProjectCollapsed,
  session: (key) => catalog.rows[key],
  sessionKeys: () => catalog.order,
  online: computerFresh,
  scope: () => ({ grouping: grouping(), filter: stateFilter(), hosts: machineFilter(), query: search().trim(), projects: projectFilter(), accounts: accountFilter(), folder: folderFilter() }),
  filterKey: () => JSON.stringify([stateFilter(), machineFilter(), projectFilter(), accountFilter(), folderFilter(), search()]),
}));
// Without a listener the cards stay empty; nothing else depends on them.
void app.start().catch((error) => console.warn("Zerus Next: the creation cards could not start.", error));

/** Every unbound card, newest first. */
export const creations = app.creations;
/** The selected card's id (m_selectedCreation). */
export const selectedCreation = app.selectedCreation;
export const selectedCard = app.selectedCard;
export const cardVersion = app.cardVersion;
export const selectCreation = app.selectCreation;
/** The status menu (463-480). */
export const creationStatusMenu = app.creationStatusMenu;
/** ", 1 creation" after the sessions' text, or "1 creation" alone (474-479). */
export const creationFooter = app.creationFooter;
/** The shown creation rows, also when sessions are absent or a project is collapsed. */
export const creationCount = app.creationCount;
export const withCreations = app.withCreations;
export const runCreationAction = app.act;
export const hideCreation = app.hideCreation;
export const editChoices = app.editChoices;
export const returnToFilters = app.returnToFilters;
/** A verified creation or fork reveals one session without discarding the user's filters. */
export const adoptSession = app.adopt;
export const creationDetailsText = app.details;
/** New session's admission (newSession.ts calls it): reveal the new card. */
export const creationAdmitted = app.admitted;
