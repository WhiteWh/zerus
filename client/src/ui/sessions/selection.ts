import type { Action } from "../../types";
import { allowedOp, forgetsBindingOnly, snapshotOf, type ActionRow } from "./actions-ui";
import type { Session } from "./types";
import { trNoop, tr } from "../../i18n";

// What a multi-selection can do, after the Qt SessionSelection.cpp:
// selectionActionText, selectionActionApplies, sameInstance, forgetsBinding,
// the runSelectionAction confirmation and the runNextSelectionAction calls and
// notices. Pure: store/batch.ts runs it and the selection bar renders it.

export type SelectionAction = "compact" | "expand" | "mark_read" | "review_later" | "pause" | "resume" | "archive" | "move" | "forget" | "terminate";
/** The actions that write to a host, one session at a time. */
export type WriteAction = Extract<SelectionAction, "pause" | "resume" | "archive" | "forget" | "terminate">;

/** The bar's and the selection menu's order (SessionSelection.cpp 121-122). */
export const SELECTION_ACTIONS: readonly SelectionAction[] = ["compact", "expand", "mark_read", "review_later", "pause", "resume", "archive", "move", "forget", "terminate"];

/** Move to project waits for the organisation stream's move (zerus-5wmh.18). */
export const MOVE_LATER = trNoop("Arrives with project organisation.");

const TEXT: Record<SelectionAction, string> = {
  mark_read: trNoop("Mark as read"),
  review_later: trNoop("Mark needs attention"),
  compact: trNoop("Compact session rows"),
  expand: trNoop("Expand session rows"),
  pause: "Pause",
  resume: "Resume / restore",
  archive: trNoop("Move to archive"),
  forget: "Forget",
  terminate: "Terminate",
  move: trNoop("Move to project"),
};

/** Qt's selectionActionText. */
export const selectionText = (a: SelectionAction) => tr(TEXT[a]);

/** A selected row: the action fields plus what Needs attention reads. A catalog row is one. */
export type SelectionRow = ActionRow & Pick<Session, "status" | "reviewLater">;

/** A plain copy of a selected row, as it was when the action was chosen. */
export function selectionSnapshot(s: SelectionRow): SelectionRow {
  return { ...snapshotOf(s), status: s.status, reviewLater: s.reviewLater };
}

/** Qt's sameInstance: the queue acts only on the row it was given. */
export function sameInstance(a: ActionRow, b: ActionRow): boolean {
  return a.name === b.name && (a.archiveId ?? "") === (b.archiveId ?? "") && a.created === b.created
    && (a.runId ?? "") === (b.runId ?? "") && (a.conversationId ?? "") === (b.conversationId ?? "") && a.state === b.state;
}

/**
 * As for one session, Forget of an OpenCode session removes only its Zerus
 * binding, even while it runs; in a selection it is scoped to the row's own
 * run and conversation.
 */
export const forgetsBinding = (s: Pick<ActionRow, "state" | "allowed" | "cmd">) => s.state !== "archived" && forgetsBindingOnly(s);

/** The operation a write needs allowed (an archived row restores). */
export function selectionOperation(s: Pick<ActionRow, "state">, a: WriteAction): "pause" | "restore" | "resume" | "archive" | "forget" | "terminate" {
  return a === "resume" ? (s.state === "archived" ? "restore" : "resume") : a;
}

/** What applicability reads besides the row. */
export interface ApplyContext {
  online: boolean;
  /** A write is pending for the session (phase 2's per-session lane). */
  updating: boolean;
  /** The row's compact preference. */
  compact: boolean;
  /** The row needs attention. */
  attention: boolean;
  /** A live row of the same computer has this archived row's name (Qt's archiveNameOccupied). */
  occupied: boolean;
}

/**
 * Qt's selectionActionApplies. Qt's other blockers of a write (terminating,
 * composer sending, a pending answer) have no client state yet.
 */
export function selectionApplies(s: ActionRow, a: SelectionAction, c: ApplyContext): boolean {
  switch (a) {
    case "compact": return !c.compact;
    case "expand": return c.compact;
    case "move": return true;
    case "mark_read": return s.state !== "archived" && c.attention;
    case "review_later": return s.state !== "archived" && !c.attention;
  }
  if (!c.online || c.updating || !allowedOp(s, selectionOperation(s, a))) return false;
  switch (a) {
    case "pause": return s.state === "running" && s.resumable && s.activity === "idle" && s.processState !== "exited";
    case "resume": return s.resumable && s.state !== "running" && (s.state !== "archived" || (!!s.archiveId && !c.occupied));
    case "archive": return s.state === "paused" || s.state === "stopped";
    case "forget": return forgetsBinding(s) ? !!s.runId && !!s.conversationId
      : s.state !== "running" && (s.state !== "archived" || !!s.archiveId);
    case "terminate": return s.state === "running";
  }
}

/** The Forget or Terminate confirmation (runSelectionAction 258-289). */
export interface BatchConfirmation {
  title: string;
  text: string;
  /** "{machine} / {name}" per target (Qt's detailed text). */
  details: string[];
  confirm: string;
}

/**
 * The confirmation for `targets`, the applicable rows of `selected` selected
 * ones. `clearArchive` words Clear archive; `paged` when only loaded archive
 * entries are in scope. Qt's "Remove working folders after stopping" belongs
 * to zerus-5wmh.10.
 */
export function confirmationCopy(a: "forget" | "terminate", targets: readonly ActionRow[], selected: number, clearArchive?: { paged: boolean }): BatchConfirmation {
  const n = targets.length;
  const action = selectionText(a);
  let text = clearArchive
    ? clearArchive.paged
      ? tr("Remove %1 loaded archived sessions in the current date and machine scope? Unloaded entries are kept.", n)
      : tr("Remove %1 archived sessions from HGS on the selected machines?", n)
    : `${action} ${n} selected sessions?`;
  const bindings = a === "forget" ? targets.filter(forgetsBinding).length : 0;
  if (a === "terminate") text += "\n\nRunning processes will stop. Tracked Codex, Claude and Kimi conversations are kept in Archive; native conversation history is kept.";
  else if (bindings < n) {
    text += bindings
      ? "\n\nThese saved or archived entries are removed. Other live sessions and native conversation history are kept."
      : "\n\nOnly these saved or archived entries are removed. Live sessions and native conversation history are kept.";
  }
  if (bindings) text += "\n\nOpenCode sessions: only Zerus forgets them; their conversations stay in OpenCode.";
  if (selected !== n) text += `\n\n${selected - n} unavailable or inapplicable sessions will be left as they are.`;
  return {
    title: clearArchive ? tr("Clear archive") : action,
    text,
    details: targets.map((t) => `${t.machine} / ${t.name}`),
    confirm: clearArchive ? tr("Clear archive") : `${action} ${n} sessions`,
  };
}

/**
 * The call behind one queue item (runNextSelectionAction 329-361) and the
 * operation that names it in an error line. An archived row restores or
 * forgets through the archive calls (Piece E).
 */
export type SelectionCall = { operation: string; call: Action } | { operation: string; archive: "restore" | "forget" };

export function selectionCall(s: ActionRow, a: WriteAction): SelectionCall {
  const archived = s.state === "archived";
  switch (a) {
    case "pause": return { operation: `pause ${s.name}`, call: { kind: "pause" } };
    case "resume": return archived ? { operation: `restore ${s.name}`, archive: "restore" } : { operation: `resume ${s.name}`, call: { kind: "resume" } };
    case "archive": return { operation: `archive ${s.name}`, call: { kind: "archive" } };
    case "forget":
      if (forgetsBinding(s)) return { operation: `forget ${s.name}`, call: { kind: "forget_binding" } };
      return archived ? { operation: `forget archive ${s.name}`, archive: "forget" } : { operation: `kill ${s.name}`, call: { kind: "forget" } };
    case "terminate": {
      const keepArchive = s.tracked && ["codex", "claude", "kimi"].includes(s.cmd);
      return keepArchive ? { operation: `terminate ${s.name}`, call: { kind: "terminate" } } : { operation: `kill ${s.name}`, call: { kind: "kill" } };
    }
  }
}

/** The info notice before each call. */
export const progressText = (a: SelectionAction, i: number, n: number) => `${selectionText(a)}: ${i} / ${n}…`;

/** An item whose row changed, went away or no longer applies. */
export const changedText = (s: Pick<ActionRow, "machine" | "name">) => `${s.machine} / ${s.name}: session changed or is unavailable`;

/** An archived row past the restore limit (Piece E). */
export const restoreLimitText = (s: Pick<ActionRow, "name">) => `${s.name}: earlier restores still need a refresh`;

/** Errors the end notice shows. */
export const SHOWN_ERRORS = 3;

/** The end notice; sticky (and an error) when any item failed. */
export function summaryText(a: SelectionAction, done: number, total: number, failed: number, errors: readonly string[], unknown = 0): { text: string; failed: boolean } {
  let text = `${selectionText(a)}: ${done} of ${total} completed.`;
  if (failed || unknown) {
    const counts = [failed ? `${failed} failed or changed` : "", unknown ? `${unknown} unknown` : ""].filter(Boolean).join(", ");
    text += ` ${counts}; refresh before retrying.\n${errors.slice(0, SHOWN_ERRORS).join("\n")}`;
  }
  return { text, failed: failed + unknown > 0 };
}

/** Mark as read and Mark needs attention: sticky when not every target changed. */
export function markSummary(a: "mark_read" | "review_later", changed: number, targets: number): { text: string; differs: boolean } {
  return { text: `${selectionText(a)}: ${changed} of ${targets} sessions updated.`, differs: changed !== targets };
}
