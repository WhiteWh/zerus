import { batch, createEffect, createRoot, createSignal, on, untrack } from "solid-js";
import { catalog as appCatalog, computerOffline, type CatalogState } from "./catalog";
import { GONE, notify as appNotify, perform as appPerform, stillListed as appStillListed, updating as appUpdating, type Notice } from "./actions";
import { archive, bindRetainedRows } from "./archive";
import { bindArchiveCalls, running, type ArchiveCalls } from "./batch";
import { search, selectedKey, setSelectedKey, setStateFilter, stateFilter, type StateFilter } from "./ui";
import { archiveNameOccupied, menuState, nameKey, snapshotOf, STATE_CHANGED, type ActionRow } from "../ui/sessions/actions-ui";
import type { Session } from "../ui/sessions/types";
import type { Action, Outcome } from "../types";
import { trNoop, tr } from "../i18n";

// Restore session and Forget archived session, after Qt's changeSession
// (SessionsWindow.cpp 5542-5560), terminateSession (5758-5826), writeDone
// (1915-1972) and the restore follow-up in applyRows (3281-3336); the batch's
// archive calls after runNextSelectionAction (SessionSelection.cpp 333-343)
// and finishSelectionAction (379-384).
//
// A restore is pending from its call until its live row appears: at most
// RESTORE_LIMIT of them, one per computer and name (Qt's m_restores). A failed
// call drops it. A successful one waits for the live (running) row of that
// name; then, when the restore is still followed (Qt's m_restoreKey: the
// single restore of the selected row, not cancelled since), the selection
// moves to that row if it carries the restored conversation, or clears with a
// sticky notice if it carries another. Writes are never repeated.
//
// The follow-up moves no draft: the archived draft stays under its archive
// key, where Saved drafts lists it, and the notice says so (Qt 3331, owner
// ruling in zerus-5wmh.9). Qt's restored-inspection set
// (awaitingRestoredInspection) has no counterpart: the details only enable
// controls when they name the row's run and conversation (store/actions.ts
// detailsFor).

export const RESTORE_LIMIT = 64;
const errorText = (error: unknown) => (typeof error === "string" ? error : error instanceof Error ? error.message : String(error));
export const RESTORES_PENDING = trNoop("Finish or refresh the earlier restores before restoring another session.");
export const RESTORE_UNVERIFIED = trNoop("The restored conversation could not be verified. Your archived draft remains in Saved drafts.");
/** Qt's selectSessionEntry refusal after a confirmation. */
export const NO_LONGER_AVAILABLE = trNoop("This session is no longer available. Refresh the list and open its actions again.");

interface PendingRestore {
  /** The computer and name (`nameKey`). */
  id: string;
  /** The archived row's key, and the row as its page listed it (kept as the selected row meanwhile). */
  sourceKey: string;
  source: Session | undefined;
  /** The conversation the live row must carry to be followed. */
  conversation: string;
  succeeded: boolean;
}

export interface RestoreDeps {
  /** The live catalog. */
  catalog: Pick<CatalogState, "rows" | "order">;
  /** A loaded archived row by key (store/archive.ts). */
  archived(key: string): Session | undefined;
  online(computer: string): boolean;
  updating(computer: string, name: string): boolean;
  /** A selection batch or a machine-wide call runs (Qt's m_pending). */
  batchRunning(): boolean;
  perform(row: ActionRow, action: Action, options?: import("./actions").PerformOptions): Promise<Outcome>;
  notify(tone: Notice["tone"], text: string, sticky?: boolean): number;
  /** The click-time recheck of a menu entry (store/actions.ts). */
  stillListed(row: ActionRow): ActionRow | undefined;
  refreshArchive(): void;
  selectedKey(): string | undefined;
  setSelectedKey(key: string | undefined): void;
  stateFilter(): StateFilter;
  setStateFilter(filter: StateFilter): void;
  search(): string;
}

/** The restore store; call it inside a reactive root. */
export function createRestores(deps: RestoreDeps) {
  const [pending, setPending] = createSignal<readonly PendingRestore[]>([]);
  /** The single restore whose call runs (Qt's m_restoreWriteKey). */
  let writing: string | undefined;
  /** The followed restore (Qt's m_restoreKey). */
  let followed: string | undefined;
  /** The filter this store set, so its own change does not cancel the follow. */
  let ownFilter: StateFilter | undefined;
  /** The live row key each restored name appeared with, the latest RESTORE_LIMIT (for `taken`). */
  const appeared = new Map<string, string>();

  const find = (id: string) => untrack(pending).find((p) => p.id === id);
  const add = (p: PendingRestore) => setPending((all) => [...all, p]);
  const drop = (id: string) => setPending((all) => all.filter((p) => p.id !== id));
  const succeed = (id: string) => setPending((all) => all.map((p) => (p.id === id ? { ...p, succeeded: true } : p)));
  /** Qt's restore limit: this session already pending, or RESTORE_LIMIT in all. */
  const full = (id: string) => !!find(id) || untrack(pending).length >= RESTORE_LIMIT;
  const pendingFor = (row: ActionRow): PendingRestore => ({
    id: nameKey(row), sourceKey: row.key, source: deps.archived(row.key), conversation: row.conversationId ?? "", succeeded: false,
  });

  /** Another write, or the user's filter or search change, cancels the follow (Qt clears m_restoreKey). */
  const cancelFollow = () => { followed = undefined; };
  createEffect(on(deps.stateFilter, (filter) => {
    if (ownFilter === filter) { ownFilter = undefined; return; }
    cancelFollow();
  }, { defer: true }));
  createEffect(on(deps.search, cancelFollow, { defer: true }));
  createEffect(on(deps.batchRunning, (busy) => { if (busy) cancelFollow(); }, { defer: true }));

  /** Qt's applyRows step for one restore whose live row appeared. */
  function followUp(p: PendingRestore, live: Session) {
    const follow = followed === p.id && deps.selectedKey() === p.sourceKey;
    const conversation = live.conversationId ?? "";
    if (p.conversation && conversation === p.conversation) {
      if (follow) deps.setSelectedKey(live.key);
      // A cancelled follow is not revived; no newer selection changes.
      else if (deps.selectedKey() === p.sourceKey) deps.setSelectedKey(undefined);
    } else if (follow) {
      // A legacy row without a native identity is followed as it is.
      if (!conversation) deps.setSelectedKey(live.key);
      else {
        deps.setSelectedKey(undefined);
        deps.notify("error", tr(RESTORE_UNVERIFIED), true);
      }
    }
    if (followed === p.id) followed = undefined;
    appeared.delete(p.id);
    appeared.set(p.id, live.key);
    if (appeared.size > RESTORE_LIMIT) appeared.delete(appeared.keys().next().value!);
    drop(p.id);
  }

  // Watches the live rows only while a successful restore waits: one pass over
  // the catalog per change, as Qt's applyRows on every rebuild.
  createEffect(() => {
    const waiting = pending().filter((p) => p.succeeded);
    if (!waiting.length) return;
    const wanted = new Set(waiting.map((p) => p.id));
    const found = new Map<string, Session>();
    for (const id of deps.catalog.order) {
      const r = deps.catalog.rows[id];
      if (!r || r.state !== "running") continue;
      const name = nameKey(r);
      if (wanted.has(name) && !found.has(name)) found.set(name, r);
      if (found.size === wanted.size) break;
    }
    if (!found.size) return;
    untrack(() => batch(() => {
      for (const p of waiting) {
        const live = found.get(p.id);
        if (live) followUp(p, live);
      }
    }));
  });

  /** The Restore entry's rule (menuState), read again on the live row at the click. */
  function restorable(live: ActionRow): boolean {
    return menuState(live, deps.online(live.computer), deps.updating(live.computer, live.name), undefined, {
      batchRunning: deps.batchRunning(), occupied: archiveNameOccupied(live, deps.catalog.rows),
    }).change.enabled;
  }

  /**
   * Restore session from a menu or the header (Qt's runSessionMenuAction
   * recheck, then changeSession). A row that is gone or changed gets the
   * recheck's notice, also one listed under another name (Qt's entry key holds
   * the name; the client's holds the archive id); an entry no longer enabled
   * returns silently; past the restore limit nothing runs and the sticky
   * notice says so.
   */
  async function restore(row: ActionRow): Promise<void> {
    // A pending restore's row, kept selected after the archive reloaded, is Qt's selected() entry.
    const kept = untrack(pending).find((p) => p.sourceKey === row.key)?.source;
    const live = kept && !deps.archived(row.key) ? snapshotOf(kept) : deps.stillListed(row);
    if (!live) return;
    if (live.name !== row.name) { deps.notify("info", tr(GONE), true); return; }
    if ((live.state === "running") !== (row.state === "running")) { deps.notify("error", tr(STATE_CHANGED), true); return; }
    if (live.state !== "archived" || !restorable(live)) return;
    const id = nameKey(live);
    if (writing !== undefined || full(id)) { deps.notify("error", tr(RESTORES_PENDING), true); return; }
    const source = snapshotOf(live);
    writing = id;
    followed = id;
    add(pendingFor(source));
    let out: Outcome;
    try {
      out = await deps.perform(source, { kind: "restore", archive_id: source.archiveId ?? "" });
    } catch (error) {
      // As a failed outcome: the sticky error, never replayed.
      out = { outcome: "failed", detail: errorText(error) };
      deps.notify("error", out.detail, true);
    } finally {
      writing = undefined;
    }
    batch(() => {
      if (out.outcome === "done") {
        // writeDone: the archive refreshes (1917), then only the list filter changes (1963-1967).
        deps.refreshArchive();
        succeed(id);
        if (followed === id && deps.selectedKey() === source.key && deps.stateFilter() === "archived") {
          ownFilter = "all";
          deps.setStateFilter("all");
        }
      } else {
        if (followed === id) followed = undefined;
        drop(id);
      }
    });
  }

  /**
   * The Forget archived session confirmation's click: only the instance the
   * dialog named (its key holds the archive id; the same name, which Qt's key
   * holds; `created` unchanged), else Qt's notice and nothing runs. An offline computer or a running batch ends
   * silently, as in Qt. Success refreshes the archive.
   */
  async function forgetArchived(row: ActionRow): Promise<void> {
    const live = deps.archived(row.key);
    if (!live || live.name !== row.name || ((row.created ?? 0) > 0 && (live.created ?? 0) > 0 && live.created !== row.created)) {
      deps.notify("error", tr(NO_LONGER_AVAILABLE), true);
      return;
    }
    if (!deps.online(live.computer) || deps.batchRunning() || deps.updating(live.computer, live.name)) return;
    if (live.state === "running") { deps.notify("error", tr(STATE_CHANGED), true); return; }
    cancelFollow();
    const out = await deps.perform(row, { kind: "forget_archive", archive_id: row.archiveId ?? "" });
    if (out.outcome === "done") deps.refreshArchive();
  }

  /** The selection batch's archive calls (store/batch.ts): quiet, the batch words the outcomes. */
  const calls: ArchiveCalls = {
    current: (row) => deps.archived(row.key),
    restore: (row) => {
      const id = nameKey(row);
      if (full(id)) return "limit";
      add(pendingFor(row));
      return deps.perform(row, { kind: "restore", archive_id: row.archiveId ?? "" }, { quiet: true }).then(
        (out) => { if (out.outcome === "done") succeed(id); else drop(id); return out; },
        (error: unknown) => { drop(id); throw error; },
      );
    },
    forget: (row, delay) => deps.perform(row, { kind: "forget_archive", archive_id: row.archiveId ?? "" }, { quiet: true, delay, skipDelay: !delay }),
    refresh: () => deps.refreshArchive(),
    taken: (row) => {
      const key = appeared.get(nameKey(row));
      const live = key ? deps.catalog.rows[key] : undefined;
      return !!live && live.state !== "archived" && live.computer === row.computer && live.name === row.name;
    },
  };

  return {
    restore,
    forgetArchived,
    calls,
    cancelFollow,
    /** The archived row of a pending restore, kept as the selected row while the archive reloads (Qt's selected()). */
    sourceOf: (key: string): Session | undefined => pending().find((p) => p.sourceKey === key)?.source,
    pendingCount: () => pending().length,
  };
}

/** The app's restores, bound to the batch and to the selection. */
const app = createRoot(() => createRestores({
  catalog: appCatalog,
  archived: archive.row,
  online: (computer) => !computerOffline(computer),
  updating: appUpdating,
  batchRunning: running,
  perform: appPerform,
  notify: appNotify,
  stillListed: appStillListed,
  refreshArchive: () => archive.refresh(),
  selectedKey, setSelectedKey, stateFilter, setStateFilter, search,
}));
bindArchiveCalls(app.calls);
bindRetainedRows(app.sourceOf);

export const { restore: restoreSession, forgetArchived, cancelFollow: cancelRestoreFollow } = app;

/** Qt's writeDone refresh (1917) after a successful single Rename or Move to archive. */
export function refreshArchiveWhenDone(out: Outcome): Outcome {
  if (out.outcome === "done") archive.refresh();
  return out;
}
