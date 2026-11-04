import { createEffect, createSignal } from "solid-js";
import * as bridge from "../bridge";
import type { ProcessesView, ProcessTarget } from "../types";
import type { Session } from "../ui/sessions/types";
import type { MenuEntry } from "./ui";
import { tr } from "../i18n";

// The Processes tab's view (Qt's ProcessView), held by the Rust hub
// (src-tauri processes.rs) and sent with the batches when it changes. The
// selection follows QTreeWidget's ExtendedSelection; the hub keeps it, the
// current row and the output, and answers with a new view. The groups'
// collapsed state, the keyboard focus and a context menu waiting for its
// selection are the tab's own.

/** The hub's calls; each names the view it was made in, so a changed view refuses it. */
export interface ProcessesPort {
  select(key: string, ids: string[], current: string | null): void;
  stop(key: string, ids: string[]): void;
}

/** Ctrl or Cmd toggles a row, Shift extends from the current row; a plain click selects one. */
export interface ClickMods { toggle: boolean; range: boolean }

/**
 * QTreeWidget's ExtendedSelection over the rows that show, in tree order: a
 * click selects one, Ctrl/Cmd toggles one, Shift selects the range from the
 * current row to the clicked one. The clicked row becomes current.
 */
export function nextSelection(order: readonly string[], selected: readonly string[], current: string | null, clicked: string, mods: ClickMods): { selected: string[]; current: string } {
  if (mods.range && current !== null && order.includes(current) && order.includes(clicked)) {
    const [from, to] = [order.indexOf(current), order.indexOf(clicked)].sort((a, b) => a - b);
    return { selected: order.slice(from, to + 1), current: clicked };
  }
  if (mods.toggle) {
    const next = selected.includes(clicked) ? selected.filter((id) => id !== clicked) : order.filter((id) => id === clicked || selected.includes(id));
    return { selected: next, current: clicked };
  }
  return { selected: [clicked], current: clicked };
}

/** One item of the tree as it shows: a group's header, or a process row of an expanded group. */
export type ProcessItem = { kind: "group"; id: string; group: string } | { kind: "row"; id: string; group: string };

/** A group header's item id; process ids never start with it. */
export const groupItem = (group: string) => `group:${group}`;

/** The tree's items in order: each group's header, then its rows unless the group is collapsed. */
export function visibleItems(view: ProcessesView, collapsed: Readonly<Record<string, boolean>>): ProcessItem[] {
  return view.groups.flatMap((g) => [
    { kind: "group" as const, id: groupItem(g.group), group: g.group },
    ...(collapsed[g.group] ? [] : g.rows.map((r) => ({ kind: "row" as const, id: r.id, group: g.group }))),
  ]);
}

/** The process rows that show, in tree order: what a Shift range can reach (Qt's QTreeView ranges cover visible rows). */
export const visibleRows = (view: ProcessesView, collapsed: Readonly<Record<string, boolean>>) =>
  visibleItems(view, collapsed).filter((i) => i.kind === "row").map((i) => i.id);

/**
 * The session the tab follows (SessionsWindow.cpp:3783-3784). `live` is Qt's
 * `!recorded`: online, not archived or paused, a process that has not exited
 * and no inspect error. `machine` is the row's own machine and `generation`
 * its details' `host_generation`.
 */
export function processTarget(s: Session, facts: { online: boolean; generation: string | null; inspectFailed: boolean }): ProcessTarget {
  const recorded = !facts.online || s.state === "archived" || s.state === "paused" || s.processState === "exited" || facts.inspectFailed;
  return {
    computer: s.computer, key: s.catalogKey, name: s.name, run_id: s.runId ?? "", conversation_id: s.conversationId ?? "",
    archive_id: s.archiveId ?? null, machine: s.rawMachine, online: facts.online, live: !recorded, generation: facts.generation,
  };
}

/** The session tab's Processes entry (SessionsWindow.cpp:1330, 3786-3787): shown while Settings > Processes is on, with the hub's count. */
export const processesTab = (enabled: boolean, count: number) =>
  ({ id: "processes" as const, label: count > 0 ? tr("Processes (%1)", count) : tr("Processes"), show: enabled });

/**
 * Follows the selected row while Processes is on, else nothing. Every change
 * of the row's facts sends the target again: the hub follows again after a
 * failed follow, even of the same target, and ignores an unchanged one.
 */
export function trackFollow(deps: {
  enabled: () => boolean;
  selected: () => Session | undefined;
  facts: (s: Session) => { online: boolean; generation: string | null; inspectFailed: boolean };
  follow: (target: ProcessTarget | null) => void;
}) {
  createEffect(() => {
    const s = deps.selected();
    if (!deps.enabled() || !s) { deps.follow(null); return; }
    deps.follow(processTarget(s, deps.facts(s)));
  });
}

/**
 * Tells the hub whether the tab shows: Processes on, a session selected, the
 * Sessions page with the Processes tab, and the window shown. The window's
 * visibility is its own (bridge.windowVisibility), as the Terminal reads it:
 * macOS fires visibilitychange on occlusion, which Qt ignores.
 */
export function trackShown(deps: { enabled: () => boolean; selected: () => boolean; page: () => string; tab: () => string; hidden: () => boolean; shown: (on: boolean) => void }) {
  createEffect(() => deps.shown(deps.enabled() && deps.selected() && deps.page() === "sessions" && deps.tab() === "processes" && !deps.hidden()));
}

/**
 * Whether the output pane keeps its end in view: while it was at its end
 * before the output changed, and always for a new current process, which
 * opens at its end as Qt's cleared output does.
 */
export function createOutputFollow() {
  let atEnd = true, shown: string | undefined;
  return {
    /** The current process shown; another one starts at its end. */
    process(id: string | undefined) { if (id !== shown) { shown = id; atEnd = true; } },
    /** The person scrolled: `end` says whether the pane now shows its end. */
    scrolled(end: boolean) { atEnd = end; },
    /** The output changed: true when the pane should go to its end. */
    stick: () => atEnd,
  };
}

/** A key on the tree, as the keyboard event gives it. */
export interface TreeKey { key: string; shift: boolean; toggle: boolean; alt?: boolean }

/** How long a right click on an unselected row waits for the hub to show that selection. */
const MENU_WAIT_MS = 1000;

export function createProcesses(port: ProcessesPort, now: () => number = () => Date.now()) {
  const [view, setView] = createSignal<ProcessesView | undefined>();
  // Qt's top-level items start expanded except Finished; a toggle lasts for the window's life.
  const [collapsed, setCollapsed] = createSignal<Record<string, boolean>>({ finished: true });
  const [focused, setFocused] = createSignal<string | undefined>();
  /** A context menu waiting for the hub to show its row selected alone, in the view it was asked in. */
  let pending: { key: string; id: string; until: number; open: (v: ProcessesView) => void } | undefined;

  const items = () => { const v = view(); return v ? visibleItems(v, collapsed()) : []; };
  /** The focused item: the one moved to while it shows, else the current row, else the first item. */
  const focus = () => {
    const all = items(), f = focused(), current = view()?.current;
    return all.find((i) => i.id === f) ?? all.find((i) => i.id === current) ?? all[0];
  };
  const select = (v: ProcessesView, id: string, mods: ClickMods) => {
    const next = nextSelection(visibleRows(v, collapsed()), v.selected, v.current, id, mods);
    port.select(v.key, next.selected, next.current);
  };
  const setGroup = (group: string, closed: boolean) => setCollapsed((c) => ({ ...c, [group]: closed }));
  const stop = () => {
    const v = view();
    if (v?.stop.enabled) port.stop(v.key, [...v.selected]);
  };

  return {
    view,
    collapsed,
    items,
    focus,
    apply(v: ProcessesView) {
      setView(v);
      const p = pending;
      if (!p) return;
      if (now() > p.until || v.key !== p.key) { pending = undefined; return; }
      if (v.selected.length === 1 && v.selected[0] === p.id) { pending = undefined; p.open(v); }
    },
    toggleGroup(group: string) { setGroup(group, !collapsed()[group]); },
    /** A row click; the hub answers with the new selection. */
    click(id: string, mods: ClickMods) {
      const v = view();
      if (!v) return;
      setFocused(id);
      select(v, id, mods);
    },
    /**
     * The row menu for `id` (ProcessView.cpp:63-75): at once on a selected
     * row; an unselected row is selected alone first, and the menu opens once
     * the hub shows that selection in the same view, or not at all.
     */
    requestMenu(id: string, open: (v: ProcessesView) => void) {
      const v = view();
      if (!v) return;
      setFocused(id);
      if (v.selected.includes(id)) { pending = undefined; open(v); return; }
      pending = { key: v.key, id, until: now() + MENU_WAIT_MS, open };
      port.select(v.key, [id], id);
    },
    /** Stop selected: the ids this view shows selected. */
    stop,
    /** A menu's Terminate: only while the view and its selection are the ones the menu opened on. */
    stopIfStill(key: string, ids: readonly string[]) {
      const v = view();
      if (v?.key === key && v.selected.length === ids.length && v.selected.every((id, i) => id === ids[i])) stop();
    },
    /**
     * The tree's keyboard, as QTreeView's ExtendedSelection works: Up, Down,
     * Home and End move (selecting the row alone; Shift extends from the
     * current row; Ctrl/Cmd moves only the current row), Space selects
     * (Ctrl/Cmd toggles), Left and Right collapse and expand a group or move
     * between a row and its group, and the Menu key or Shift+F10 asks for the
     * row's menu. True when the key was the tree's.
     */
    key(k: TreeKey, menu: (id: string) => void): boolean {
      const v = view(), all = items(), at = focus();
      // An Alt chord is the browser's (history, menus).
      if (!v || !at || k.alt) return false;
      const index = all.findIndex((i) => i.id === at.id);
      const moveTo = (target: ProcessItem | undefined) => {
        if (!target) return;
        setFocused(target.id);
        if (target.kind !== "row") return;
        if (k.toggle && !k.shift) port.select(v.key, [...v.selected], target.id);
        else select(v, target.id, { toggle: false, range: k.shift });
      };
      switch (k.key) {
        case "ArrowDown": moveTo(all[index + 1]); return true;
        case "ArrowUp": moveTo(all[index - 1]); return true;
        // Shift extends through the first or last row that shows, as Qt's tree does, even when a header is first or last.
        case "Home": moveTo(k.shift ? all.find((i) => i.kind === "row") : all[0]); return true;
        case "End": moveTo(k.shift ? all.findLast((i) => i.kind === "row") : all[all.length - 1]); return true;
        case " ":
          if (at.kind === "row") select(v, at.id, { toggle: k.toggle, range: false });
          return true;
        case "ArrowLeft":
          if (at.kind === "group") setGroup(at.group, true);
          else setFocused(groupItem(at.group));
          return true;
        case "ArrowRight":
          if (at.kind === "group") {
            if (collapsed()[at.group]) setGroup(at.group, false);
            else if (all[index + 1]?.kind === "row") setFocused(all[index + 1].id);
          }
          return true;
        case "ContextMenu":
          if (at.kind === "row") menu(at.id);
          return true;
        case "F10":
          if (!k.shift) return false;
          if (at.kind === "row") menu(at.id);
          return true;
        default:
          return false;
      }
    },
  };
}

export const processes = createProcesses({
  select: (key, ids, current) => bridge.processesSelect(key, ids, current),
  stop: (key, ids) => bridge.processesStop(key, ids),
});

/** Qt's backgroundIdle (SessionPresentation.h:120-123): idle with confirmed background jobs. */
const backgroundIdle = (s: Pick<Session, "activity" | "phase" | "backgroundJobs">) =>
  s.activity === "idle" && s.phase === "idle" && (s.backgroundJobs ?? 0) > 0;

/**
 * The session menu's Show running commands (SessionsWindow.cpp:5297-5299,
 * 5463-5465): listed for a session with background work, enabled while its
 * computer is online and Processes is on, and checked again when chosen.
 */
export function runningCommandsEntry(s: Pick<Session, "state" | "activity" | "phase" | "backgroundJobs">, online: () => boolean, enabled: () => boolean, open: () => void): Extract<MenuEntry, { label: string }> | undefined {
  if (s.state === "archived" || !backgroundIdle(s)) return undefined;
  return {
    label: tr("Show running commands"), icon: "activity", disabled: !(online() && enabled()),
    hint: enabled() ? tr("Follow this session's background work in Processes") : tr("Enable Settings > Processes to inspect running commands"),
    action: () => { if (online() && enabled()) open(); },
  };
}
