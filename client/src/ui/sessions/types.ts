import type { Reply } from "../../types";
// View models the session list and Activity render. A `Session` is the
// catalog store's row: the Rust view row plus the presentation fields derived
// from it once per upsert (store/catalog.ts `toSession`).

export type AgentKind = "claude" | "codex" | "kimi" | "deepseek" | "opencode" | "sh";

/** Presentation status, in the existing precedence order (design/webui). */
export type Status =
  | "attention" // Needs attention: question, approval or explicit attention
  | "working" // native work in progress
  | "background" // idle with confirmed owned live shell/native jobs
  | "scheduled" // live idle with pending self-scheduled jobs
  | "ready" // idle
  | "error" // provider error / interrupted / recovery needed
  | "offline" // machine unreachable: last known state
  | "paused" // saved conversation, not running
  | "waiting" // idle, waiting for another agent's result (hgs wait)
  | "starting" // pending launch row
  | "ended"; // conversation ended (archive candidate)

export interface Subagent {
  id: string;
  name: string;
  state: "working" | "ready" | "attention" | "error" | "paused";
  summary: string;
  /** Qt's inactiveChild: hidden unless completed children, this child or its draft is retained. */
  inactive?: boolean;
  group?: boolean;
}

export interface Session {
  /** The row id: computer + "\n" + catalog key. Stable across renames. */
  key: string;
  /** The catalog key (`run:<run_id>` or `name:<name>`). */
  catalogKey: string;
  /** "" is this computer, otherwise its `hgs @alias`. */
  computer: string;
  /** The full session name (`agent/project/tag`), used for actions and follow. */
  name: string;
  /** What the card shows as its title. */
  title: string;
  /** The machine the card shows; "this computer" stands in for a local row without one. */
  machine: string;
  /** The row's own machine (the host name or alias, "" before this computer's hello): with the name, the project member key. */
  rawMachine: string;
  project: string;
  agent: AgentKind;
  model: string;
  effort?: string;
  /** The checkout's folder as people know it (a name; a linked worktree names its main checkout). */
  cwd: string;
  branch?: string;
  worktree?: string;
  git?: { ahead: number; behind: number; changed: number; conflicts?: number; state: "synced" | "diff" | "push" | "pull" | "unknown" };
  status: Status;
  /** The Qt caption for the status (SessionPresentation). */
  caption: string;
  phase?: string;
  summary: string;
  unread: boolean;
  /** The reply before a reply in progress hides it; undefined for an older projection. */
  unreadReply?: boolean;
  draft: boolean;
  muted?: boolean;
  /** The host's preferences conflict with the confirmed override (Qt's notificationPreferencesError). */
  notificationConflict?: boolean;
  attention?: "question" | "approval" | "reply";
  scheduleNext?: string;
  backgroundJobs?: number;
  subagents?: Subagent[];
  subagentCountsComplete?: boolean;
  reviewLater?: boolean;
  /**
   * Qt's SessionInfo::needsAttention with this device's marks: a request not
   * acknowledged, an unread reply or Review later (no children). Undefined for
   * a projection without `needs_attention`.
   */
  attentionNeeded?: boolean;
  /**
   * Qt's `needsAction() && !attentionAcknowledged` with this device's marks:
   * what a project header counts as needing a response, beside Review later
   * and children. Undefined for a projection without `needs_attention`.
   */
  actionNeeded?: boolean;
  /** A subagent asks while the session runs and the attention is not acknowledged. */
  childAttention?: boolean;
  optionalQuestions?: number;
  /** Seconds since the Unix epoch of the row's last change, when known. */
  updatedAt?: number;
  /** Seconds since the Unix epoch when the current turn started (the Working clock). */
  turnStarted?: number;
  runId?: string;
  launchId?: string;
  conversationId?: string;
  /** Exact/fallback device-local density identity, calculated in Rust. */
  compactKeys?: [string, string];
  // What the guarded actions read, copied flat from the core's row: a nested
  // row in the store would double the cost of reconciling a large catalog.
  /** The agent as the core reports it (`deepseek` for `dsh`); `agent` draws unknown agents as Claude. */
  cmd: string;
  accountProvider?: string;
  accountId?: string;
  accountHome?: string;
  accountReported?: boolean;
  /** The raw state (`running`, `paused`, `stopped`, `archived`, ...). */
  state: string;
  tracked: boolean;
  resumable: boolean;
  /** The raw activity (`idle`, `busy`, ...). */
  activity: string;
  processState: string;
  archiveId?: string;
  /** The latest reply and the attention it raised, for marking them read. */
  replyId?: string;
  attentionSignature?: string;
  /** The host shares read marks. */
  sharedRead: boolean;
  /** The operations the host reports as allowed, space separated; undefined when it reports none. */
  allowed?: string;
  /** The host's reasons for a disallowed archive or terminate. */
  archiveReason?: string;
  terminateReason?: string;
  forgetReason?: string;
  /** The raw working directory (`cwd` is the folder name). */
  cwdPath: string;
  /** The canonical working directory, "" when the row has none (New session's prefill). */
  cwdCanonical: string;
  /** The checkout's root, "" outside Git (the worktree picker's session counts). */
  gitRoot: string;
  /** The row's terminal support, and whether the host reported it (NativeSessionCapabilities::terminal). */
  terminalSupported: boolean;
  terminalReported: boolean;
  terminalAttachSupported: boolean;
  terminalAttachReported: boolean;
  /** Pause all and Resume all include the row (false for OpenCode). */
  bulkLifecycle: boolean;
  // The row as the host lists it, before this device's marks: store/marks.ts
  // derives status, caption, attention, unread, reviewLater, summary and muted
  // from these (Qt's FleetState::updateUnread).
  /** `created`, whole seconds since the Unix epoch (0 when unknown). */
  created: number;
  /** What a local attention mark acknowledges (Qt's `attentionFingerprint`); "" when not reported. */
  fingerprint: string;
  needsAction: boolean;
  /** Status and caption with the attention acknowledged. */
  ackStatus: Status;
  ackCaption: string;
  /** A finished reply of a live conversation; undefined for a projection without it. */
  unreadCandidate?: boolean;
  /** The listing's `needs_attention`; undefined for a projection without it. */
  baseAttentionNeeded?: boolean;
  baseChildAttention: boolean;
  /** The core kept the confirmed preferences over a conflicting listing. */
  baseNotificationError: boolean;
  baseStatus: Status;
  baseCaption: string;
  baseAttention?: "question" | "approval" | "reply";
  baseUnread: boolean;
  baseSummary: string;
  baseMuted: boolean;
  /** The host's read mark of this conversation. */
  readReplyId?: string;
  localReadReply?: boolean;
  localReadAttention?: boolean;
  readAttentionSignature?: string;
  /** The notification preferences pass Qt's validation, and their revision. */
  sharedNotifications: boolean;
  notificationRevision?: number;
}

/** A file of a message, as Qt's attachment card shows it (ActivityView.cpp:759-782). */
export interface ItemAttachment {
  name: string;
  /** `{reference }{name}`. */
  caption: string;
  mime: string;
  /** "{n} KiB", or "File" when its size is unknown. */
  meta: string;
  /** The message's request and the file's place in it; the request is empty when the message saved no reference. */
  request_id: string;
  index: number;
  /** An image the card shows a thumbnail of: any image but SVG. */
  image: boolean;
  /** Private presentation from this client's exact local send, never a journal path. */
  preview_path?: string;
  image_size?: import("../../types").ImageSize;
  own?: boolean;
}

/** A local message row's state (ActivityView.cpp:1053-1068). */
export interface LocalItem {
  /** The row's id (its request id). */
  id: string;
  /** "Sending…", "Not sent", "Delivery not confirmed" or "Submitted to terminal". */
  stamp: string;
  /** A failed row's error line. */
  error: string;
  uncertain: boolean;
  /** Retry and Delete for a definite failure; Check and Delete for an uncertain one. */
  actions: ("retry" | "inspect" | "delete")[];
  inspectLabel: string;
  /** Per file: whether it can be opened (Qt's request_id or local_path rule). */
  available: boolean[];
}

export interface ActivityItem {
  id: string;
  kind: "user" | "agent" | "tool" | "tools" | "question" | "approval" | "system" | "receipt" | "thinking" | "report" | "voice" | "cleared" | "context";
  at: string;
  title?: string;
  body?: string;
  /** The native prompt echo to restore if its attachment cannot be read. */
  echoBody?: string;
  code?: string;
  lines?: string[];
  attachments?: ItemAttachment[];
  sender?: string;
  /** Receipt line under a user message ("Sent", "Queued at this time"). */
  receipt?: string;
  /** Operational group that contains an attention event (approval, failure). */
  warn?: boolean;
  /** A header other than "You" or "Agent" (recorded context, answers). */
  label?: string;
  /** A local message row (this app's send) and its buttons. */
  local?: LocalItem;
  /** Codex question replies the message holds, shown as Question and Your answer blocks (ActivityView.cpp:347-363). */
  replies?: Reply[];
}

export type VoiceState =
  | "uploading" | "queued" | "transcribing" | "ready" | "waiting_agent"
  | "delivering" | "submitted" | "failed" | "cancelled" | "uncertain";

export interface MediaMessage {
  id: string;
  kind: "media";
  media: "voice" | "video";
  at: string;
  state: VoiceState;
  durationMs: number;
  /** Exactly 64 integer amplitudes 0-255; anything else draws a flat line. */
  waveform: number[];
  /** Public receipt identity, never a path on the recording's host. */
  file?: { request_id: string; index: number };
  posterMessageId?: string;
  transcript?: string;
  error?: string;
}

export type TranscriptItem = ActivityItem | MediaMessage;
