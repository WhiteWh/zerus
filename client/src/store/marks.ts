import { bus } from "./notices";
import { tr } from "../i18n";
import { desktopService } from "./desktopService";
import { batch, createSignal } from "solid-js";
import * as bridge from "../bridge";
import type { AttentionSnapshot, Mark } from "../bridge";
import type { Batch, ReadMark, ViewRow } from "../types";
import type { ActionRow } from "../ui/sessions/actions-ui";
import type { Session } from "../ui/sessions/types";

// This device's read state, as Qt's FleetState keeps it (tray/src/FleetState.cpp):
// attention marks (marked for review, the acknowledged attention) and the
// replies read on hosts without shared read state, both saved by the Rust side
// (`attention_marks`, `update_attention_marks`), plus a copy of the read marks
// still waiting for delivery (`queue_read_marks`, `pending_read_marks`). The
// overlay applies them to a catalog row's host values in `toSession`; a change
// applies it again to the affected rows only.
//
// Marks are local first: a click changes the rows at once, saves the changed
// entries and queues a read mark for a host that shares read state. The Rust
// worker delivers it (Qt's ReadMarkSync); a host shows it in a later listing,
// which settles the mark. A mark names the reply, request and attention it was
// made for, so a newer one shows again.

export type { AttentionSnapshot, Mark };

/** Confirmed notification overrides kept; the oldest goes first. */
export const OVERRIDE_CAP = 1000;

/** Entries kept per map; the oldest written goes first (the Rust side caps its copy the same way). */
export const CAP = 20_000;

/** What the overlay decides; the rest of a row is the host's. */
export type Overlay = Pick<Session, "status" | "caption" | "attention" | "unread" | "unreadReply" | "reviewLater" | "attentionNeeded" | "actionNeeded" | "childAttention" | "summary" | "muted" | "notificationConflict">;

export interface MarksPort {
  attentionMarks(): Promise<AttentionSnapshot>;
  updateAttentionMarks(marks: Record<string, Mark | null>, replies: Record<string, string | null>): Promise<void>;
  /** Every pending mark after the merge; undefined when there is no list (outside the app), and the store keeps its own. */
  queueReadMarks(marks: ReadMark[]): Promise<ReadMark[] | undefined>;
  pendingReadMarks(): Promise<ReadMark[] | undefined>;
}

/** The computers whose older marks were queued once (Qt's `attention/sharedReadMigrated`). */
export interface MigratedStorage { load(): string[]; save(computers: string[]): void }

/** The catalog rows the overlay is applied to. */
export interface RowsPort {
  rows(): Record<string, Session>;
  patch(id: string, fields: Overlay): void;
}

/** What the catalog needs from the marks. */
export interface RowMarks {
  overlay(s: Session): Overlay;
  attach(rows: RowsPort): void;
}

type KeyFields = Pick<Session, "computer" | "cmd" | "name" | "created"> & { conversationId?: string };

/** The reply key (`FleetState.cpp` replyKey): computer, agent and conversation. */
export const replyKey = (computer: string, agent: string, conversation: string) => JSON.stringify([computer, agent, conversation]);
/** The attention key of a row without a conversation: computer, agent, name and `created`. */
export const fallbackKey = (s: KeyFields) => JSON.stringify([s.computer, s.cmd, s.name, s.created]);
/** The attention key: the reply key, or the fallback key without a conversation. */
export const attentionKey = (s: KeyFields) => (s.conversationId ? replyKey(s.computer, s.cmd, s.conversationId) : fallbackKey(s));
const replyKeyOf = (s: KeyFields) => replyKey(s.computer, s.cmd, s.conversationId ?? "");
/** The pending marks are kept per computer and session name, as the Rust `ReadMarks`. */
const pendingKey = (computer: string, name: string) => JSON.stringify([computer, name]);

/** ReadMark::merged: a newer mark of the same run and conversation keeps the part it does not set. */
function merged(old: ReadMark | undefined, next: ReadMark): ReadMark {
  if (!old || old.run_id !== next.run_id || old.conversation_id !== next.conversation_id) return next;
  return { ...next, reply_id: next.reply_id || old.reply_id, attention_signature: next.attention_signature || old.attention_signature };
}

const sameMark = (a: ReadMark | undefined, b: ReadMark | undefined) =>
  !!a && !!b && a.computer === b.computer && a.name === b.name && a.run_id === b.run_id && a.conversation_id === b.conversation_id
  && (a.reply_id || "") === (b.reply_id || "") && (a.attention_signature || "") === (b.attention_signature || "");

/** ReadMarks::settle: the row names another run or conversation, or each part is unset, no longer current or read. */
function settled(mark: ReadMark, v: ViewRow): boolean {
  if ((v.run_id ?? "") !== mark.run_id || (v.conversation_id ?? "") !== mark.conversation_id) return true;
  const part = (mine: string | null, current: string | null | undefined, read: string | null | undefined) =>
    !mine || mine !== (current ?? "") || (read ?? "") === mine;
  return part(mark.reply_id, v.reply_id, v.read_reply_id) && part(mark.attention_signature, v.attention_signature, v.read_attention_signature);
}

const bridgePort: MarksPort = {
  attentionMarks: () => bridge.attentionMarks(),
  updateAttentionMarks: (marks, replies) => bridge.updateAttentionMarks(marks, replies),
  queueReadMarks: (marks) => bridge.queueReadMarks(marks),
  pendingReadMarks: () => bridge.pendingReadMarks(),
};

const MIGRATED_KEY = "zerus-client-read-migrated";

/** The webview's local storage, like the client settings until the QSettings migration (zerus-5wmh.8). */
function browserStorage(): MigratedStorage {
  const storage = () => { try { return typeof window === "undefined" ? undefined : window.localStorage; } catch { return undefined; } };
  return {
    load() {
      try {
        const value: unknown = JSON.parse(storage()?.getItem(MIGRATED_KEY) ?? "[]");
        return Array.isArray(value) ? value.filter((c): c is string => typeof c === "string") : [];
      } catch { return []; }
    },
    save(computers) { try { storage()?.setItem(MIGRATED_KEY, JSON.stringify(computers)); } catch { /* storage may be unavailable */ } },
  };
}

/** What one change touched: the entries to save and the rows to overlay again. */
interface Change {
  marks: Record<string, Mark | null>;
  replies: Record<string, string | null>;
  /** The fields each written mark sets (Mark needs attention sets only `review`). */
  fields: Record<string, Partial<Mark>>;
  saved: boolean;
  /** Rows to overlay again, by computer and name or by computer and conversation (a superset is harmless). */
  names: Set<string>;
  conversations: Set<string>;
  all: boolean;
}
const change = (): Change => ({ marks: {}, replies: {}, fields: {}, saved: false, names: new Set(), conversations: new Set(), all: false });

/** Rust's string order (UTF-8 bytes, that is code points): the order a `BTreeMap` delta is applied in. */
export function byCodePoint(a: string, b: string): number {
  const unit = (u: number) => (u >= 0xd800 && u <= 0xdfff ? u + 0x2000 : u >= 0xe000 ? u - 0x800 : u);
  for (let i = 0; i < a.length && i < b.length; i++) {
    const x = a.charCodeAt(i), y = b.charCodeAt(i);
    if (x !== y) return unit(x) - unit(y);
  }
  return a.length - b.length;
}

/** The entries `keys` names become the newest, in key order, as one Rust update writes them. */
function newestInKeyOrder<V>(map: Map<string, V>, keys: string[]) {
  for (const key of [...keys].sort(byCodePoint)) {
    const value = map.get(key);
    if (value === undefined) continue;
    map.delete(key);
    map.set(key, value);
  }
}
const pair = (computer: string, value: string) => `${computer}\n${value}`;

/** The rows a key belongs to: a pending key names the session, a reply key its conversation, a fallback key its name. */
function touch(c: Change, key: string) {
  let value: unknown;
  try { value = JSON.parse(key); } catch { value = undefined; }
  if (!Array.isArray(value) || typeof value[0] !== "string") { c.all = true; return; }
  if (value.length === 2) c.names.add(pair(value[0], String(value[1])));
  else if (value.length === 3) c.conversations.add(pair(value[0], String(value[2])));
  else if (value.length === 4) c.names.add(pair(value[0], String(value[2])));
  else c.all = true;
}

export function createMarks(port: MarksPort = bridgePort, storage: MigratedStorage = browserStorage(), service = desktopService) {
  /** Both maps in write order, oldest first. */
  let marks = new Map<string, Mark>();
  let replies = new Map<string, string>();
  let pending = new Map<string, ReadMark>();
  let loaded = false;
  /**
   * Changes made before the stored marks arrived, applied over them and sent
   * with the load: a mark as the fields it sets, so an early Mark needs
   * attention keeps a stored acknowledgement.
   */
  const early = { marks: new Map<string, Partial<Mark> | null>(), replies: new Map<string, string | null>() };
  /** Fallback keys a row with a conversation still finds its mark under, and that key. */
  const moves = new Map<string, string>();
  /** The last pending request; an older answer never replaces a newer local change. */
  let pendingRequest = 0;
  let migrated: Set<string> | undefined;
  let target: RowsPort | undefined;

  /** Keeps the newest `CAP` entries; the rows of a dropped entry get the overlay again. */
  function cap<V>(map: Map<string, V>, c: Change) {
    if (map.size <= CAP) return;
    for (const key of map.keys()) {
      if (map.size <= CAP) break;
      map.delete(key);
      touch(c, key);
    }
  }

  function writeMark(key: string, mark: Mark, c: Change, fields: Partial<Mark> = mark) {
    const old = marks.get(key);
    if (old && old.review === mark.review && old.acknowledged === mark.acknowledged) return;
    marks.delete(key);
    marks.set(key, mark);
    c.marks[key] = mark;
    c.fields[key] = { ...c.fields[key], ...fields };
    c.saved = true;
    touch(c, key);
    cap(marks, c);
  }

  function removeMark(key: string, c: Change) {
    if (!marks.delete(key)) return;
    c.marks[key] = null;
    delete c.fields[key];
    c.saved = true;
    touch(c, key);
  }

  function writeReply(key: string, reply: string, c: Change) {
    if (replies.get(key) === reply) return;
    replies.delete(key);
    replies.set(key, reply);
    c.replies[key] = reply;
    c.saved = true;
    touch(c, key);
    cap(replies, c);
  }

  /** FleetState::updateUnread for one row: the review mark, the reply's read state and the acknowledged attention. */
  function readState(s: Session) {
    // Runs for every upserted row: keys are built only when a map has entries.
    let mark: Mark | undefined;
    if (marks.size) {
      const key = attentionKey(s);
      mark = marks.get(key);
      if (s.conversationId) {
        const fallback = fallbackKey(s);
        if (marks.has(fallback)) { moves.set(fallback, key); mark ??= marks.get(fallback); }
      }
    }
    const reviewLater = s.state !== "archived" && !!mark?.review;
    const localAck = !!s.fingerprint && mark?.acknowledged === s.fingerprint;
    const reply = s.replyId ?? "";
    const current = s.state !== "archived" && !!s.conversationId && !!reply;
    let unreadReply: boolean;
    let ack: boolean;
    if (!s.sharedRead) {
      // An older host keeps this device's own read state.
      unreadReply = current && (!replies.size || replies.get(replyKeyOf(s)) !== reply);
      ack = localAck;
    } else {
      const queued = pending.size ? pending.get(pendingKey(s.computer, s.name)) : undefined;
      const sameRun = !!queued?.run_id && queued.run_id === (s.runId ?? "") && queued.conversation_id === (s.conversationId ?? "");
      unreadReply = current && !(s.localReadReply || (s.readReplyId ?? "") === reply || (sameRun && (queued!.reply_id ?? "") === reply));
      const signature = s.attentionSignature ?? "";
      // Recovery-only attention has no host signature and stays a local choice.
      ack = !signature ? localAck
        : !!s.localReadAttention || signature === (s.readAttentionSignature ?? "") || (sameRun && (queued!.attention_signature ?? "") === signature);
    }
    // Shown unread once the reply is finished (Qt's visibleUnreadReply); a
    // projection without the field keeps the row's own value.
    const unread = s.unreadCandidate === undefined ? s.baseUnread : unreadReply && s.unreadCandidate;
    return { reviewLater, unreadReply, unread, ack };
  }

  /**
   * Confirmed notification preferences by reply key (computer, agent,
   * conversation), oldest written first and bounded: what the host confirmed
   * for a change, which an older listing must not roll back (Qt's
   * keepNotificationPreferences). Only the revision and `muted` are kept: the
   * row carries no more of the preferences.
   *
   * The core now applies the confirmed preferences to the row and keeps them
   * across older listings itself (`CatalogState::apply_notification_preferences`
   * and the keep step in `state.rs`), for the window and the menu bar alike,
   * so these overrides are redundant there. They stay as a harmless second
   * guard until the core's path has run in the field.
   */
  const overrides = new Map<string, { revision: number; muted: boolean }>();
  const overrideKey = (s: Pick<Session, "computer" | "cmd" | "conversationId">) => replyKey(s.computer, s.cmd, s.conversationId ?? "");

  /**
   * keepNotificationPreferences for one row. A host that no longer reports shared
   * notifications keeps the last confirmed display. A row with a higher
   * revision wins and ends the override; a lower one is ignored; the same
   * revision with another value keeps the override and is a conflict.
   */
  function notificationOf(s: Session): { muted: boolean; conflict: boolean } {
    const base = { muted: s.baseMuted, conflict: s.baseNotificationError };
    const kept = overrides.size && s.conversationId ? overrides.get(overrideKey(s)) : undefined;
    if (!kept) return base;
    // The core's conflict shows whatever the override says.
    if (!s.sharedNotifications) return { muted: kept.muted, conflict: base.conflict };
    const incoming = s.notificationRevision ?? -1;
    if (incoming > kept.revision) return base;
    if (incoming === kept.revision) return s.baseMuted === kept.muted ? base : { muted: kept.muted, conflict: true };
    return { muted: kept.muted, conflict: base.conflict };
  }

  const mutedOf = (s: Session) => notificationOf(s).muted;

  function overlay(s: Session): Overlay {
    const r = readState(s);
    const status = r.ack ? s.ackStatus : s.baseStatus;
    return {
      status,
      caption: r.ack ? s.ackCaption : s.baseCaption,
      attention: status === "attention" ? s.baseAttention : undefined,
      unread: r.unread,
      unreadReply: r.unreadReply,
      reviewLater: r.reviewLater,
      // SessionInfo::needsAttention with these marks. The core's `needs_attention`
      // is the listing's value before any device mark: an older host's reply is
      // never unread there, so it must never be read without this overlay. A
      // projection without the field keeps none.
      attentionNeeded: s.baseAttentionNeeded === undefined ? undefined
        : (s.needsAction && !r.ack) || (s.state !== "archived" && (r.unread || r.reviewLater)),
      // The part a project header counts as needing a response (SessionsWindowRows.cpp:400-401).
      actionNeeded: s.baseAttentionNeeded === undefined ? undefined : s.needsAction && !r.ack,
      childAttention: s.baseChildAttention && !r.ack,
      // Qt's currentAction repeats the status for an unread reply unless the session needs action.
      summary: r.unread && !s.needsAction ? "" : s.baseSummary,
      muted: mutedOf(s),
      notificationConflict: notificationOf(s).conflict,
    };
  }

  /** Applies the overlay again to the rows that match (after a confirmed notification override). */
  function reapply(match: (s: Session) => boolean) {
    bump();
    if (!target) return;
    const rows = target.rows();
    batch(() => { for (const id in rows) if (match(rows[id])) target!.patch(id, overlay(rows[id])); });
  }

  /** Applies the overlay again to the rows a change touched (every row after a load or a cap). */
  function refresh(c: Change) {
    if (!target || (!c.all && !c.names.size && !c.conversations.size)) return;
    const rows = target.rows();
    batch(() => {
      for (const id in rows) {
        const s = rows[id];
        if (!c.all && !c.names.has(pair(s.computer, s.name)) && !(s.conversationId && c.conversations.has(pair(s.computer, s.conversationId)))) continue;
        target!.patch(id, overlay(s));
      }
    });
  }

  // The read state lives in plain maps and the catalog is read unwrapped, so
  // Solid tracks nothing there: readers of `unreadReplies` track this version,
  // which every change, load, observed batch and pending-mark reload bumps.
  const [version, setVersion] = createSignal(0);
  const bump = () => { setVersion((v) => v + 1); };

  /** Every listed row, or the rows of some computers. */
  function listed(computers?: Set<string>): Session[] {
    if (!target) return [];
    const rows = target.rows();
    const out: Session[] = [];
    for (const id in rows) if (!computers || computers.has(rows[id].computer)) out.push(rows[id]);
    return out;
  }

  /** FleetState::queueRead: the parts of a shared-read row its host has not read yet, merged with the pending mark. */
  function queueRead(s: Session, reply: boolean, attention: boolean, fresh: ReadMark[], c: Change) {
    if (!s.sharedRead || s.state === "archived" || !s.runId || !s.conversationId) return;
    const mark: ReadMark = {
      computer: s.computer, name: s.name, run_id: s.runId, conversation_id: s.conversationId,
      reply_id: reply && (s.readReplyId ?? "") !== (s.replyId ?? "") ? s.replyId || null : null,
      attention_signature: attention && (s.readAttentionSignature ?? "") !== (s.attentionSignature ?? "") ? s.attentionSignature || null : null,
    };
    enqueue(mark, fresh, c);
  }

  function enqueue(mark: ReadMark, fresh: ReadMark[], c: Change) {
    if (!mark.reply_id && !mark.attention_signature) return;
    const key = pendingKey(mark.computer, mark.name);
    const next = merged(pending.get(key), mark);
    pending.set(key, next);
    fresh.push(next);
    c.names.add(pair(mark.computer, mark.name));
  }

  /** Takes a pending list from the Rust side; the rows whose mark changed get the overlay again. */
  function replacePending(list: unknown) {
    if (!Array.isArray(list)) return;
    const next = new Map<string, ReadMark>();
    for (const m of list as ReadMark[]) {
      if (!m || typeof m.name !== "string" || typeof m.computer !== "string") continue;
      next.set(pendingKey(m.computer, m.name), {
        computer: m.computer, name: m.name, run_id: m.run_id ?? "", conversation_id: m.conversation_id ?? "",
        reply_id: m.reply_id || null, attention_signature: m.attention_signature || null,
      });
    }
    const c = change();
    for (const [key, m] of next) if (!sameMark(pending.get(key), m)) c.names.add(pair(m.computer, m.name));
    for (const [key, m] of pending) if (!next.has(key)) c.names.add(pair(m.computer, m.name));
    pending = next;
    bump();
    refresh(c);
  }

  /** Reads the pending marks from the Rust side; without a list (outside the app) the mirror stays. */
  async function loadPending() {
    const request = ++pendingRequest;
    try {
      const list = await port.pendingReadMarks();
      if (request === pendingRequest && list) replacePending(list);
    } catch { /* a build without the command keeps what it has */ }
  }

  function send(fresh: ReadMark[]) {
    if (!fresh.length) return;
    const request = ++pendingRequest;
    port.queueReadMarks(fresh).then(
      (list) => { if (request === pendingRequest && list) replacePending(list); },
      // The Rust side's own list is what will be delivered.
      () => { if (request === pendingRequest) void loadPending(); },
    );
  }

  function save(c: Change) {
    if (!c.saved) return;
    if (!loaded) {
      for (const [key, mark] of Object.entries(c.marks)) {
        const before = early.marks.get(key);
        early.marks.delete(key);
        // A removal, or the fields written over the earlier ones (a removal before them stays a full mark).
        early.marks.set(key, mark === null ? null : before === null ? mark : { ...before, ...c.fields[key] });
      }
      for (const [key, reply] of Object.entries(c.replies)) { early.replies.delete(key); early.replies.set(key, reply); }
      return;
    }
    // The Rust side writes one update's entries in key order; so does this copy.
    newestInKeyOrder(marks, Object.keys(c.marks));
    newestInKeyOrder(replies, Object.keys(c.replies));
    port.updateAttentionMarks(c.marks, c.replies).catch((error) => console.warn("Zerus Next: saving attention marks failed", error));
  }

  function commit(c: Change, fresh: ReadMark[] = []) {
    bump();
    save(c);
    send(fresh);
    refresh(c);
  }

  /** A mark under the fallback key moves to the conversation key (`FleetState.cpp:136-139`). */
  function moveFallbacks(c: Change) {
    for (const [fallback, key] of moves) {
      const mark = marks.get(fallback);
      if (!mark) continue;
      if (!marks.has(key)) writeMark(key, mark, c);
      removeMark(fallback, c);
    }
    moves.clear();
  }

  /**
   * FleetState::legacyReadMarks, once per computer (TrayAgent::migrateReadMarks):
   * `sharing` names computers whose listed rows show shared read state.
   */
  function migrateLegacy(sharing: Set<string>, fresh: ReadMark[], c: Change) {
    if (service.enabled() || !loaded || !sharing.size) return;
    migrated ??= new Set(storage.load());
    const due = new Set([...sharing].filter((computer) => !migrated!.has(computer)));
    if (!due.size) return;
    const byComputer = new Map<string, Session[]>([...due].map((computer) => [computer, []]));
    for (const s of listed(due)) byComputer.get(s.computer)!.push(s);
    let changed = false;
    for (const [computer, rows] of byComputer) {
      if (!rows.some((s) => s.sharedRead)) continue;
      for (const s of rows) {
        if (!s.sharedRead || s.state === "archived" || !s.runId || !s.conversationId) continue;
        // Only marks for the current values can still apply on the host.
        const reply = !!s.replyId && s.readReplyId !== s.replyId && replies.get(replyKeyOf(s)) === s.replyId;
        const attention = !!s.attentionSignature && s.readAttentionSignature !== s.attentionSignature
          && !!s.fingerprint && marks.get(attentionKey(s))?.acknowledged === s.fingerprint;
        enqueue({
          computer, name: s.name, run_id: s.runId, conversation_id: s.conversationId,
          reply_id: reply ? s.replyId! : null, attention_signature: attention ? s.attentionSignature! : null,
        }, fresh, c);
      }
      migrated.add(computer);
      changed = true;
    }
    if (changed) storage.save([...migrated]);
  }

  /**
   * A stored map in its write order, oldest first: the keys the order lists,
   * after any it does not (kept as the oldest).
   */
  function inWriteOrder<V>(entries: Record<string, unknown> | undefined, order: unknown, read: (value: unknown) => V | undefined): Map<string, V> {
    const values = new Map<string, V>();
    for (const [key, value] of Object.entries(entries ?? {})) {
      const v = read(value);
      if (v !== undefined) values.set(key, v);
    }
    const listed = Array.isArray(order) ? order.filter((key): key is string => typeof key === "string" && values.has(key)) : [];
    const listedSet = new Set(listed);
    const out = new Map<string, V>();
    for (const [key, value] of values) if (!listedSet.has(key)) out.set(key, value);
    for (const key of listed) { out.delete(key); out.set(key, values.get(key)!); }
    return out;
  }

  /** The stored mark of a key, as the snapshot holds it. */
  function marksBefore(key: string, stored: AttentionSnapshot | undefined): Partial<Mark> {
    const value: unknown = stored?.marks?.[key];
    if (!value || typeof value !== "object") return {};
    const mark = value as Partial<Mark>;
    return { review: mark.review === true, acknowledged: typeof mark.acknowledged === "string" ? mark.acknowledged : "" };
  }

  /** Loads the stored marks and the pending read marks; changes made meanwhile win and are saved then. */
  async function load() {
    const pendingLoad = loadPending();
    let stored: AttentionSnapshot | undefined;
    try { stored = await port.attentionMarks(); } catch { stored = undefined; }
    const c = change();
    const nextMarks = inWriteOrder(stored?.marks, stored?.order?.marks, (value) => value && typeof value === "object"
      ? { review: (value as Mark).review === true, acknowledged: typeof (value as Mark).acknowledged === "string" ? (value as Mark).acknowledged : "" }
      : undefined);
    const nextReplies = inWriteOrder(stored?.replies, stored?.order?.replies, (value) => (typeof value === "string" ? value : undefined));
    // As Rust's update: the value an entry holds already is no change and keeps its place.
    const delta = change();
    for (const [key, fields] of early.marks) {
      const before = nextMarks.get(key);
      if (fields === null) {
        if (nextMarks.delete(key)) delta.marks[key] = null;
        continue;
      }
      const mark: Mark = { review: false, acknowledged: "", ...marksBefore(key, stored), ...fields };
      if (before && before.review === mark.review && before.acknowledged === mark.acknowledged) continue;
      nextMarks.delete(key);
      nextMarks.set(key, mark);
      delta.marks[key] = mark;
    }
    for (const [key, reply] of early.replies) {
      const before = nextReplies.get(key);
      if (reply === null) {
        if (nextReplies.delete(key)) delta.replies[key] = null;
        continue;
      }
      if (before === reply) continue;
      nextReplies.delete(key);
      nextReplies.set(key, reply);
      delta.replies[key] = reply;
    }
    delta.saved = Object.keys(delta.marks).length > 0 || Object.keys(delta.replies).length > 0;
    early.marks.clear();
    early.replies.clear();
    marks = nextMarks;
    replies = nextReplies;
    loaded = true;
    // The early changes go as one update, which the Rust side applies before its cap.
    save(delta);
    cap(marks, c);
    cap(replies, c);
    c.all = true;
    refresh(c);
    const after = change();
    const fresh: ReadMark[] = [];
    moveFallbacks(after);
    migrateLegacy(new Set(listed().filter((s) => s.sharedRead).map((s) => s.computer)), fresh, after);
    commit(after, fresh);
    await pendingLoad;
  }

  /**
   * After the catalog applied a batch: settles pending marks (ReadMarks::settle
   * and Actions::observe), moves fallback marks of rows that now have a
   * conversation and queues a computer's older marks once it shares read state.
   */
  function observe(b: Batch) {
    const c = change();
    const fresh: ReadMark[] = [];
    if (pending.size) {
      for (const u of b.replaces.length ? [...b.upserts, ...b.replaces.flatMap((r) => r.rows)] : b.upserts) {
        const key = pendingKey(u.computer, u.row.name);
        const mark = pending.get(key);
        if (mark && settled(mark, u.row)) { pending.delete(key); c.names.add(pair(u.computer, u.row.name)); }
      }
      // An empty replace is not a listing: a stopped stream sends one.
      for (const r of b.replaces) {
        if (!r.rows.length) continue;
        const names = new Set(r.rows.map((u) => u.row.name));
        for (const [key, mark] of pending) if (mark.computer === r.computer && !names.has(mark.name)) { pending.delete(key); c.names.add(pair(r.computer, mark.name)); }
      }
    }
    if (overrides.size) {
      // A listing that reaches the override ends it: a higher revision, or the
      // same revision with the same value (the overlay already shows the row's
      // own value then). The core keeps the confirmed value from here on.
      for (const u of b.replaces.length ? [...b.upserts, ...b.replaces.flatMap((r) => r.rows)] : b.upserts) {
        const row = u.row;
        const key = row.conversation_id ? replyKey(u.computer, row.agent ?? "", row.conversation_id) : undefined;
        const kept = key ? overrides.get(key) : undefined;
        const revision = row.notification_revision ?? -1;
        if (kept && row.shared_notifications && (revision > kept.revision || (revision === kept.revision && !!row.muted === kept.muted))) {
          overrides.delete(key!);
        }
      }
      // A conversation no longer listed takes its override with it, as Qt's
      // preferences go with their rows. An empty replace is not a listing.
      if (overrides.size && (b.removes.length || b.replaces.some((r) => r.rows.length))) {
        const listedKeys = new Set(listed().filter((s) => s.conversationId).map((s) => overrideKey(s)));
        for (const key of [...overrides.keys()]) if (!listedKeys.has(key)) overrides.delete(key);
      }
    }
    moveFallbacks(c);
    const sharing = new Set<string>();
    for (const u of b.upserts) if (u.row.shared_read) sharing.add(u.computer);
    for (const r of b.replaces) if (r.rows.some((u) => u.row.shared_read)) sharing.add(r.computer);
    migrateLegacy(sharing, fresh, c);
    commit(c, fresh);
  }

  /** FleetState::matchingSession: the listed row of that name, archive, conversation and `created`, not archived. */
  function matching(row: ActionRow): Session | undefined {
    return listed(new Set([row.computer])).find((s) => s.name === row.name && (s.archiveId ?? "") === (row.archiveId ?? "")
      && (s.conversationId ?? "") === (row.conversationId ?? "") && s.created === row.created && s.state !== "archived");
  }

  /**
   * FleetState::markSessionRead: only while the listed row still has the
   * reply, attention and signature the menu showed. Reads the reply (queued for
   * a shared-read host, kept here for an older one) and acknowledges the
   * attention. False when the row changed.
   */
  // Bounded user-intent fanout, not a background read-mark delivery worker.
  // Dwell and menu repeats coalesce; at most two service RPCs run at once.
  const serviceReadKeys = new Set<string>();
  const serviceReadQueue: (() => Promise<void>)[] = [];
  let serviceReadActive = 0;
  function drainServiceReads() {
    while (serviceReadActive < 2 && serviceReadQueue.length) {
      serviceReadActive++;
      void serviceReadQueue.shift()!().finally(() => { serviceReadActive--; drainServiceReads(); });
    }
  }
  /** Service mode has no GUI read producer or optimistic acknowledgement. */
  function serviceRead(selected: Session, attention: boolean): boolean {
    // Catalog rows are reactive objects updated in place. Capture the exact
    // reply/run before awaiting an actor receipt.
    const s = { ...selected };
    if (!service.ready("reads") || !s.sharedRead || !s.runId || !s.conversationId) return false;
    const mark: ReadMark = { computer: s.computer, name: s.name, run_id: s.runId, conversation_id: s.conversationId,
      reply_id: s.replyId || null, attention_signature: attention ? s.attentionSignature || null : null };
    if (!mark.reply_id && !mark.attention_signature) return false;
    const identity = JSON.stringify(mark);
    if (serviceReadKeys.has(identity)) return false;
    if (serviceReadKeys.size >= 128) {
      bus.notify("error", tr("Too many read actions are pending. Wait for the background service and try again."), true, "desktop-service-read");
      return false;
    }
    serviceReadKeys.add(identity);
    serviceReadQueue.push(async () => {
      try {
        if (!service.ready("reads")) throw Error(tr("Background service unavailable. Retry to reconnect."));
        const committed = await port.queueReadMarks([mark]);
        if (!committed?.some(m => sameMark(m, mark))) return;
        const current = matching(s);
        if (!current || current.runId !== mark.run_id || current.replyId !== s.replyId || current.attentionSignature !== s.attentionSignature) return;
        const c = change(), fresh: ReadMark[] = [];
        enqueue(mark, fresh, c);
        if (s.replyId) writeReply(replyKeyOf(s), s.replyId, c);
        if (attention) writeMark(attentionKey(s), { review: false, acknowledged: s.fingerprint }, c);
        // Actor commit permits the local overlay. Delivery and host observation
        // remain separate; the catalog later settles this pending copy.
        commit(c);
      } catch (error) {
        bus.notify("error", String(error), true, "desktop-service-read");
      } finally { serviceReadKeys.delete(identity); }
    });
    drainServiceReads();
    return true;
  }

  // Cutover requires importing every frozen next_pending_reads mark (zerus-5wmh.66.3.31.6); this overlay never delivers it.
  const sharedReadNeeded = (s: Session) => s.sharedRead && !!(
    (s.replyId && !s.localReadReply && s.readReplyId !== s.replyId) || (s.attentionSignature && !s.localReadAttention && s.readAttentionSignature !== s.attentionSignature)
  );

  /** A current row can be unread because the service is unavailable; that is
   * distinct from an identity changing under an open menu. Older hosts keep
   * their device-local read overlay and need no host writer at all. */
  function markReadUnavailable(row: ActionRow): string | undefined {
    if (!service.enabled()) return;
    const s = matching(row);
    if (!s || s.replyId !== row.replyId || s.fingerprint !== row.fingerprint || s.attentionSignature !== row.attentionSignature || !sharedReadNeeded(s)) return;
    if (!s.runId || !s.conversationId) return tr("This host has no shared read state for this session.");
    if (!service.ready("reads")) return tr("Read marks are unavailable until the background service is ready.");
  }

  function markRead(row: ActionRow): boolean {
    const s = matching(row);
    if (!s || (s.replyId ?? "") !== (row.replyId ?? "") || s.fingerprint !== row.fingerprint
      || (s.attentionSignature ?? "") !== (row.attentionSignature ?? "")) return false;
    if (service.enabled() && sharedReadNeeded(s)) return serviceRead(s, true);
    const before = readState(s);
    const c = change();
    const fresh: ReadMark[] = [];
    queueRead(s, true, true, fresh, c);
    // markReplyRead, as the row showed before this mark.
    if (before.unreadReply) writeReply(replyKeyOf(s), s.replyId!, c);
    moveFallbacks(c);
    writeMark(attentionKey(s), { review: false, acknowledged: s.fingerprint }, c);
    // The overlay of this row again, also when no entry changed.
    c.names.add(pair(s.computer, s.name));
    commit(c, fresh);
    return true;
  }

  /**
   * FleetState::markReplyRead: reads exactly this reply of the listed,
   * unarchived session of that name and conversation while it is still the
   * row's unread reply. Only the reply: the attention, its acknowledgement and
   * Review later stay. Queued for a shared-read host, kept here for an older
   * one. False when nothing matched (a newer reply, another conversation).
   */
  function markReplyRead(computer: string, name: string, conversation: string, reply: string): boolean {
    if (!reply || !conversation) return false;
    const s = listed(new Set([computer])).find((s) => s.state !== "archived" && s.name === name
      && (s.conversationId ?? "") === conversation && (s.replyId ?? "") === reply && readState(s).unreadReply);
    if (!s) return false;
    if (service.enabled() && s.sharedRead) return serviceRead(s, false);
    const c = change();
    const fresh: ReadMark[] = [];
    writeReply(replyKeyOf(s), reply, c);
    queueRead(s, true, false, fresh, c);
    c.names.add(pair(s.computer, s.name));
    commit(c, fresh);
    return true;
  }

  /** FleetState::setReviewLater(..., true): false when the row is gone. */
  function markNeedsAttention(row: ActionRow): boolean {
    const s = matching(row);
    if (!s) return false;
    const c = change();
    moveFallbacks(c);
    const key = attentionKey(s);
    writeMark(key, { review: true, acknowledged: marks.get(key)?.acknowledged ?? "" }, c, { review: true });
    commit(c);
    return true;
  }

  /**
   * The listed row a notification change was started for: the same name,
   * archive, conversation, `created` and run, not archived (the filter of
   * SessionsWindow's notificationsFinished). Undefined for a replaced run.
   */
  function currentNotificationRow(row: ActionRow): Session | undefined {
    const s = matching(row);
    return s && (s.runId ?? "") === (row.runId ?? "") ? s : undefined;
  }

  /**
   * The revision to send as `expected_revision`: the row's, or the confirmed
   * override's when that is higher (an Unmute before the next listing).
   */
  function notificationRevisionOf(row: ActionRow): number | undefined {
    const s = currentNotificationRow(row);
    const kept = s?.conversationId ? overrides.get(overrideKey(s)) : undefined;
    const own = row.notificationRevision;
    if (kept === undefined) return own;
    return own === undefined ? kept.revision : Math.max(own, kept.revision);
  }

  /**
   * FleetState::applyNotificationPreferences: keeps the host's confirmed
   * preferences as an override. Refused (false) for a row that no longer shares
   * notifications, an invalid reply, a lower revision than the row shows, or
   * the same revision with another value.
   */
  function applyNotifications(row: ActionRow, preferences: unknown): boolean {
    const s = currentNotificationRow(row);
    const p = preferences && typeof preferences === "object" ? (preferences as Record<string, unknown>) : undefined;
    if (!s || !p || !s.sharedNotifications || !s.runId || !s.conversationId || p.conversation_id !== s.conversationId
      || typeof p.muted !== "boolean" || typeof p.revision !== "number" || !Number.isSafeInteger(p.revision) || p.revision < 0) return false;
    const kept = overrides.get(overrideKey(s));
    const current = Math.max(s.notificationRevision ?? -1, kept?.revision ?? -1);
    const muted = notificationOf(s).muted;
    if (p.revision < current || (p.revision === current && p.muted !== muted)) return false;
    const key = overrideKey(s);
    overrides.delete(key);
    overrides.set(key, { revision: p.revision, muted: p.muted });
    while (overrides.size > OVERRIDE_CAP) overrides.delete(overrides.keys().next().value!);
    reapply((r) => r.computer === s.computer && r.cmd === s.cmd && r.conversationId === s.conversationId);
    return true;
  }

  /** FleetState::unreadReplies: every unread reply, hidden rows included, except rows marked for review. */
  function unreadReplies(): Map<string, string> {
    version();
    const out = new Map<string, string>();
    for (const s of listed()) {
      const r = readState(s);
      if (r.unreadReply && !r.reviewLater) out.set(replyKeyOf(s), s.replyId!);
    }
    return out;
  }

  /**
   * FleetState::markRepliesRead: marks each reply of the snapshot that is still
   * the row's reply. A row marked for review keeps its reply unread. Returns
   * how many it marked.
   */
  function markAllRead(snapshot: Map<string, string> = unreadReplies()): number {
    const c = change();
    const fresh: ReadMark[] = [];
    let count = 0;
    // Each row as it was before this pass, as Qt reads its stored row fields.
    for (const [s, r] of listed().map((s) => [s, readState(s)] as const)) {
      if (!r.unreadReply || r.reviewLater) continue;
      const key = replyKeyOf(s);
      if (snapshot.get(key) !== s.replyId) continue;
      if (!s.sharedRead && replies.get(key) === s.replyId) continue;
      if (service.enabled() && s.sharedRead) { if (serviceRead(s, false)) count++; continue; }
      writeReply(key, s.replyId!, c);
      queueRead(s, true, false, fresh, c);
      count++;
    }
    commit(c, fresh);
    return count;
  }

  return {
    overlay,
    attach(rows: RowsPort) { target = rows; },
    load,
    loadPending,
    observe,
    markRead,
    markReadUnavailable,
    markReplyRead,
    markNeedsAttention,
    unreadReplies,
    markAllRead,
    currentNotificationRow,
    notificationRevisionOf,
    applyNotifications,
    /** The confirmed overrides held (tests, diagnostics). */
    overrideCount: () => overrides.size,
    reapply,
    /** The pending read marks the rows read (a copy). */
    pending: (): ReadMark[] => [...pending.values()],
    /** FleetState::updateUnread for one row; the Rust overlay (menubar::read_state) reads the same fixture (tests). */
    readState,
    /** Each map's keys, oldest written first (tests, diagnostics). */
    writeOrder: () => ({ marks: [...marks.keys()], replies: [...replies.keys()] }),
  };
}

export type Marks = ReturnType<typeof createMarks>;

/** The app's marks; the app catalog applies them. Tests build their own with `createMarks`. */
export const marks = createMarks();
export const { overlay, markReadUnavailable, markRead, markReplyRead, markNeedsAttention, unreadReplies, markAllRead, loadPending } = marks;
/** Hands a batch the catalog just applied to the marks (session.ts). */
export const observeMarks = marks.observe;
void marks.load();
