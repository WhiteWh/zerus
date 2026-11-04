import { pendingEnd, isEndAction, captureEnd, sameEnd, type EndReference } from "./endActionBusy";
import { createSignal, untrack } from "solid-js";
import { createStore } from "solid-js/store";
import * as bridge from "../bridge";
import { catalog as appCatalog, computerOffline, type CatalogState } from "./catalog";
import { marks as appMarks, type Marks } from "./marks";
import { activityReadOnly } from "./activitySource";
import { bus as appBus, createNoticeBus, sessionNoticeKey, type NoticeBus } from "./notices";
import { copyState, sessionIdentity, detailsFromInspection, MUTE_PENDING, STATE_CHANGED, forgetsBindingOnly, notificationsStillAllowed, snapshotOf, terminates, type ActionRow, type ChildIdentity, type ForgetScope, type PendingNotifications } from "../ui/sessions/actions-ui";
import type { Action, Details, Outcome, Target } from "../types";
import type { Session } from "../ui/sessions/types";
import { tr, trNoop } from "../i18n";

// Guarded session actions. Every action is one call to the Rust side; a call
// is never repeated. A rejected call is an unknown outcome and is shown as a
// failure. Read marks are local first (store/marks.ts) and the Rust worker
// delivers them; this store never repeats one.
//
// Writes and interrupts run in separate lanes (as the Rust in-flight guard
// does): one write per session shows "Updating…" on its entries; Stop waits
// only for its own interrupt, as Qt's m_interruptRequest. Shared notification
// changes are tracked apart too. There is no read lane here: the Rust worker
// claims the Read lane when it delivers a mark.

export type { Notice } from "./notices";

/** The followed session's Activity `state` value, and whether its subscription has answered. */
export interface Followed { value: Record<string, unknown> | undefined; settled: boolean }

const CHANGED = trNoop("This session changed. Open its actions again.");
const MUTE_CHANGED = trNoop("The shared notification setting changed while this request was pending. Refresh the session.");
export const GONE = trNoop("This session is no longer available. Open its actions again.");
const ALREADY_STOPPED = ["hgs: This turn is no longer working", trNoop("This turn is no longer working")];

const sessionId = (computer: string, name: string) => `${computer}\n${name}`;
const failed = (detail: string): Outcome => ({ outcome: "failed", detail });
/** The text of a rejected call: the string itself, an Error's message, else its String(). */
export const errorText = (error: unknown) => (typeof error === "string" ? error : error instanceof Error ? error.message : String(error));

/**
 * The target of an action. Interrupt, mark read and terminate act on one
 * identified run (and interrupt and mark read on its conversation): without
 * it they have no target. The other actions name the session only.
 */
export function targetOf(s: Pick<ActionRow, "computer" | "name" | "runId" | "conversationId">, kind: Action["kind"]): Target | undefined {
  const identity = kind === "interrupt" || kind === "mark_read" || kind === "terminate" || kind === "forget_binding";
  if (identity && (!s.runId || (kind !== "terminate" && !s.conversationId))) return undefined;
  return { computer: s.computer, name: s.name, run_id: s.runId ?? "", conversation_id: s.conversationId ?? "" };
}

/** The selected row and the open subagent, which decide whose details a copy reads. */
export interface Selection { key?: string; subagent?: string }

export interface PerformOptions { quiet?: boolean; cleanupId?: string; delay?: EndReference; skipDelay?: boolean; noticeKey?: string }
export type EndDriver = (row: ActionRow, action: Action, execute: (reference: EndReference, key: string) => Promise<Outcome>, current: () => boolean, remove: boolean) => Promise<Outcome | undefined>;

export function createActions(
  catalog: Pick<CatalogState, "rows">,
  online: (computer: string) => boolean = () => true,
  selection: () => Selection = () => ({}),
  notificationMarks: Pick<Marks, "currentNotificationRow" | "applyNotifications" | "notificationRevisionOf"> = appMarks,
  /** Where notices show: the window's bus for the app, a private one in tests. */
  bus: NoticeBus = createNoticeBus(),
  cleanupReport?: (out: Extract<Outcome, { outcome: "done" }>, row: ActionRow) => void,
  delayEnd?: EndDriver,
) {
  /** The write in flight per session (its kind), and whether an interrupt is. */
  const [writes, setWrites] = createStore<Record<string, Action["kind"] | undefined>>({});
  const [interrupts, setInterrupts] = createStore<Record<string, boolean>>({});
  const { notices: list, notify, dismiss, clearNotice } = bus;

  const updating = (computer: string, name: string) => !!writes[sessionId(computer, name)] || (!!delayEnd && pendingEnd(computer, name));
  const interrupting = (computer: string, name: string) => !!interrupts[sessionId(computer, name)];

  function report(action: Action, out: Outcome, row: ActionRow, cleanupHandled = false, key?: string) {
    const notify: NoticeBus["notify"] = (tone, text, sticky, _key, title, options) => bus.notify(tone, text, sticky, key ?? _key, title, options);
    if (action.kind === "interrupt") {
      // Qt's Stop notices (SessionsWindow.cpp 1206-1211).
      const target = `${row.name} (${row.machine})`;
      if (out.outcome === "done") notify("info", tr("Interrupt sent to %1. Waiting for the agent to stop…", target));
      else if (ALREADY_STOPPED.includes(out.detail.trim())) notify("info", tr("Stop %1: the turn already ended or is waiting for input. No action needed.", target));
      else notify("error", tr("Stop %1 failed: %2\nOpen Terminal to check the agent before trying again.", target, out.detail), true);
      return;
    }
    if (key && out.outcome === "failed" && out.detail === STATE_CHANGED && isEndAction(action)) {
      const verb = action.kind === "archive" ? "Archive" : ["terminate", "kill", "terminate_shell"].includes(action.kind) ? "Terminate" : "Forget";
      notify("info", `${verb} cancelled: the session changed.`); return;
    }
    if (out.outcome === "failed") { notify("error", out.detail, true); return; }
    if (action.kind === "forget_binding") { notify("info", tr("Zerus forgot %1. The conversation stays in OpenCode.", row.name)); return; }
    if (!cleanupHandled && out.outcome === "done" && out.cleanup && ["kept", "unconfirmed"].includes(out.cleanup.status)) {
      reportCleanup(out, row); return;
    }
    if (action.kind === "rename") notify("info", tr("Session renamed. Refreshing…"));
    else notify("info", tr("Session updated. Refreshing…"));
    if (!cleanupHandled) reportCleanup(out, row);
  }

  /** The worktree lines of an Archive or Terminate (Qt's worktreeCleanupFinished), shown also during a selection batch. */
  function reportCleanup(out: Outcome, row: ActionRow) {
    if (out.outcome !== "done" || !out.cleanup) return;
    const cleanup = out.cleanup;
    if (cleanup.status === "removed") notify("info", tr("Worktree removed automatically. The branch and archived conversation are kept."));
    else if (cleanup.status === "kept" || cleanup.status === "unconfirmed") {
      if (cleanupReport) cleanupReport(out, row);
      else notify("warn", tr("Working folder kept for review."), true, sessionNoticeKey(row.computer, row.name), tr("Session archived"));
    }
  }

  /** One guarded call in its lane; a session with a call in that lane gets none. */
  async function call(target: Target, action: Action, cleanupId?: string, delay?: EndReference): Promise<{ out: Outcome; busy: boolean }> {
    const id = sessionId(target.computer, target.name);
    const interrupt = action.kind === "interrupt";
    if (interrupt ? interrupts[id] : writes[id]) return { out: failed("busy"), busy: true };
    if (interrupt) setInterrupts(id, true); else setWrites(id, action.kind);
    try {
      return { out: await (cleanupId ? (delay ? bridge.terminationCleanupConfirm(cleanupId, delay) : bridge.terminationCleanupConfirm(cleanupId)) : (delay ? bridge.runAction(target, action, delay) : bridge.runAction(target, action))), busy: false };
    } catch (error) {
      const detail = typeof error === "string" ? error : error instanceof Error ? error.message : String(error);
      return { out: failed(detail), busy: detail === "busy" };
    } finally {
      if (interrupt) setInterrupts(id, false); else setWrites(id, undefined);
    }
  }

  /**
   * Runs an action from a menu, header button or dialog. `row` is what the
   * entry was built from; the action runs only while the listed row still
   * has that run (or, without a run id, still exists). A busy session is a
   * silent no-op. `quiet` is a selection batch's item (Qt's finishSelectionAction):
   * the batch words the outcome, and only the worktree lines show.
   */
  async function perform(row: ActionRow, action: Action, options: PerformOptions = {}): Promise<Outcome> {
    const live = catalog.rows[row.key];
    const target = targetOf(row, action.kind);
    if (!live || !target || (row.runId && live.runId !== row.runId)) {
      if (!options.quiet) notify("info", tr(CHANGED));
      return failed(tr(CHANGED));
    }
    if (delayEnd && isEndAction(action) && !options.delay && !options.skipDelay) {
      const captured = snapshotOf(row);
      const out = await delayEnd(captured, action, (delay, noticeKey) => perform(captured, action, { ...options, delay, noticeKey }), () => sameEnd(captured, catalog.rows[captured.key]), !!options.cleanupId);
      return out ?? failed(tr("Action cancelled."));
    }
    if (!options.delay && action.kind !== "interrupt" && updating(target.computer, target.name)) return failed("busy");
    const { out, busy } = await call(target, action, options.cleanupId, options.delay);
    if (!busy) { if (options.quiet) reportCleanup(out, row); else report(action, out, row, !!options.cleanupId, options.noticeKey); }
    return out;
  }

  /**
   * A computer-wide call (Pause all, Resume all): one per computer at a time,
   * no session named. Qt's writeDone words the outcome; nothing is retried.
   */
  async function performComputer(computer: string, action: Extract<Action, { kind: "pause_all" | "resume_all" }>): Promise<Outcome> {
    const { out, busy } = await call({ computer, name: "", run_id: "", conversation_id: "" }, action);
    if (busy) return out;
    if (out.outcome === "failed") notify("error", out.detail, true);
    else notify("info", tr("Session updated. Refreshing…"));
    return out;
  }

  /**
   * The Terminate confirmation's click: the session must still terminate
   * (a live session that is not an OpenCode binding) as when the dialog
   * opened; then `action` is the call the dialog chose. A computer that went
   * offline meanwhile gets no call and no notice, as in Qt.
   */
  async function end(row: ActionRow, action: "terminate" | "kill" | Extract<Action, { kind: "terminate_shell" }>, cleanupId?: string): Promise<Outcome> {
    if (!online(row.computer)) return failed("offline");
    const live = catalog.rows[row.key];
    if (live && terminates(live) !== terminates(row)) {
      notify("error", tr(STATE_CHANGED), true);
      return failed(STATE_CHANGED);
    }
    return perform(row, typeof action === "string" ? { kind: action } : action, { cleanupId: action === "terminate" ? cleanupId : undefined });
  }

  /**
   * The Forget saved session confirmation's click (terminateSession,
   * 5799-5818). The row must be as when the dialog opened: running-and-not-a-
   * binding and binding-only both unchanged, and a scoped forget also the same
   * run and conversation (the selected row's details win over the row);
   * otherwise nothing runs and the sticky notice says so. A scoped forget
   * (a binding with a run) is `forget_binding`, any other is `kill`.
   * An offline computer ends silently, as in Qt; a vanished row raises the same notice.
   */
  async function forget(row: ActionRow, opened: ForgetScope): Promise<Outcome> {
    if (!online(row.computer)) return failed("offline");
    const live = catalog.rows[row.key];
    if (!live) {
      // Rows are keyed by run, so a vanished row may be the same session on a new run; Qt refuses a scoped forget then.
      notify("error", tr(STATE_CHANGED), true);
      return failed(STATE_CHANGED);
    }
    const now = selectedDetails(live);
    const binding = forgetsBindingOnly(live, now);
    if ((live.state === "running" && !binding) || binding !== opened.binding) {
      notify("error", tr(STATE_CHANGED), true);
      return failed(STATE_CHANGED);
    }
    if (opened.binding && opened.run) {
      const run = now ? now.run_id : live.runId ?? "";
      const conversation = now ? now.conversation_id : live.conversationId ?? "";
      if (run !== opened.run || conversation !== opened.conversation) {
        notify("error", tr(STATE_CHANGED), true);
        return failed(STATE_CHANGED);
      }
      return perform({ ...row, runId: run, conversationId: conversation }, { kind: "forget_binding" });
    }
    return perform(row, { kind: "forget" });
  }

  /**
   * Changing the state from a menu: the session must still be running, or
   * still not running, as when the menu opened (Qt's runSessionMenuAction).
   */
  async function change(row: ActionRow): Promise<Outcome> {
    const live = catalog.rows[row.key];
    if (live && (live.state === "running") !== (row.state === "running")) {
      notify("error", tr(STATE_CHANGED), true);
      return failed(STATE_CHANGED);
    }
    return perform(row, { kind: row.state === "running" ? "pause" : "resume" });
  }

  // Shared notification changes (Qt's m_pendingNotifications): tracked apart
  // from the writes, one per conversation of a computer and sixteen in all.
  const [changing, setChanging] = createSignal<{ id: number; computer: string; conversation: string }[]>([]);
  let nextChange = 1;

  /** What the pending notification changes block for a row (reactive). */
  const pendingNotifications = (computer: string, conversationId: string | undefined): PendingNotifications => ({
    conversation: changing().some((p) => p.computer === computer && p.conversation === (conversationId ?? "")),
    total: changing().length,
  });

  /**
   * Qt's click-time recheck (runSessionMenuAction), run first by every action
   * a menu entry starts: the row is looked up again by key; one that is gone,
   * whose `created` changed (when it had one) or whose conversation changed
   * raises the sticky notice and returns undefined, so nothing else happens.
   * Otherwise the live row.
   */
  function stillListed(row: ActionRow): ActionRow | undefined {
    const live = catalog.rows[row.key];
    if (!live || ((row.created ?? 0) > 0 && live.created !== row.created) || (live.conversationId ?? "") !== (row.conversationId ?? "")) {
      notify("info", tr(GONE), true);
      return undefined;
    }
    return live;
  }

  /**
   * Mute or Unmute (SessionsWindow runSessionMenuAction ToggleMute and the
   * notificationsFinished result). The same checks run again on the live row,
   * and the preferences must be unchanged since the menu opened; otherwise
   * nothing happens. The call is never repeated. A late answer for a row that
   * is no longer the same run, conversation and `created` is dropped.
   */
  async function toggleMute(row: ActionRow): Promise<void> {
    const live = stillListed(row);
    if (!live) return;
    const original = snapshotOf(live);
    if (!notificationsStillAllowed(original, row, online(live.computer), untrack(() => pendingNotifications(live.computer, live.conversationId)))) return;
    // The row's revision, or a higher confirmed override's (an Unmute before the next listing).
    const expected = notificationMarks.notificationRevisionOf(original);
    if (expected === undefined) return;
    const target: Target = { computer: original.computer, name: original.name, run_id: original.runId ?? "", conversation_id: original.conversationId ?? "" };
    const entry = { id: nextChange++, computer: original.computer, conversation: original.conversationId ?? "" };
    setChanging((all) => [...all, entry]);
    let preferences: Record<string, unknown> | undefined;
    let detail = "";
    try { preferences = await bridge.setNotifications(target, !original.muted, expected); } catch (error) { detail = errorText(error); if (detail === "busy") detail = MUTE_PENDING; } finally {
      setChanging((all) => all.filter((p) => p.id !== entry.id));
    }
    if (!notificationMarks.currentNotificationRow(original)) return;
    if (!preferences) { notify("error", detail, true); return; }
    if (!notificationMarks.applyNotifications(original, preferences)) notify("error", tr(MUTE_CHANGED), true);
  }

  // Details: the followed session's Activity state value when it names the
  // row's run and conversation; otherwise one inspect per session at a time,
  // where the signature is what made the last fetch and a different one
  // fetches again once the running one ends.
  const [fetched, setFetched] = createStore<Record<string, { sig: string; value: Details | null }>>({});
  const inflight = new Map<string, string>();
  const wanted = new Map<string, { sig: string; row: ActionRow }>();

  const signature = (s: ActionRow) => `${s.runId ?? ""}\n${s.conversationId ?? ""}\n${s.state}`;

  function fetchDetails(id: string, s: ActionRow) {
    const sig = signature(s);
    inflight.set(id, sig);
    const target: Target = { computer: s.computer, name: s.name, run_id: s.runId ?? "", conversation_id: s.conversationId ?? "" };
    bridge.inspectSession(target, s.archiveId ?? null).then(
      (value) => setFetched(id, { sig, value }),
      () => setFetched(id, { sig, value: null }),
    ).finally(() => {
      inflight.delete(id);
      const next = wanted.get(id);
      wanted.delete(id);
      if (next && next.sig !== sig) fetchDetails(id, next.row);
    });
  }

  /**
   * The selected session's details. `followed` is its Activity subscription:
   * a state value of this run wins; the separate inspect runs only once the
   * subscription answered without one (or without a subscription), and never
   * while the machine is offline.
   */
  function detailsFor(s: ActionRow, online: boolean, followed?: Followed): Details | undefined {
    const matches = (d: Details | null | undefined): d is Details =>
      !!d && d.run_id === (s.runId ?? "") && d.conversation_id === (s.conversationId ?? "");
    const streamed = followed?.value ? detailsFromInspection(followed.value) : undefined;
    if (matches(streamed)) return streamed;
    const id = sessionId(s.computer, s.name);
    if (online && (!followed || followed.settled)) {
      const sig = signature(s);
      untrack(() => {
        if (fetched[id]?.sig === sig) return;
        if (inflight.has(id)) { if (inflight.get(id) !== sig) wanted.set(id, { sig, row: snapshotOf(s) }); return; }
        fetchDetails(id, snapshotOf(s));
      });
    }
    const value = fetched[id]?.value;
    return matches(value) ? value : undefined;
  }

  /** The text the last successful copy put on the clipboard (the actions smoke logs it). */
  let copied = "";
  const lastCopied = () => copied;

  /** Copies text; a failure is a sticky notice, a success shows nothing. */
  async function copy(text: string): Promise<Outcome> {
    try { await bridge.copyText(text); copied = text; return { outcome: "done", receipt: null, cleanup: null }; } catch (error) {
      notify("error", errorText(error), true, undefined, tr("Not copied"));
      return failed(errorText(error));
    }
  }

  /** The live row for a copy, after the click-time recheck (`stillListed`). */
  function current(row: ActionRow): ActionRow | undefined {
    const live = stillListed(row);
    return live ? snapshotOf(live) : undefined;
  }

  /** The details of the live row while it is the selected one with no subagent open (Qt's m_details). */
  function selectedDetails(live: ActionRow): Details | undefined {
    const sel = selection();
    if (sel.key !== live.key || sel.subagent) return undefined;
    return detailsFor(live, online(live.computer), followedFor(live.key));
  }

  /** Copy session name: the live row's name. */
  async function copyName(row: ActionRow): Promise<Outcome> {
    const live = current(row);
    return live ? copy(live.name) : failed(tr(GONE));
  }

  /** The header copies the selected conversation, run or child ID and names it. */
  async function copyIdentity(row: ActionRow, details?: Details, child?: ChildIdentity): Promise<Outcome> {
    const live = current(row);
    if (!live) return failed(tr(GONE));
    const sel = selection();
    if (live.runId !== row.runId || (sel.key && sel.key !== live.key) || sel.subagent !== child?.id) return failed(tr(CHANGED));
    const identity = sessionIdentity(live, details ?? selectedDetails(live), child);
    if (!identity.value) return failed("nothing to copy");
    const out = await copy(identity.value);
    if (out.outcome === "done") notify("info", identity.title === "Session ID" ? tr("The session ID is on the clipboard") : identity.title === "Agent ID" ? tr("The agent ID is on the clipboard") : tr("The run ID is on the clipboard"), false, undefined, tr("Copied"));
    return out;
  }

  /** Copy folder path: the selected row's current details folder, else the live row's; nothing when empty. */
  async function copyFolder(row: ActionRow, details?: Details): Promise<Outcome> {
    const live = current(row);
    if (!live) return failed(tr(GONE));
    const folder = copyState(live, online(live.computer), details ?? selectedDetails(live)).folder;
    return folder.enabled ? copy(folder.path) : failed("nothing to copy");
  }

  /** Copy connect command or Copy restore command (the archive id only for an archived row, which needs one). */
  async function copyCommand(row: ActionRow): Promise<Outcome> {
    const live = current(row);
    if (!live) return failed(tr(GONE));
    const archived = live.state === "archived";
    if (archived && !live.archiveId) return failed("nothing to copy");
    let command: string;
    try { command = await bridge.copyCommand(live.computer, live.name, archived ? live.archiveId ?? null : null); } catch (error) {
      notify("error", errorText(error), true);
      return failed(errorText(error));
    }
    return copy(command);
  }

  /** Copy session reference: the selected row's details name the run and conversation when present. */
  async function copyReference(row: ActionRow, details?: Details): Promise<Outcome> {
    const live = current(row);
    if (!live) return failed(tr(GONE));
    const d = details ?? selectedDetails(live);
    const target: Target = {
      computer: live.computer, name: live.name,
      run_id: d?.run_id || live.runId || "", conversation_id: d?.conversation_id || live.conversationId || "",
    };
    let uri: string;
    try { uri = await bridge.sessionReference(target, live.state === "archived" ? live.archiveId ?? null : null); } catch (error) {
      notify("error", errorText(error), true);
      return failed(errorText(error));
    }
    const out = await copy(uri);
    if (out.outcome === "done") notify("info", tr("The session reference is on the clipboard"), false, undefined, "Copied");
    return out;
  }

  return { perform, performComputer, end, forget, toggleMute, stillListed, pendingNotifications, copy, copyName, copyIdentity, copyFolder, lastCopied, copyCommand, copyReference, change, updating, interrupting, detailsFor, notices: list, dismiss, notify, clearNotice };
}

type FollowSource = { target(): { id: string } | undefined; summary(): Record<string, unknown> | undefined; loaded(): boolean; ended(): boolean; error(): unknown };
let activity: FollowSource | undefined;
/** Hands the store the app's Activity (session.ts), which this module cannot import without a cycle. */
export function bindActivity(source: FollowSource) { activity = source; }

/** The Activity subscription of a session: its state value while it is the followed one, and whether it has answered. */
export function followedFor(key: string): Followed {
  // An archived row has no subscription (it is never streamed): its details are inspected at once.
  if (activityReadOnly(key)) return { value: undefined, settled: true };
  if (!activity) return { value: undefined, settled: false };
  const mine = activity.target()?.id === key;
  const value = mine ? activity.summary() : undefined;
  return { value, settled: mine && (value !== undefined || activity.loaded() || activity.ended() || !!activity.error()) };
}

let selection: () => Selection = () => ({});
/** Hands the store the selected row and open subagent (session.ts), which this module cannot import: ui.ts reads the page. */
export function bindSelection(source: () => Selection) { selection = source; }

/**
 * The rows the actions look up by key: the catalog's, then the loaded archived
 * rows (Qt's entries hold both). Only lookups by key are answered.
 */
export function withArchived(catalog: Pick<CatalogState, "rows">, archived: (key: string) => Session | undefined): Pick<CatalogState, "rows"> {
  return { rows: new Proxy({} as CatalogState["rows"], { get: (_, key) => (typeof key === "string" ? catalog.rows[key] ?? archived(key) : undefined) }) };
}

let archivedRow: (key: string) => Session | undefined = () => undefined;
let searchRow: (key: string) => Session | undefined = () => undefined;
/** One selected archive from a checked search reply; normal action preflights still apply. */
export function bindSearchActionRow(lookup: (key: string) => Session | undefined) { searchRow = lookup; }
/** Hands the store the loaded archived rows (store/archive.ts), which this module cannot import: archive.ts reads ui.ts. */
export function bindArchivedRows(lookup: (key: string) => Session | undefined) { archivedRow = lookup; }

const app = createActions(withArchived(appCatalog, (key) => archivedRow(key) ?? searchRow(key)), (computer) => !computerOffline(computer), () => selection(), appMarks, appBus,
  (out, row) => { void import("./cleanupNotices").then((m) => m.reportCleanup(out, row)).catch(() => {
    appBus.notify("warn", tr("Working folder kept for review."), true, sessionNoticeKey(row.computer, row.name), tr("Session archived"));
  }); }, async (row, action, execute, current, remove) => {
    const { endActions } = await import("./endActions");
    return endActions.schedule([captureEnd(row, action)], ([ref], key) => execute(ref, key), { current, remove });
  });
export const { perform, performComputer, end, forget, toggleMute, stillListed, pendingNotifications, copy, copyName, copyIdentity, copyFolder, lastCopied, copyCommand, copyReference, change, updating, interrupting, detailsFor, notices, dismiss, notify, clearNotice } = app;
