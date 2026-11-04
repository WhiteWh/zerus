// Loaded only on a confirmed end action. Native timing and existing dispatchers
// own execution; these timers paint the countdown and never release an action.
import { createComputed, createRoot, createSignal } from "solid-js";
import { unwrap } from "solid-js/store";
import * as bridge from "../bridge";
import { bus, NOTICE_MS, sessionNoticeKey, type NoticeBus } from "./notices";
import { catalog } from "./catalog";
import { claimEnds, endKey, type EndCapture, type EndReference } from "./endActionBusy";
import { trNoop, tr } from "../i18n";

const FIVE = trNoop("Five actions are waiting. Undo one or wait a moment.");
/** The toast names at most this many sessions. */
const NAMED = 3;
const UNDO_MS = 3000;
type Titled = { computer: string; name: string; title: string };
/** The session list's label (its title, else its name) for the first captures; one pass, stops when all are found. */
export function endTitles(captures: readonly EndCapture[], rows: Iterable<Titled>): string[] {
  const shown = captures.slice(0, NAMED);
  const titles = shown.map((c) => c.target.name);
  const wanted = new Map<string, number[]>();
  shown.forEach((c, i) => { const key = endKey(c.target.computer, c.target.name); wanted.set(key, [...(wanted.get(key) ?? []), i]); });
  let left = wanted.size;
  for (const row of rows) {
    const at = wanted.get(endKey(row.computer, row.name));
    if (!at) continue;
    for (const i of at) titles[i] = row.title || row.name;
    wanted.delete(endKey(row.computer, row.name));
    if (--left === 0) break;
  }
  return titles;
}
export interface EndPort {
  begin(id: string, captures: EndCapture[]): Promise<void>;
  wait(id: string): Promise<boolean>;
  cancel(id: string): Promise<void>;
}
export interface EndOptions {
  key?: string;
  remove?: boolean;
  current?: () => boolean;
}
export function actionVerb(c: EndCapture) {
  return c.action.kind === "archive" ? "Archive" : ["terminate", "kill", "terminate_shell"].includes(c.action.kind) ? "Terminate" : c.action.kind === "clear_context" ? "Clear" : "Forget";
}
// One session, then several; the toast title and its cancel notices.
const ongoing = {
  Archive: [trNoop("Archiving session"), trNoop("Archiving %1 sessions")],
  Terminate: [trNoop("Terminating session"), trNoop("Terminating %1 sessions")],
  Clear: [trNoop("Clearing session"), trNoop("Clearing %1 sessions")],
  Forget: [trNoop("Forgetting session"), trNoop("Forgetting %1 sessions")],
};
// Cancelled by Undo, then because the session changed.
const cancelled = {
  Archive: [trNoop("Archive cancelled"), trNoop("Archive cancelled: the session changed.")],
  Terminate: [trNoop("Terminate cancelled"), trNoop("Terminate cancelled: the session changed.")],
  Clear: [trNoop("Clear cancelled"), trNoop("Clear cancelled: the session changed.")],
  Forget: [trNoop("Forget cancelled"), trNoop("Forget cancelled: the session changed.")],
};
export function createEndActions(port: EndPort, notices: NoticeBus, uuid: () => string = () => crypto.randomUUID(), now = () => performance.now(),
  titles: (captures: readonly EndCapture[]) => string[] = (captures) => captures.slice(0, NAMED).map((c) => c.target.name)) {
  const pending = new Map<string, { note(text: string): void; cancel(changed?: boolean, quiet?: boolean): void }>();
  let closed = false;
  async function schedule<T>(captures: EndCapture[], run: (refs: EndReference[], key: string) => Promise<T>, options: EndOptions = {}): Promise<T | undefined> {
    if (closed || !captures.length) return;
    if (pending.size >= 5) { [...pending.values()].at(-1)?.note(tr(FIVE)); return; }
    const release = claimEnds(captures);
    if (!release) return;
    const id = uuid(), verb = actionVerb(captures[0]);
    const key = options.key ?? (captures.length === 1 ? sessionNoticeKey(captures[0].target.computer, captures[0].target.name) : `end-batch:${id}`);
    const reportError = (error: unknown) => {
      const changed = String(error) === "The session state changed. Open its actions again.";
      notices.notify(changed ? "info" : "error", changed ? tr(cancelled[verb][1]) : String(error), !changed, key);
    };
    const [seconds, setSeconds] = createSignal(UNDO_MS / 1000);
    const [note, setNote] = createSignal("");
    let noticeId: number | undefined;
    let active = true, timer: ReturnType<typeof setInterval> | undefined, noteTimer: ReturnType<typeof setTimeout> | undefined, dispose: (() => void) | undefined;
    const finish = () => {
      active = false; clearInterval(timer); clearTimeout(noteTimer); dispose?.(); pending.delete(id); release(); if (noticeId !== undefined) notices.dismiss(noticeId);
    };
    const cancel = (changed = false, quiet = false) => {
      if (!active) return;
      finish();
      void port.cancel(id).catch(() => {});
      if (!quiet) notices.notify("info", tr(cancelled[verb][changed ? 1 : 0]), false, key);
    };
    pending.set(id, { cancel, note: (text) => { setNote(text); clearTimeout(noteTimer); noteTimer = setTimeout(() => setNote(""), NOTICE_MS); } });
    try {
      const start = now();
      await port.begin(id, captures);
      if (!active || closed) { await port.cancel(id).catch(() => {}); return; }
      // Single and batch toasts name sessions the way the session list does.
      const listed = titles(captures).join(", ");
      const names = captures.length > NAMED ? tr("%1 and %2 more", listed, captures.length - NAMED) : listed;
      noticeId = notices.notify("info", `${names}${options.remove ? `\n${tr("The working folder is removed afterwards.")}` : ""}`, true, key,
        captures.length === 1 ? tr(ongoing[verb][0]) : tr(ongoing[verb][1], captures.length), {
          dismissible: false, note,
          actions: [{ label: () => tr("Undo (%1)", seconds()), style: "primary", disabled: () => !active, run: () => cancel() }],
        });
      timer = setInterval(() => setSeconds(Math.max(1, Math.ceil((UNDO_MS - (now() - start)) / 1000))), 100);
      if (options.current) createRoot((d) => { dispose = d; createComputed(() => { if (!options.current!()) cancel(true); }); });
      const ready = await port.wait(id);
      if (!active) return;
      if (!ready || closed) { finish(); return; }
      if (options.current && !options.current()) { cancel(true); return; }
      finish(); // Release presentation busy only; native claims still exclude other writers.
      try { return await run(captures.map((_, index) => ({ id, index })), key); }
      catch (error) { reportError(error); }
      finally { await port.cancel(id).catch(() => {}); } // Release unused/refused batch members.
    } catch (error) {
      if (active) { finish(); await port.cancel(id).catch(() => {}); reportError(error); }
    }
  }
  function close() {
    closed = true;
    for (const item of [...pending.values()]) item.cancel(false, true);
  }
  return { schedule, close };
}
export const endActions = createEndActions({ begin: bridge.actionDelayBegin, wait: bridge.actionDelayWait, cancel: bridge.actionDelayCancel }, bus,
  undefined, undefined, (captures) => endTitles(captures, Object.values(unwrap(catalog.rows))));
if (typeof window !== "undefined") window.addEventListener("pagehide", () => endActions.close(), { once: true });
// Quit admission is closed in Rust before this event. It can never deliver an action.
void bridge.onExitFlush(async () => endActions.close()).catch(() => {});
