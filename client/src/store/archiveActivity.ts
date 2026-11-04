import { batch, createEffect, createMemo, createRoot, createSignal, on, onCleanup, untrack } from "solid-js";
import * as bridge from "../bridge";
import { seqOf } from "./activity";
import { errorText } from "./actions";
import { computerOffline } from "./catalog";
import { activityReadOnly, type ActivitySource } from "./activitySource";
import { page, selected, selectedKey } from "./ui";
import { pageHidden } from "./visibility";
import type { ActivityEvent, ArchivedActivity, ComputerId } from "../types";
import { tr } from "../i18n";

// The recorded Activity of the selected archived row, after Qt's
// SessionsWindow::inspect (3467-3492) and acceptInspection (3587-3660). An
// archived row is never streamed: while it is selected and the Sessions page
// shows, it is read with `hgs inspect NAME --after CURSOR --skip-processes
// --archive ID` (bridge.archivedActivity) every 2500 ms, one read at a time.
// Only the selected row is read and kept; leaving it, another page or a
// hidden window drops everything, and the next read starts at cursor 0.
// While its computer is offline nothing is read and the events stay. The
// pane is read-only: no older pages, no end, no live state.

/** Qt's m_activityPollMs: a read starts this long after the previous one started, once that one ended. */
export const ARCHIVED_POLL_MS = 2500;
/** Qt's 500 ms timer: a read refused as busy is tried again at its next tick. */
export const ARCHIVED_RETRY_MS = 500;
/** At most this many events, the oldest dropped first (acceptInspection 3651). */
export const ARCHIVED_EVENTS_MAX = 500;

/** The archived row read: its row id (computer + "\n" + `archive:ID`), computer, name and archive id. */
export interface ArchiveTarget {
  id: string;
  computer: ComputerId;
  name: string;
  archiveId: string;
}

export interface ArchiveActivityDeps {
  /** One read after `after`; rejects with ARCHIVED_ACTIVITY_BUSY while the same row's read runs, else with Qt's failure text. */
  read(computer: ComputerId, name: string, archiveId: string, after: number): Promise<ArchivedActivity>;
  /** The window shows and the Sessions page is current (reactive). */
  showing(): boolean;
  /** The computer is online (reactive); Qt's inspect() reads nothing while it is offline. Always, when left out. */
  online?(computer: ComputerId): boolean;
  /** A timer; returns its cancel. */
  later?(callback: () => void, ms: number): () => void;
  /** A monotonic clock in milliseconds (performance.now), as Qt's QElapsedTimer. */
  now?(): number;
}

const capped = (list: ActivityEvent[]) => (list.length > ARCHIVED_EVENTS_MAX ? list.slice(list.length - ARCHIVED_EVENTS_MAX) : list);
const sameTarget = (a: ArchiveTarget | undefined, b: ArchiveTarget | undefined) =>
  a === b || (!!a && !!b && a.id === b.id && a.computer === b.computer && a.name === b.name && a.archiveId === b.archiveId);
/** Qt reads a missing seq as 0, which is never above the cursor. */
const seqNumber = (e: ActivityEvent) => seqOf(e) ?? 0;

/** The archived Activity store; call it inside a reactive root. */
export function createArchiveActivity(deps: ArchiveActivityDeps) {
  const later = deps.later ?? ((callback: () => void, ms: number) => { const timer = setTimeout(callback, ms); return () => clearTimeout(timer); });
  const now = deps.now ?? (() => performance.now());
  const [target, setTarget] = createSignal<ArchiveTarget | undefined>();
  const [events, setEvents] = createSignal<readonly ActivityEvent[]>([]);
  const [summary, setSummary] = createSignal<Record<string, unknown> | undefined>();
  const [loaded, setLoaded] = createSignal(false);
  const [error, setError] = createSignal<string | undefined>();
  /** The target's computer is online: the reads pause while it is not, keeping the events. */
  const reachable = createMemo(() => { const t = target(); return !!t && (deps.online?.(t.computer) ?? true); });

  /** The next read's `--after` (Qt's m_cursor). */
  let cursor = 0;
  /** The conversation shown (Qt's m_details conversation_id); unknown before the first answer. */
  let conversation: string | undefined;
  /** The same conversation, reactive, for the attachments' scope. */
  const [shownConversation, setShownConversation] = createSignal("");
  /** Counts the drops: an answer to a read started before the last one does not apply. */
  let generation = 0;
  let reading = false;
  let started = 0;
  let showing = false;
  let cancel: (() => void) | undefined;

  const stopTimer = () => { cancel?.(); cancel = undefined; };
  const schedule = (ms: number) => { stopTimer(); cancel = later(() => { cancel = undefined; read(); }, ms); };
  /** The next poll, ARCHIVED_POLL_MS after the read that just ended started. */
  const next = () => schedule(Math.max(0, started + ARCHIVED_POLL_MS - now()));

  /** Drops the events, the summary, the cursor and the error; a read in flight no longer applies. */
  function drop() {
    generation++;
    reading = false;
    stopTimer();
    cursor = 0;
    conversation = undefined;
    setShownConversation("");
    batch(() => { setEvents([]); setSummary(undefined); setLoaded(false); setError(undefined); });
  }

  function read() {
    const t = untrack(target);
    if (!t || !showing || reading || !untrack(reachable)) return;
    const mine = generation;
    reading = true;
    started = now();
    let reply: Promise<ArchivedActivity>;
    try { reply = deps.read(t.computer, t.name, t.archiveId, cursor); } catch (e) { reply = Promise.reject(e); }
    reply.then(
      (r) => { if (mine === generation) { reading = false; accept(r); } },
      (e) => { if (mine === generation) { reading = false; fail(e); } },
    );
  }

  function accept(r: ArchivedActivity) {
    const list = Array.isArray(r.events) ? r.events : [];
    const id = typeof r.conversation_id === "string" ? r.conversation_id : "";
    const cleared = Array.isArray(r.cleared_conversations) ? r.cleared_conversations : [];
    if (conversation !== undefined && conversation !== id && !cleared.includes(conversation)) {
      // Another conversation: its window, and at once a read from 0, so the
      // previous conversation's cursor never skips the new history.
      cursor = 0;
      conversation = id;
      setShownConversation(id);
      batch(() => { setEvents(capped([...list])); setSummary(r.summary); setLoaded(true); });
      read();
      return;
    }
    conversation = id;
    setShownConversation(id);
    const shown = untrack(events);
    let merged: readonly ActivityEvent[] = shown;
    if (r.replace_events) merged = capped([...list]);
    else {
      // Skip what is at or below the cursor or already shown; the rest append in the reply's (journal) order.
      const known = new Set(shown.map(seqNumber));
      const fresh = list.filter((e) => {
        const seq = seqNumber(e);
        if (seq <= cursor || known.has(seq)) return false;
        known.add(seq);
        return true;
      });
      if (fresh.length) merged = capped([...shown, ...fresh]);
    }
    cursor = typeof r.cursor === "number" ? r.cursor : 0;
    batch(() => {
      if (merged !== shown) setEvents(merged);
      setSummary(r.summary);
      setLoaded(true);
      setError(undefined);
    });
    next();
  }

  function fail(reason: unknown) {
    const text = errorText(reason);
    // Busy is Qt's `return false`: nothing started, so the next tick tries again.
    if (text === bridge.ARCHIVED_ACTIVITY_BUSY) { schedule(ARCHIVED_RETRY_MS); return; }
    batch(() => { setError(tr("Could not refresh activity: %1", text)); setLoaded(true); });
    next();
  }

  /** Reads one archived row (the selected one) from cursor 0; another row or none drops everything first. */
  function follow(nextTarget: ArchiveTarget | undefined) {
    if (sameTarget(untrack(target), nextTarget)) return;
    drop();
    setTarget(nextTarget);
    read();
  }

  createEffect(on(deps.showing, (show) => {
    showing = show;
    if (!show) drop();
    else read();
  }));
  // Back online: the paused reads resume.
  createEffect(on(reachable, (up) => { if (up) read(); }, { defer: true }));
  onCleanup(() => { generation++; stopTimer(); });

  const source = {
    target, events, summary, loaded, error,
    /** The archived row's conversation, as its last read named it. */
    attachmentScope: () => {
      const t = target();
      return t ? { computer: t.computer, name: t.name, conversation_id: shownConversation(), archive_id: t.archiveId } : undefined;
    },
    loadingOlder: () => false,
    olderDone: () => true,
    ended: () => false,
    loadOlder: () => {},
    readOnly: true as const,
    follow,
  };
  return source satisfies ActivitySource;
}

/**
 * The selected archived row with an archive id: a loaded one, or the source of
 * a pending restore while the archive reloads (the same lookup as `selected`,
 * Qt's selected() 3136-3139).
 */
function selectedArchived(): ArchiveTarget | undefined {
  const key = selectedKey();
  if (!key || !activityReadOnly(key)) return undefined;
  const row = selected();
  return row?.key === key && row.archiveId ? { id: row.key, computer: row.computer, name: row.name, archiveId: row.archiveId } : undefined;
}

/** The app's archived Activity: the selected archived row, while the Sessions page shows in a shown window. */
export const archiveActivity = createRoot(() => {
  const store = createArchiveActivity({
    read: (computer, name, archiveId, after) => bridge.archivedActivity(computer, name, archiveId, after),
    showing: () => page() === "sessions" && !pageHidden(),
    online: (computer) => !computerOffline(computer),
  });
  createEffect(() => {
    const t = selectedArchived();
    untrack(() => store.follow(t));
  });
  return store;
});
