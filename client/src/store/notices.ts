import { createSignal } from "solid-js";

// The window's one notice bus: the design's toasts in the bottom-right corner
// (design/webui, state.ts toast()). Action results, copies, updates and
// refusals all show here, for the design's time, and never over the header.
// Every toast dismisses itself (zerus-yr3kl): errors after a longer time than
// passing notices, and hovering or focusing a toast holds its timer.

export interface Notice {
  id: number;
  tone: "error" | "warn" | "info";
  /** The toast's first line; action results carry only their text. */
  title?: string;
  text: string;
  /** Kept until dismissed for info and warn; errors always dismiss themselves. */
  sticky: boolean;
  /** An actual in-flight operation, not a timer-based progress estimate. */
  busy?: boolean;
  /** Notices with one key replace each other, as one status line does. */
  key?: string;
  actions?: NoticeAction[];
  dismissible?: boolean;
  /** A transient refusal on an existing Undo, without replacing its focused button. */
  note?: () => string;
}

export interface NoticeAction {
  label: string | (() => string);
  style: "ghost" | "primary";
  icon?: "play";
  disabled?: () => boolean;
  run(): void | Promise<void>;
}
export type NoticeOptions = Pick<Notice, "actions" | "dismissible" | "note" | "busy">;
export const sessionNoticeKey = (computer: string, name: string) => `session-end:${computer}\n${name}`;

/** At most this many show; the oldest non-sticky one goes first. */
export const MAX_NOTICES = 5;
/** The design's toast time. */
export const NOTICE_MS = 4200;
/** Errors stay longer for reading, then dismiss like every toast (zerus-yr3kl). */
export const ERROR_NOTICE_MS = 10_000;

export interface NoticeBus {
  notices: () => Notice[];
  notify(tone: Notice["tone"], text: string, sticky?: boolean, key?: string, title?: string, options?: NoticeOptions): number;
  dismiss(id: number): void;
  /** Holds or releases the auto-dismiss timer of a notice that still shows. */
  hold(id: number, held: boolean): void;
  /** Dismisses the notice with this key, if one shows. */
  clearNotice(key: string): void;
}

export function createNoticeBus(): NoticeBus {
  const [list, setList] = createSignal<Notice[]>([]);
  let nextId = 1;
  let waiting: Notice[] = [];
  interface NoticeTimer { handle: ReturnType<typeof setTimeout>; deadline: number; remaining: number; held: boolean }
  const timers = new Map<number, NoticeTimer>();
  function clear(id: number) { const timer = timers.get(id); if (timer) clearTimeout(timer.handle); timers.delete(id); }
  function schedule(id: number, ms: number) {
    clear(id);
    timers.set(id, { handle: setTimeout(() => dismiss(id), ms), deadline: Date.now() + ms, remaining: ms, held: false });
  }
  function hold(id: number, held: boolean) {
    const timer = timers.get(id);
    if (!timer || timer.held === held) return;
    if (held) {
      clearTimeout(timer.handle);
      timer.remaining = Math.max(0, timer.deadline - Date.now());
      timer.held = true;
    } else {
      timer.held = false;
      if (timer.remaining === 0) { dismiss(id); return; }
      timer.deadline = Date.now() + timer.remaining;
      timer.handle = setTimeout(() => dismiss(id), timer.remaining);
    }
  }
  function drain(all: Notice[]) {
    while (all.length < MAX_NOTICES && waiting.length) all.push(waiting.shift()!);
    return all;
  }

  function dismiss(id: number) { clear(id); waiting = waiting.filter((n) => n.id !== id); setList((all) => drain(all.filter((n) => n.id !== id))); }
  function clearNotice(key: string) {
    waiting = waiting.filter((n) => n.key !== key);
    setList((all) => {
      const kept = all.filter((n) => n.key !== key || n.dismissible === false);
      for (const n of all) if (!kept.includes(n)) clear(n.id);
      return drain(kept);
    });
  }

  /**
   * Adds a notice and returns its id. A notice with a key first takes the
   * place of the one with that key; a passing one without takes the place of
   * the same passing message, as Qt's one notice line shows a repeat once
   * (several sessions updated together). Over the cap, the oldest other
   * non-sticky one goes first, then the oldest other; the new one stays.
   */
  function notify(tone: Notice["tone"], text: string, sticky = false, key?: string, title?: string, options?: NoticeOptions): number {
    const id = nextId++;
    const replaced = (n: Notice) => key === undefined
      ? !sticky && n.key === undefined && !n.sticky && n.tone === tone && n.title === title && n.text === text
      : n.key === key && (n.dismissible !== false || options?.dismissible === false);
    const replacedIds: number[] = [];
    const keep = (n: Notice) => { if (replaced(n)) { replacedIds.push(n.id); return false; } return true; };
    waiting = waiting.filter(keep);
    setList((all) => {
      const next = [...all.filter(keep), { id, tone, title, text, sticky, key, ...options }];
      while (next.length > MAX_NOTICES) {
        // Never the notice being added: it is the last entry.
        const others = next.slice(0, -1);
        const loose = others.findIndex((n) => !n.sticky && n.dismissible !== false);
        const at = loose < 0 ? others.findIndex((n) => n.dismissible !== false) : loose;
        if (at < 0) { waiting.push(next.pop()!); if (waiting.length > MAX_NOTICES) waiting.shift(); }
        else next.splice(at, 1);
      }
      return next;
    });
    const ms = tone === "error" ? ERROR_NOTICE_MS : sticky ? 0 : NOTICE_MS;
    if (ms > 0) schedule(id, ms);
    for (const replacedId of replacedIds) clear(replacedId);
    return id;
  }

  return { notices: list, notify, dismiss, clearNotice, hold };
}

/** The window's bus, shared by the action store and every toast. */
export const bus = createNoticeBus();
export const notices = bus.notices;
export const dismissNotice = bus.dismiss;

/** A titled notice, as the design's toast; every toast dismisses itself, errors after ERROR_NOTICE_MS. */
export function toast(title: string, text: string, tone: Notice["tone"] = "info", sticky = false): number {
  return bus.notify(tone, text, sticky, undefined, title);
}
