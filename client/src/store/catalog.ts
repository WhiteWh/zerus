import { batch, createSignal } from "solid-js";
import { applyMachines } from "./machines";
import { createStore, produce, reconcile, unwrap } from "solid-js/store";
import type { Batch, ComputerId, RowOut, ViewRow } from "../types";
import type { AgentKind, Session, Status } from "../ui/sessions/types";
import { marks as appMarks, type RowMarks } from "./marks";
import { connectionOf } from "./overview";
import { childRows } from "../ui/sessions/subagents";
import { keepWarmOf } from "../ui/sessions/keep-warm";
import { draftKey, nameDraftKey } from "./draftKeys";
import { tr } from "../i18n";
import { createGitProjection } from "./git";
import { anchorRetry, type RetryState } from "./retry";

// The catalog: every live session row of every computer, keyed by
// computer + "\n" + catalog key. A batch is one reactive transaction. An
// upsert reconciles into the existing row object, so unchanged rows keep their
// identity and a changed row updates only the DOM bound to its changed fields
// (rebuilding rows on every delta made the round 1 mockup flicker). A replace
// swaps one computer's rows in the same transaction: the old rows stay on
// screen until the replacement exists, never an empty list in between.

export const rowId = (computer: ComputerId, key: string) => `${computer}\n${key}`;
const computerOf = (id: string) => id.slice(0, id.indexOf("\n"));

export interface ComputerState {
  id: ComputerId;
  /** probing, unsupported, live, polled, reconnecting or ended. */
  status: string;
  /** Restart attempt while reconnecting. */
  attempt: number;
  /** This app's own supervisor's restart schedule, anchored on arrival. */
  retry?: RetryState;
}

export interface CatalogState {
  rows: Record<string, Session>;
  /** Arrival order. The list groups rows by the organization's projects and ranks, ties in this order (ui/sessions/model.ts). */
  order: string[];
  computers: Record<ComputerId, ComputerState>;
}

const KINDS = new Set<Status>(["attention", "working", "background", "scheduled", "ready", "error", "offline", "paused", "waiting", "starting", "ended"]);
const AGENTS = new Set<AgentKind>(["claude", "codex", "kimi", "deepseek", "opencode", "sh"]);

/** The presentation kind of a view row; Qt gives `sh` its own Shell badge. */
export function agentKind(agent: string | undefined): AgentKind {
  const a = (agent ?? "").toLowerCase();
  if (a === "dsh") return "deepseek";
  return AGENTS.has(a as AgentKind) ? (a as AgentKind) : "claude";
}

/**
 * The presentation status. The core sends a status kind; raw listing
 * vocabularies (activity, phase) are accepted too, so a row always lands in
 * one of the list's states.
 */
export function statusOf(v: ViewRow): Status {
  const s = (v.status ?? "").toLowerCase();
  const phase = (v.phase ?? "").toLowerCase();
  if (v.attention === true || s === "attention" || phase === "approval" || phase === "input") return "attention";
  if (KINDS.has(s as Status)) return s as Status;
  switch (s) {
    case "busy": case "tool": case "compacting": return "working";
    case "archived": return "ended";
    case "stopped": case "saved": return "paused";
    case "interrupted": case "exited": return "error";
  }
  switch (phase) {
    case "working": case "tool": case "compacting": return "working";
    case "starting": return "starting";
    case "error": case "interrupted": return "error";
  }
  return "ready";
}

/** Captions as in Qt (SessionPresentation::status), for the states a view row carries. */
export function captionOf(status: Status, v: ViewRow): string {
  const phase = (v.phase ?? "").toLowerCase();
  switch (status) {
    case "attention": return phase === "approval" ? tr("Needs approval") : phase === "input" ? tr("Needs input") : tr("Needs attention");
    case "working": return phase === "tool" ? tr("Using a tool") : phase === "compacting" ? "Compacting" : "Working";
    case "background": return "Background";
    case "scheduled": return "Scheduled";
    case "ready": return v.unread ? tr("New reply") : "Ready";
    case "error": return phase === "interrupted" ? "Interrupted" : "Error";
    case "offline": return "Offline";
    case "paused": return "Paused";
    case "waiting": return tr("Waiting for result");
    case "starting": return "Starting";
    case "ended": return "Archived";
  }
}

const text = (value: unknown) => (typeof value === "string" ? value : "");
const optional = (value: unknown) => (typeof value === "string" && value ? value : undefined);

/**
 * One catalog row from a view row, with this device's marks applied (store/marks.ts).
 * Every field is always present so reconcile compares like with like.
 */
export function toSession(computer: ComputerId, key: string, v: ViewRow, marks: Pick<RowMarks, "overlay"> = appMarks): Session {
  const status = statusOf(v);
  const phase = (v.phase ?? "").toLowerCase();
  const known = KINDS.has((v.status ?? "").toLowerCase() as Status) || !v.status;
  const reason = typeof v.attention === "string" ? v.attention : phase === "approval" ? "approval" : phase === "input" ? "question" : "";
  // The core words the caption as Qt does; a status that is not a kind is a caption too.
  const caption = text(v.caption) || (known ? captionOf(status, v) : text(v.status));
  const attention: Session["attention"] = status === "attention" ? (reason === "approval" ? "approval" : reason === "question" ? "question" : "reply") : undefined;
  const base: Session = {
    key: rowId(computer, key),
    catalogKey: key,
    computer,
    name: text(v.name),
    title: text(v.title) || text(v.name),
    machine: text(v.machine) || computer || "this computer",
    rawMachine: text(v.machine),
    project: text(v.project) || "Inbox",
    agent: agentKind(v.agent),
    model: text(v.model),
    effort: optional(v.effort),
    cwd: text(v.folder),
    branch: optional(v.branch),
    worktree: optional(v.worktree),
    gitWorktree: typeof v.git_worktree === "boolean" ? v.git_worktree : undefined,
    git: undefined,
    gitMetadataState: optional(v.git_metadata_state),
    gitDetached: v.git_detached === true,
    status,
    caption,
    phase: phase || undefined,
    summary: text(v.summary),
    unread: !!v.unread,
    draft: false,
    muted: !!v.muted,
    attention,
    scheduleNext: undefined,
    backgroundJobs: typeof v.background === "number" ? v.background : undefined,
    subagents: v.subagents ? childRows(v.subagents, v.subagent_counts_complete) : undefined,
    subagentCountsComplete: !!v.subagent_counts_complete,
    reviewLater: false,
    attentionNeeded: typeof v.needs_attention === "boolean" ? v.needs_attention : undefined,
    childAttention: !!v.child_attention,
    optionalQuestions: typeof v.optional_question_count === "number" ? v.optional_question_count : undefined,
    updatedAt: typeof v.updated_at === "number" ? v.updated_at : undefined,
    turnStarted: typeof v.turn_started === "number" && v.turn_started > 0 ? v.turn_started : undefined,
    runId: optional(v.run_id),
    launchId: optional(v.launch_id),
    sessionId: optional(v.session_id),
    parent: v.parent ?? undefined,
    quiet: !!v.quiet,
    conversationId: optional(v.conversation_id),
    terminalConversationId: typeof v.conversation_id === "string" ? v.conversation_id : undefined,
    compactKeys: v.compact_keys,
    cmd: text(v.agent),
    accountProvider: text(v.account_provider) || (v.agent === "deepseek" ? "dsh" : text(v.agent)),
    accountId: text(v.account_id),
    accountHome: text(v.account_home),
    accountReported: v.account_binding_reported ?? (typeof v.account_id === "string" || typeof v.account_home === "string"),
    // A row without a state is running, as Qt's HgsClient reads it.
    state: typeof v.state === "string" ? v.state : "running",
    tracked: !!v.tracked,
    resumable: !!v.resumable,
    activity: text(v.activity),
    processState: text(v.process_state),
    conversationState: text(v.conversation_state),
    archiveId: optional(v.archive_id),
    replyId: optional(v.reply_id),
    attentionSignature: optional(v.attention_signature),
    sharedRead: !!v.shared_read,
    allowed: Array.isArray(v.allowed_actions) ? v.allowed_actions.join(" ") : undefined,
    archiveReason: optional(v.action_reasons?.archive),
    terminateReason: optional(v.action_reasons?.terminate),
    forgetReason: optional(v.action_reasons?.forget),
    restoreReason: optional(v.action_reasons?.restore),
    cwdPath: text(v.cwd),
    cwdCanonical: text(v.cwd_canonical),
    gitRoot: text(v.git_root),
    terminalSupported: v.terminal_supported ?? !["deepseek", "dsh", "opencode"].includes(text(v.agent)),
    terminalReported: !!v.terminal_reported,
    terminalAttachSupported: !!v.terminal_attach_supported,
    terminalAttachReported: !!v.terminal_attach_reported,
    bulkLifecycle: v.bulk_lifecycle_supported ?? text(v.agent) !== "opencode",
    // The host's values the marks overlay reads.
    created: typeof v.created === "number" ? Math.trunc(v.created) : 0,
    fingerprint: text(v.attention_fingerprint),
    needsAction: !!v.needs_action,
    ackStatus: v.acknowledged_status ? statusOf({ name: "", status: v.acknowledged_status }) : status,
    ackCaption: text(v.acknowledged_caption) || caption,
    unreadCandidate: typeof v.unread_candidate === "boolean" ? v.unread_candidate : undefined,
    baseAttentionNeeded: typeof v.needs_attention === "boolean" ? v.needs_attention : undefined,
    baseChildAttention: !!v.child_attention,
    baseNotificationError: !!v.notification_error,
    baseStatus: status,
    baseCaption: caption,
    baseAttention: attention,
    baseUnread: !!v.unread,
    baseSummary: text(v.summary),
    baseMuted: !!v.muted,
    readReplyId: optional(v.read_reply_id),
    localReadReply: !!v.local_read_reply,
    localReadAttention: !!v.local_read_attention,
    readAttentionSignature: optional(v.read_attention_signature),
    sharedNotifications: !!v.shared_notifications,
    notificationRevision: typeof v.notification_revision === "number" ? v.notification_revision : undefined,
    keepWarm: keepWarmOf(v.keep_warm),
  };
  return Object.assign(base, marks.overlay(base));
}

/** How long a `replaced` removal waits for the new run of the same name. */
const REPLACED_MS = 10_000;

/** `marks` given: the catalog applies them and they refresh it. Without, rows get the app's overlay and nothing is attached. */
export function createCatalog(given?: RowMarks, onMachines?: (b: Batch) => void) {
  const marks = given ?? appMarks;
  const [catalog, setCatalog] = createStore<CatalogState>({ rows: {}, order: [], computers: {} });
  // Bumped with every change to the rows, their order or the computers, once
  // per batch: whole-catalog aggregates track this instead of each field of
  // every row (zerus-5wmh.66.50).
  const [revision, setRevision] = createSignal(0);
  const git = createGitProjection({
    row: (id) => catalog.rows[id],
    online: (computer) => catalog.computers[computer]?.status === "live",
    patch: (id, value) => { if (catalog.rows[id]) setCatalog("rows", id, "git", value); },
  });
  const changed = () => setRevision((n) => n + 1);
  // A marks change applies the overlay again to the rows it touched, in place.
  // The marks only read the rows, so they scan the plain objects (no proxy reads).
  given?.attach({ rows: () => unwrap(catalog.rows), patch: (id, fields) => { setCatalog("rows", id, fields); changed(); } });
  // `replaced` removals by computer + name, so the next run of that name can
  // inherit selection and drafts (the remove and its upsert may land in
  // different batches).
  const replaced = new Map<string, { id: string; at: number }>();
  const replacedListeners = new Set<(oldId: string, newId: string) => void>();
  // Current named rows, for an upsert/removal split in either arrival order.
  const currentNames = new Map<string, Set<string>>();
  const appearedAfter = new Map<string, { id: string; name: string }>();
  const nameKey = (row: Session) => JSON.stringify([row.computer, row.name]);
  const forgetCurrent = (id: string, row: Session | undefined) => {
    if (!row) return;
    const identity = nameKey(row), entries = currentNames.get(identity);
    entries?.delete(id);
    if (entries?.size === 0) currentNames.delete(identity);
  };
  const currentSuccessor = (id: string, row: Session): Session | undefined => {
    const successor = appearedAfter.get(id), next = successor && catalog.rows[successor.id];
    return next && successor.name === nameKey(row) && nameKey(next) === successor.name
      && !next.archiveId && next.state !== "archived" ? next : undefined;
  };
  // A removal without a reason (the background service's journal and its
  // baselines keep only each key's final row) hands selection over only as a
  // coalesced full catalog does: to the unique, known same-conversation run of
  // that name, in either delivery order. Insertion order is age order.
  type Orphan = { id: string; computer: ComputerId; name: string; runId: string; conversationId: string; at: number };
  const orphans = new Map<string, Orphan>();
  const conversationKey = (row: Pick<Orphan, "computer" | "name" | "conversationId">) => JSON.stringify([row.computer, row.name, row.conversationId]);
  const knownRun = (row: Session | undefined): row is Session & { runId: string; conversationId: string } =>
    !!row?.runId && !!row.conversationId && !row.archiveId && row.state !== "archived";
  const uniqueSuccessor = (from: Orphan, gone: ReadonlySet<string>): string | undefined => {
    let found: string | undefined;
    for (const id of currentNames.get(JSON.stringify([from.computer, from.name])) ?? []) {
      const next = catalog.rows[id];
      if (id === from.id || gone.has(id) || !knownRun(next) || next.conversationId !== from.conversationId || next.runId === from.runId) continue;
      if (found) return undefined;
      found = id;
    }
    return found;
  };
  const pruneOrphans = (now: number) => {
    for (const [identity, orphan] of orphans) {
      if (now - orphan.at <= REPLACED_MS) break;
      orphans.delete(identity);
    }
  };
  /** The draft keys of the nonempty drafts (Rust's `draft_keys`): a row's `draft` mark. */
  let drafts = new Set<string>(), childDraftParents = new Set<string>();
  // A row keyed by session ID also counts a draft still under its name key,
  // which moves to the ID key when its composer opens.
  const drafted = (row: Pick<Session, "computer" | "catalogKey" | "name" | "sessionId">) => {
    const ref = { computer: row.computer, key: row.catalogKey, name: row.name, archive_id: null, session_id: row.sessionId ?? null };
    return [draftKey(ref), nameDraftKey(ref)].some((owner) => drafts.has(owner) || childDraftParents.has(owner));
  };

  const put = (id: string, next: Session, fresh: string[], delta = true) => {
    next.git = git.project(next);
    next.draft = drafted(next);
    if (delta && !catalog.rows[id] && !next.archiveId && next.state !== "archived") {
      const identity = nameKey(next), previous = currentNames.get(identity);
      if (previous?.size === 1) appearedAfter.set(previous.values().next().value!, { id, name: identity });
    }
    forgetCurrent(id, catalog.rows[id]);
    if (catalog.rows[id]) setCatalog("rows", id, reconcile(next));
    else {
      setCatalog("rows", id, next);
      fresh.push(id);
    }
    if (!next.archiveId && next.state !== "archived") {
      const identity = nameKey(next), entries = currentNames.get(identity) ?? new Set<string>();
      entries.add(id);
      currentNames.set(identity, entries);
    }
  };

  const inheritReplaced = (next: Session) => {
    const named = replaced.get(`${next.computer}\n${next.name}`);
    if (!named || named.id === next.key) return;
    replaced.delete(`${next.computer}\n${next.name}`);
    if (Date.now() - named.at <= REPLACED_MS) for (const listener of replacedListeners) listener(named.id, next.key);
  };

  // The core's order: computers, replaces, removes, upserts (Activity goes to its own store).
  function applyBatch(b: Batch) {
    batch(() => {
      for (const c of b.computers) {
        const retry = c.status === "ended" ? undefined : catalog.computers[c.computer]?.retry;
        setCatalog("computers", c.computer, { id: c.computer, status: c.status, attempt: c.attempt ?? 0, retry });
      }
      const arrived = performance.now();
      for (const r of b.retries ?? []) {
        if (!catalog.computers[r.computer]) continue;
        setCatalog("computers", r.computer, "retry", anchorRetry(catalog.computers[r.computer].retry, r, arrived));
      }
      onMachines?.(b);
      const fresh: string[] = [];
      const gone = new Set<string>();
      const upserted: Session[] = [];
      const resumed = new Map<string, string>();
      for (const r of b.replaces) {
        const keep = new Set<string>();
        const successors = new Map<string, Session | null>();
        for (const row of r.rows) {
          const id = rowId(r.computer, row.key);
          keep.add(id);
          const next = toSession(r.computer, row.key, row.row, marks);
          put(id, next, fresh, false);
          upserted.push(next);
          if (next.runId && next.conversationId && !next.archiveId && next.state !== "archived") {
            const identity = JSON.stringify([next.name, next.conversationId]);
            successors.set(identity, successors.has(identity) ? null : next);
          }
        }
        for (const id of catalog.order) if (computerOf(id) === r.computer && !keep.has(id)) {
          gone.add(id);
          const previous = catalog.rows[id];
          // Coalescing into a full catalog loses the explicit removal reason.
          // Carry selection only to a unique, known same-conversation run.
          if (!previous?.runId || !previous.conversationId || previous.archiveId || previous.state === "archived") continue;
          const next = successors.get(JSON.stringify([previous.name, previous.conversationId]));
          if (next && next.runId !== previous.runId) resumed.set(id, next.key);
        }
      }
      const reasonless: Orphan[] = [];
      const now = Date.now();
      for (const k of b.removes) {
        const id = rowId(k.computer, k.key);
        const row = catalog.rows[id];
        if (!row) continue;
        if (k.reason === "replaced") {
          replaced.set(`${k.computer}\n${row.name}`, { id, at: now });
          const next = currentSuccessor(id, row);
          if (next) resumed.set(id, next.key);
        } else if (!k.reason && knownRun(row)) {
          reasonless.push({ id, computer: row.computer, name: row.name, runId: row.runId, conversationId: row.conversationId, at: now });
        }
        gone.add(id);
      }
      for (const u of b.upserts) {
        const id = rowId(u.computer, u.key);
        const next = toSession(u.computer, u.key, u.row, marks);
        gone.delete(id);
        put(id, next, fresh);
        upserted.push(next);
      }
      // After this batch's upserts, so a successor in the same batch counts.
      pruneOrphans(now);
      const adopted: [string, string][] = [];
      for (const from of reasonless) {
        if (!gone.has(from.id)) continue;
        const to = uniqueSuccessor(from, gone);
        const identity = conversationKey(from);
        orphans.delete(identity);
        if (to) resumed.set(from.id, to);
        else orphans.set(identity, from);
      }
      for (const next of upserted) {
        if (!orphans.size) break;
        if (!knownRun(next)) continue;
        const identity = conversationKey(next), from = orphans.get(identity);
        if (!from || catalog.rows[from.id]) continue;
        const to = uniqueSuccessor(from, gone);
        if (!to) continue;
        orphans.delete(identity);
        adopted.push([from.id, to]);
      }
      if (gone.size || fresh.length) {
        for (const id of gone) { forgetCurrent(id, catalog.rows[id]); appearedAfter.delete(id); }
        setCatalog(produce((s) => {
          for (const id of gone) delete s.rows[id];
          const kept = gone.size ? s.order.filter((id) => !gone.has(id)) : s.order.slice();
          const seen = new Set(kept);
          for (const id of fresh) if (!seen.has(id) && !gone.has(id)) { kept.push(id); seen.add(id); }
          s.order = kept;
        }));
      }
      for (const [from, to] of resumed) {
        const next = catalog.rows[to];
        if (!next || !gone.has(from) || gone.has(to)) continue;
        const identity = `${next.computer}\n${next.name}`;
        if (replaced.get(identity)?.id === from) replaced.delete(identity);
        for (const listener of replacedListeners) listener(from, to);
      }
      for (const [from, to] of adopted) {
        if (catalog.rows[from] || !catalog.rows[to]) continue;
        for (const listener of replacedListeners) listener(from, to);
      }
      for (const next of upserted) inheritReplaced(next);
      git.apply(b.git ?? []);
      if (b.computers.length || b.replaces.length || b.removes.length || b.upserts.length) changed();
    });
  }

  function onReplaced(listener: (oldId: string, newId: string) => void) {
    replacedListeners.add(listener);
    return () => replacedListeners.delete(listener);
  }

  /**
   * The sessions with a nonempty draft (Qt's SessionDelegate draft status):
   * Rust's draft keys, `computer + "\n" + name` for a live session, or
   * `computer + "\nid:" + session ID` once it has one. Rows that arrive later
   * take their mark from the same set.
   */
  function setDraftKeys(keys: readonly string[]) {
    drafts = new Set(keys);
    childDraftParents = new Set(keys.filter((key) => key.split("\n").length === 4).map((key) => key.split("\n").slice(0, 2).join("\n")));
    batch(() => {
      for (const id of catalog.order) {
        const row = catalog.rows[id];
        if (row && row.draft !== drafted(row)) { setCatalog("rows", id, "draft", drafted(row)); changed(); }
      }
    });
  }

  /**
   * The ordered rows as plain objects, for whole-catalog aggregates such as
   * the Overview's tiles: they track only the catalog's revision, never each
   * field of every row, which would be ten subscriptions per row and batch at
   * tens of thousands of sessions. They are the store's own objects: never
   * mutate them. Their fields are not reactive: render a row through
   * `catalog.rows[row.key]`.
   */
  const plainRows = (): Session[] => {
    revision();
    const state = unwrap(catalog);
    return state.order.map((id) => state.rows[id]).filter(Boolean);
  };
  /** The computers as plain objects, tracking only the catalog's revision; never mutate them. */
  const plainComputers = (): CatalogState["computers"] => {
    revision();
    return unwrap(catalog).computers;
  };

  return { catalog, applyBatch, onReplaced, setDraftKeys, hasDraftKey: (key: string) => drafts.has(key), revision, plainRows, plainComputers, gitInterests: git.interests };
}

/** The app's catalog. Tests build their own with `createCatalog`. */
export const { catalog, applyBatch, onReplaced, setDraftKeys, hasDraftKey, plainRows, plainComputers, gitInterests } = createCatalog(appMarks, applyMachines);

/** Rows in display order. */
export const orderedRows = () => catalog.order.map((id) => catalog.rows[id]).filter(Boolean);

/** A computer whose stream is down shows its rows as last known. */
export function computerOffline(computer: ComputerId): boolean {
  const status = catalog.computers[computer]?.status;
  return status === "reconnecting" || status === "ended";
}

/**
 * The window's online state of a computer's rows, as the Overview's: its
 * stream is live or polled. Qt decides both alike: the Sessions list's
 * `entry.online` (SessionsWindow.cpp:3207-3213) is the Dashboard's
 * `machine.fresh` (DashboardPage.cpp:411-412), so a computer still
 * connecting, probing or unsupported is not online.
 */
export function computerFresh(computer: ComputerId): boolean {
  return connectionOf(catalog.computers[computer]?.status).fresh;
}

export type { RowOut };
