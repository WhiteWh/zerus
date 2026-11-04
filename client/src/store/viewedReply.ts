import type { ActivityEvent } from "../types";
import type { Session } from "../ui/sessions/types";

// A reply the person has looked at is read, as in Qt's
// SessionsWindow::checkViewedReply (tray/src/SessionsWindow.cpp:3404-3432) and
// ActivityView::replyVisible (tray/src/ActivityView.cpp:1549-1574). Only the
// selected session's current reply counts, only in an active window, and only
// after it stayed in view for a second; then that reply alone is read (Qt's
// FleetState::markReplyRead): the attention and Review later stay as they are.
// Pure: ui/sessions/viewed-reply.ts reads the window, the row, its inspection
// and what Activity or Terminal shows on Qt's 400 ms timer.

/** Qt's m_readTimer interval (SessionsWindow.cpp:2028). */
export const CHECK_MS = 400;
/** How long the same reply stays in view before it is read (m_readDwell, SessionsWindow.cpp:3423). */
export const DWELL_MS = 1000;
/** ActivityView::nearBottom: Activity is at its end within this many pixels. */
export const NEAR_END = 30;

/** What the check reads of the selected row. */
export type ViewedRow = Pick<Session, "key" | "computer" | "name" | "state" | "conversationId" | "replyId" | "unreadReply">;

export interface ViewedFacts {
  /** A subagent's Activity is selected (Qt's m_subagentId). */
  subagent: boolean;
  /** The selected session, undefined without one. */
  row: ViewedRow | undefined;
  /** The row's computer is online (Qt's entry.online). */
  online: boolean;
  /** The selected session's inspection (Qt's m_details): its followed state value. */
  inspected: Record<string, unknown> | undefined;
  /** Its last inspect failed (Qt's m_inspectError). */
  inspectError: boolean;
  /** The detail tab that shows. */
  tab: string;
  /**
   * The window shows, is not minimised and has the focus, the Sessions page is
   * current and no modal dialog or popup is open. Read after the row's own
   * facts, so a session without an unread reply reads no window state.
   */
  active(): boolean;
  /** Activity shows this reply (replyVisible). Read only for the Activity tab. */
  activityShows(reply: string): boolean;
  /** Terminal is connected, scrolled to its bottom and not blank. Read only for the Terminal tab. */
  terminalShowsLatest(): boolean;
}

/**
 * The reply in view, as `key \n conversation \n reply`, or undefined. The row
 * must have an unread reply on an online computer, its inspection must have
 * succeeded and name the same conversation and reply, and Activity or Terminal
 * must show it. An archived row, a subagent's Activity and any other tab never
 * count.
 */
export function viewedCandidate(f: ViewedFacts): string | undefined {
  const s = f.row;
  if (f.subagent || !s || s.state === "archived" || !s.unreadReply || !f.online || f.inspectError) return undefined;
  const conversation = s.conversationId ?? "", reply = s.replyId ?? "";
  if (!conversation || !reply || f.inspected?.conversation_id !== conversation || f.inspected?.reply_id !== reply) return undefined;
  if (!f.active()) return undefined;
  const shown = f.tab === "activity" ? f.activityShows(reply) : f.tab === "terminal" && f.terminalShowsLatest();
  return shown ? `${s.key}\n${conversation}\n${reply}` : undefined;
}

export interface ViewedReplyDeps {
  now(): number;
  /** FleetState::markReplyRead: true when it read exactly that reply. */
  markReplyRead(computer: string, name: string, conversation: string, reply: string): boolean;
  /** Keeps the row in the filtered list until another selection (Qt's m_readSelectionKey). */
  keep(key: string): void;
}

/**
 * The dwell of checkViewedReply. Each `check` takes the current facts: a new
 * candidate starts the dwell, any change or a check without one resets it, and
 * the same candidate after DWELL_MS reads its reply once. True when it did.
 */
export function createViewedReply(deps: ViewedReplyDeps) {
  let candidate: string | undefined;
  let since = 0;
  return {
    check(f: ViewedFacts): boolean {
      const next = viewedCandidate(f);
      if (!next) { candidate = undefined; return false; }
      if (next !== candidate) { candidate = next; since = deps.now(); return false; }
      if (deps.now() - since < DWELL_MS) return false;
      const s = f.row!;
      // The opened result stays in the filtered list until the person switches
      // away: reading it must not replace the conversation under their eyes.
      candidate = undefined;
      deps.keep(s.key);
      return deps.markReplyRead(s.computer, s.name, s.conversationId!, s.replyId!);
    },
    /** A hidden window, another selection or a closed view forgets the candidate. */
    reset() { candidate = undefined; },
    /** The candidate in its dwell (tests). */
    candidate: () => candidate,
  };
}

export type ViewedReply = ReturnType<typeof createViewedReply>;

/** What Activity shows, read only when the check asks. */
export interface ReplyView {
  /** The transcript is placed and current: no search result shows and no layout waits for a gesture or selection. */
  settled(): boolean;
  /** Pixels between the viewport's bottom and the transcript's end. */
  below(): number;
  /** The viewport's height. */
  viewport(): number;
  /** A rendered row's top relative to the viewport's top (ReadingRows.offset), undefined when it is not rendered. */
  offset(id: string): number | undefined;
}

/**
 * ActivityView::replyVisible: Activity is at its end and the reply's row
 * starts within the viewport. The row is the reply's journal event
 * (`item-s<seq>`), else the recorded response of a timeline without events.
 * Without either, a turn that finished without response text (an empty Stop of
 * an idle session) or loaded events at or past the reply count as seen.
 */
export function replyVisible(view: ReplyView, reply: string, events: readonly ActivityEvent[], inspected: Record<string, unknown> | undefined): boolean {
  if (!view.settled() || view.below() > NEAR_END) return false;
  const seq = reply.split(":", 1)[0];
  let top = view.offset(`item-s${seq}`);
  if (top === undefined && typeof inspected?.last_message === "string" && inspected.last_message) top = view.offset("item-answer-snapshot");
  if (top !== undefined) return top < view.viewport();
  // Providers can finish a turn without response text. Its visible Ready
  // state and final operational event are the available result.
  if (inspected?.activity === "idle" && events.some((e) => String(e.seq ?? 0) === seq && e.type === "Stop" && !e.detail)) return true;
  // Catching up to activity after a completed turn acknowledges that turn
  // too, even after its event was pruned from the visible window.
  const sequence = /^\d+$/.test(seq) ? Number(seq) : 0;
  return sequence > 0 && inspected?.reply_id === reply && events.some((e) => (e.seq ?? 0) >= sequence);
}
