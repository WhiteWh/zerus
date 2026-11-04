import { GONE, notify, stillListed } from "../../store/actions";
import { markAllRead, markNeedsAttention, markRead, markReadUnavailable, unreadReplies } from "../../store/marks";
import { computerFresh } from "../../store/catalog";
import { needsAttention, selected, selectedKey, setReadSelectionKey, type MenuEntry } from "../../store/ui";
import type { AttentionFacts } from "../../store/attention";
import { snapshotOf, type ActionRow } from "./actions-ui";
import type { Session } from "./types";
import { trNoop, tr } from "../../i18n";

// The mark entries of Qt's Sessions menu (SessionsWindow.cpp 5324-5328, 5416-5433,
// 362-365, 3389-3402), run against the local-first marks store.

export const NEW_REPLY = "A new reply or question arrived. Open the menu again to mark it as read.";
export const REVIEW_HINT = trNoop("Keep a reminder to review this session later");
export const MARK_ALL_HINT = trNoop("Mark current replies across all machines as read, including hidden sessions. Pending approvals, questions and errors stay highlighted. Sessions you marked as needing attention keep their reply unread.");
const CHIP_REVIEW = trNoop("Marked for later. Mark as read to clear this reminder.");
const CHIP_READ = trNoop("Mark this session's current notification as read. This does not answer a pending question.");

/** The Mark read chip of the composer toolbar (phase 4 places it): active for a live row that needs attention. */
export function markReadChip(s: AttentionFacts, online: boolean): { active: boolean; detail: string } {
  const active = s.state !== "archived" && needsAttention(s, online);
  return { active, detail: tr(s.reviewLater ? CHIP_REVIEW : CHIP_READ) };
}

/** Mark as read, with Qt's refusal notice. The open row stays listed until another selection (SessionsWindow.cpp:5432). */
export function runMarkRead(row: ActionRow): boolean {
  if (!stillListed(row)) return false;
  const reason = markReadUnavailable(row);
  if (reason) { notify("error", reason, true); return false; }
  const ok = markRead(row);
  if (!ok) notify("error", NEW_REPLY, true);
  else if (row.key === selectedKey()) setReadSelectionKey(row.key);
  return ok;
}

/** Mark needs attention, with Qt's refusal notice. */
export function runMarkNeedsAttention(row: ActionRow): boolean {
  if (!stillListed(row)) return false;
  const ok = markNeedsAttention(row);
  if (!ok) notify("error", tr(GONE), true);
  return ok;
}

/** The row's entry, or none for an archived row. Both are always enabled. */
export function markEntry(s: Session): Extract<MenuEntry, { label: string }> | undefined {
  if (s.state === "archived") return undefined;
  const row = snapshotOf(s);
  return markReadChip(s, computerFresh(s.computer)).active
    ? { label: tr("Mark as read"), icon: "check-check", action: () => { runMarkRead(row); } }
    : { label: tr("Mark needs attention"), icon: "bell", hint: tr(REVIEW_HINT), action: () => { runMarkNeedsAttention(row); } };
}

/** The footer button's tooltip (Qt: the label, a newline, the hint). */
export const markAllTip = (e: { label: string; hint?: string }) => `${e.label}\n${e.hint ?? ""}`;

export const markAllLabel = (n: number) => (n > 0 ? tr("Mark all as read (%1)", n) : tr("Mark all as read"));

/**
 * Mark all as read: every current reply on every machine; a count shows its
 * notice. An open row with an unread reply stays listed until another
 * selection (SessionsWindow.cpp:3393-3395).
 */
export function runMarkAll(): number {
  const open = selected();
  if (open?.unreadReply) setReadSelectionKey(open.key);
  const n = markAllRead();
  if (n > 0) notify("info", n === 1 ? "1 reply marked as read." : `${n} replies marked as read.`);
  return n;
}

export function markAllEntry(): Extract<MenuEntry, { label: string }> {
  const n = unreadReplies().size; // tracked: the marks store's version
  return { label: markAllLabel(n), icon: "read-all", disabled: n === 0, hint: tr(MARK_ALL_HINT), action: () => { runMarkAll(); } };
}
