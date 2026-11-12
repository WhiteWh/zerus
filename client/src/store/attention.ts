import type { Session } from "../ui/sessions/types";

// Shared by the session list and the Overview; pure, so tests can import it
// without the page state of store/ui.ts.

/** What the attention rules read from a row. */
export type AttentionFacts = Pick<Session, "state" | "status" | "unread" | "reviewLater" | "attentionNeeded" | "actionNeeded" | "childAttention">;
type Quiet = Partial<Pick<Session, "muted" | "quiet" | "reviewLater" | "state">>;

/**
 * Attention that draws nobody: a muted session, or a supervised child whose
 * requests are its parent's (zerus-sa9qn.3, the core's `quiet`). Its state,
 * unread identity and questions are unchanged. The counters never count it,
 * except a quiet child's Review later mark; the status filters read it through
 * filterAttention (zerus-sa9qn.18). A child with a request only the person can
 * answer is never quiet (core view.rs).
 */
export const quietAttention = (s: Pick<Session, "muted" | "quiet">) => !!s.muted || !!s.quiet;
/** A quiet supervised child still counts the person's own Review later mark. */
const quietCounts = (s: Quiet) => !s.muted && !!s.quiet && !!s.reviewLater && s.state !== "archived";

/**
 * Qt's SessionInfo::needsAttention (HgsClient.h:107-108) with this device's
 * marks: the core's `needs_attention` through store/marks.ts, what the menu
 * bar (menubar.rs, FleetState::attentionSessions) and Qt's machine cards
 * (DashboardPage.cpp:133) count. No children and no online gate. A
 * projection without the field reads its status and unread reply instead.
 */
export const attentionNeeded = (s: Pick<Session, "status" | "unread" | "attentionNeeded">) =>
  s.attentionNeeded ?? (s.status === "attention" || s.unread);

/** Qt's visibleUnreadReply (HgsClient.h:104-105), with older projections kept as they were. */
export const visibleUnreadReply = (s: Pick<Session, "state" | "unread" | "unreadReply" | "unreadCandidate">, online: boolean) => {
  if (s.state === "archived") return false;
  if (s.unreadReply === undefined || s.unreadCandidate === undefined) return s.unread;
  return s.unreadReply && (!online || s.unreadCandidate);
};

/**
 * Needs attention in the window's lists and counts: Qt's
 * SessionPresentation::needsAttention (SessionPresentation.h:279-293), that is
 * Review later, or, while the row's computer is `online`, attentionNeeded or
 * a subagent that asks. A computer that
 * is not online shows its last rows, which count only their Review later.
 */
export const needsAttention = (s: AttentionFacts, online: boolean) =>
  s.state !== "archived" && (!!s.reviewLater || (online && (attentionNeeded(s) || !!s.childAttention)));

/**
 * Qt's `needsAction() && !attentionAcknowledged` with this device's marks: a
 * request, error or recovery that asks for the person and is not
 * acknowledged. No unread reply, Review later or children. A projection
 * without the field reads its status.
 */
export const actionNeeded = (s: Pick<Session, "status" | "actionNeeded">) => s.actionNeeded ?? s.status === "attention";

/**
 * A project header's "need a response" (SessionsWindowRows.cpp:400-401):
 * Review later, or, while the row's computer is `online`, a request not
 * acknowledged or a subagent that asks. The header counts an unread reply
 * as unread, not here.
 */
export const headerAttention = (s: AttentionFacts, online: boolean) =>
  s.state !== "archived" && (!!s.reviewLater || (online && (actionNeeded(s) || !!s.childAttention)));

/** Needs attention as the counters count it: never muted, and a quiet child only for Review later. */
export const countedAttention = (s: AttentionFacts & Quiet, online: boolean) =>
  quietAttention(s) ? quietCounts(s) : needsAttention(s, online);
/** The project header's attention count, with the same quiet rule. */
export const countedHeaderAttention = (s: AttentionFacts & Quiet, online: boolean) =>
  quietAttention(s) ? quietCounts(s) : headerAttention(s, online);

/**
 * Qt's SessionPresentation::working (SessionPresentation.h:268-271): native
 * work or confirmed background jobs, only while the row's computer is
 * `online`. A computer that is not online shows its last rows, which never
 * count as working.
 */
export const isWorking = (s: Pick<Session, "status">, online: boolean) =>
  online && (s.status === "working" || s.status === "background");

/**
 * Needs attention as the status filters list and count it (zerus-sa9qn.18): a
 * quiet supervised child's unread reply, retry or limit waits for its parent,
 * so only the person's own Review later mark puts it there. Every other row
 * keeps needsAttention: mute silences notifications and the counters, and a
 * muted session stays listed, and counted, in the Needs attention filter.
 */
export const filterAttention = (s: AttentionFacts & Quiet, online: boolean) =>
  s.quiet ? !!s.reviewLater && s.state !== "archived" : needsAttention(s, online);

/**
 * The Active filter: current work OR Scheduled OR its attention OR a local
 * draft, as isActive, with the filters' attention (filterAttention).
 */
export const filterActive = (s: Session, online: boolean) =>
  isWorking(s, online) || (online && (s.status === "scheduled" || s.status === "waiting")) || filterAttention(s, online) || s.draft;

/**
 * Active = current work OR Scheduled OR Needs attention OR a local draft
 * (SessionPresentation::active, SessionPresentation.h:308-312). Work,
 * Scheduled and Waiting count only while the row's computer is `online`; a
 * computer that is not online keeps its Review later and its drafts.
 */
export const isActive = (s: Session, online: boolean) =>
  isWorking(s, online) || (online && (s.status === "scheduled" || s.status === "waiting")) || needsAttention(s, online) || s.draft;
