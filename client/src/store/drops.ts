import { createSignal } from "solid-js";
import * as bridge from "../bridge";
import { computerFresh } from "./catalog";
import { composer } from "./composer";
import { notify } from "./actions";
import { terminal, VIEW } from "./terminal";
import { detailTab, selected, selectedKey, setDetailTab, subagentOpen, type DetailTab } from "./ui";
import { trNoop, tr } from "../i18n";

// Files dropped on the session's panel (Qt's SessionFileDrop,
// SessionFileDrop.h, wired at SessionsWindow.cpp:1595-1601): a Terminal that
// shows, connected to an online session, gets their paths pasted, after
// staging them on a peer (dropTerminalFiles, 2641-2659); otherwise the
// composer attaches them and the Activity tab shows (attachDroppedFiles,
// MessageComposer.cpp:961-967). The target a drag entered with must still be
// the target when it drops, every path must be a file, and the drop must land
// on the panel; while it is over the panel the overlay says what a drop does.

export type DropRoute = { kind: "terminal"; key: string } | { kind: "composer"; key: string } | { kind: "none" };

/** What decides a drop's target. */
export interface DropState {
  /** The selected row's id. */
  selected: string | undefined;
  /** Its computer is online (Qt's entry->online). */
  online: boolean;
  tab: DetailTab;
  /** A subagent's Activity shows: its own composer is the drop target. */
  subagent: boolean;
  terminal: { connected: boolean; generation: number };
  composer: { key: string | undefined; canAttach: boolean };
}

export const TERMINAL_HINT = trNoop("Drop files to insert their paths in Terminal");
export const COMPOSER_HINT = trNoop("Drop files to attach to this message");

/**
 * Qt's targets: the Terminal while it is the current tab, its session online
 * and its attach connected (terminalDropTarget, 2618-2622); on any other tab
 * the current main or child composer when it can attach files. Each target has
 * a key: the drop must go to the target the drag entered with.
 */
export function routeDrop(s: DropState): DropRoute {
  if (!s.selected || s.tab === "taskview") return { kind: "none" };
  if (s.tab === "terminal") {
    return s.online && s.terminal.connected && s.terminal.generation !== 0
      ? { kind: "terminal", key: `${s.selected}\n${s.terminal.generation}` } : { kind: "none" };
  }
  return s.composer.key !== undefined && s.composer.canAttach ? { kind: "composer", key: s.composer.key } : { kind: "none" };
}

/** What a drop does and reads; tests pass their own. */
export interface DropDeps {
  state: () => DropState;
  /** The point, in CSS pixels, is over the session's panel. */
  overPanel: (x: number, y: number) => boolean;
  /** Every path names a file (Qt's LocalFile::readable). */
  files: (paths: string[]) => Promise<boolean>;
  /** Qt's reveal: the composer's Activity tab, the main conversation. */
  reveal: () => void;
  attach: (paths: string[]) => Promise<void>;
  /** The peer a Terminal drop stages its files on, when the session is not this computer's. */
  peer: () => string | undefined;
  terminalDrop: (generation: number, paths: string[]) => Promise<string | null>;
  notify: (tone: "error" | "info", text: string, sticky: boolean) => void;
}

const routeKey = (r: DropRoute) => (r.kind === "none" ? undefined : r.key);

export function createDrops(deps: DropDeps) {
  const [hint, setHint] = createSignal<string | undefined>();
  /** The drag under way: the target it entered with and whether its paths are files. */
  let drag: { key: string | undefined; files: Promise<boolean>; ok?: boolean; at?: { x: number; y: number } } | undefined;

  /** The overlay while the drag stays over the panel with its entering target and only files. */
  function update() {
    const d = drag;
    if (!d || !d.at || d.ok !== true || !deps.overPanel(d.at.x, d.at.y)) { setHint(undefined); return; }
    const r = routeDrop(deps.state());
    setHint(r.kind !== "none" && r.key === d.key ? tr(r.kind === "terminal" ? TERMINAL_HINT : COMPOSER_HINT) : undefined);
  }

  return {
    hint,
    enter(paths: string[], x: number, y: number) {
      const files = paths.length ? deps.files(paths) : Promise.resolve(false);
      const d = { key: routeKey(routeDrop(deps.state())), files, at: { x, y } } as NonNullable<typeof drag>;
      drag = d;
      void files.then((ok) => { d.ok = ok; if (drag === d) update(); });
      update();
    },
    over(x: number, y: number) {
      if (!drag) return;
      drag.at = { x, y };
      update();
    },
    leave() { drag = undefined; setHint(undefined); },
    async drop(paths: string[], x: number, y: number) {
      const d = drag;
      drag = undefined;
      setHint(undefined);
      if (!d || !paths.length || !deps.overPanel(x, y)) return;
      const r = routeDrop(deps.state());
      if (r.kind === "none" || r.key !== d.key || !(await d.files)) return;
      if (r.kind === "composer") {
        deps.reveal();
        await deps.attach(paths);
        return;
      }
      const generation = Number(r.key.slice(r.key.lastIndexOf("\n") + 1));
      const peer = deps.peer();
      if (peer !== undefined) deps.notify("info", tr("Transferring files to %1…", peer), false);
      try {
        const notice = await deps.terminalDrop(generation, paths);
        if (notice) deps.notify("info", notice, false);
      } catch (error) {
        deps.notify("error", typeof error === "string" ? error : String(error), true);
      }
    },
  };
}

/** The app's drops: the selected session, its Terminal and its composer. */
export const drops = createDrops({
  state: () => {
    const s = selected();
    const v = composer.view();
    return {
      selected: selectedKey(), online: !!s && computerFresh(s.computer), tab: detailTab(), subagent: !!subagentOpen(),
      terminal: { connected: terminal.connected(), generation: terminal.generation() },
      composer: { key: v?.key, canAttach: composer.canAttach() },
    };
  },
  overPanel: (x, y) => {
    const panel = document.querySelector(".detail")?.getBoundingClientRect();
    return !!panel && x >= panel.left && x <= panel.right && y >= panel.top && y <= panel.bottom;
  },
  files: (paths) => bridge.droppedFiles(paths),
  reveal: () => { setDetailTab("activity"); },
  attach: (paths) => composer.attachDropped(paths),
  peer: () => { const s = selected(); return s && s.computer !== "" ? s.machine : undefined; },
  terminalDrop: (generation, paths) => {
    const s = selected();
    if (!s) return Promise.resolve(null);
    return bridge.terminalDrop(VIEW, generation, {
      computer: s.computer, name: s.name, machine: s.machine, run_id: s.runId ?? "", conversation_id: s.conversationId ?? "",
      tracked: s.tracked, created: s.created > 0 ? s.created : -1,
    }, paths);
  },
  notify: (tone, text, sticky) => { notify(tone, text, sticky, "file-drop"); },
});

/** Listens to the window's file drops; positions arrive in physical pixels. */
export function startDrops(): () => void {
  if (!bridge.inTauri()) return () => {};
  let stop: (() => void) | undefined;
  let stopped = false;
  void import("@tauri-apps/api/webview").then(({ getCurrentWebview }) => getCurrentWebview().onDragDropEvent((event) => {
    const p = event.payload;
    if (p.type === "leave") { drops.leave(); return; }
    const x = p.position.x / devicePixelRatio, y = p.position.y / devicePixelRatio;
    if (p.type === "enter") drops.enter(p.paths, x, y);
    else if (p.type === "over") drops.over(x, y);
    else void drops.drop(p.paths, x, y);
  })).then((unlisten) => { if (stopped) unlisten(); else stop = unlisten; });
  return () => { stopped = true; stop?.(); };
}
