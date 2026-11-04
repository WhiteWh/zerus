import { captureEnd, type EndReference } from "./endActionBusy";
import { batch as together, createSignal } from "solid-js";
import { catalog as appCatalog, computerFresh, computerOffline, type CatalogState } from "./catalog";
import { dismiss as appDismiss, notify as appNotify, perform as appPerform, performComputer as appPerformComputer, updating as appUpdating, type Notice } from "./actions";
import { markNeedsAttention as appMarkNeedsAttention, markRead as appMarkRead } from "./marks";
import { compactRows, needsAttention, setCompactRows } from "./ui";
import { archiveNameOccupied, liveNameIndex, nameKey, snapshotOf, type ActionRow } from "../ui/sessions/actions-ui";
import {
  changedText, confirmationCopy, markSummary, progressText, restoreLimitText, sameInstance, selectionApplies, selectionCall,
  selectionSnapshot, summaryText, SHOWN_ERRORS, type BatchConfirmation, type SelectionAction, type SelectionRow, type WriteAction,
} from "../ui/sessions/selection";
import type { Session } from "../ui/sessions/types";
import type { Action, Outcome } from "../types";

// A multi-selection's actions, after Qt's runSelectionAction,
// runNextSelectionAction and finishSelectionAction (SessionSelection.cpp
// 237-400). Compact and Expand change the row preference at once; Mark as
// read and Mark needs attention change the local marks at once. A write runs
// one session at a time through the guarded `perform`: each item is looked up
// again and must be the same instance and still apply, the next starts only
// after the previous outcome, a failure (also a hook that throws) is counted
// and never retried, and one notice sums the batch up. The archive refreshes
// when a write batch ends. While a batch runs (`running`, Qt's m_pending) no
// other write starts: the single-session entries read it too.
//
// Qt's local launch question before resuming sessions of this computer (spec
// D5, LocalSessionHost::confirmLaunch) has no client counterpart yet, as for
// one session's Resume.

export type { SelectionAction, SelectionRow };

/**
 * The archive calls (Qt's restoreArchive and forgetArchive, store/restores.ts).
 * Archived rows are not catalog rows: `current` is the archived row as its
 * archive page lists it now. Until these are bound an archived item fails
 * without a call.
 */
export interface ArchiveCalls {
  current(row: ActionRow): SelectionRow | undefined;
  /** "limit" when earlier restores still need a refresh: nothing runs. */
  restore(row: ActionRow): Promise<Outcome> | "limit";
  forget(row: ActionRow, delay?: EndReference): Promise<Outcome>;
  /** Reloads the archive pages, once a write batch has ended (Qt SessionsWindow.cpp 1916). */
  refresh(): void;
  /**
   * The catalog lists a live row of the archived row's name now, one that the
   * restores saw appear since the run's name index was built (Qt reads its
   * entries at each item). Never inferred from an earlier item's outcome.
   */
  taken(row: ActionRow): boolean;
}

export interface BatchDeps {
  catalog: Pick<CatalogState, "rows">;
  online(computer: string): boolean;
  updating(computer: string, name: string): boolean;
  /** One guarded call that leaves the wording to the batch (the worktree lines still show). */
  perform(row: ActionRow, action: Action, delay?: EndReference): Promise<Outcome>;
  delay?(rows: SelectionRow[], action: WriteAction, execute: (refs: EndReference[], key: string) => Promise<void>): Promise<void>;
  notify(tone: Notice["tone"], text: string, sticky?: boolean, key?: string): number;
  dismiss(id: number): void;
  compact: { get(key: string): boolean; set(key: string, compact: boolean): void };
  attention(s: SelectionRow): boolean;
  markRead(row: ActionRow): boolean;
  markNeedsAttention(row: ActionRow): boolean;
  /** The Forget or Terminate confirmation: true only for its confirm button. */
  confirm(copy: BatchConfirmation): Promise<boolean>;
  archive(): ArchiveCalls | undefined;
}

export interface RunOptions {
  /** Qt's clearArchive: the targets are the archived rows in scope. */
  clearArchive?: boolean;
  /** Only the loaded archive entries are in scope (Qt's paged archive browser). */
  paged?: boolean;
}

const errorText = (error: unknown) => (typeof error === "string" ? error : error instanceof Error ? error.message : String(error));

export function createBatch(deps: BatchDeps) {
  const [running, setRunning] = createSignal(false);

  /**
   * Qt's selectionActionApplies for a row now. `names` is a live name index
   * (`liveNameIndex`) for checking many rows, plus the live rows of restores
   * that appeared since (`taken`); without it an archived row's Resume scans
   * the catalog once (Qt's archiveNameOccupied).
   */
  function applies(s: SelectionRow, a: SelectionAction, online: boolean, updating: boolean, names?: () => ReadonlySet<string>): boolean {
    const occupied = () => (names ? names().has(nameKey(s)) || !!deps.archive()?.taken(s) : archiveNameOccupied(s, deps.catalog.rows));
    return selectionApplies(s, a, {
      online, updating, compact: deps.compact.get(s.key), attention: deps.attention(s),
      occupied: a === "resume" && s.state === "archived" && occupied(),
    });
  }

  /** The row as it is listed now: a catalog row, or an archived row through the archive calls. */
  const current = (s: ActionRow): SelectionRow | undefined =>
    s.state === "archived" ? deps.archive()?.current(s) : deps.catalog.rows[s.key];

  /** One item's call; an archived row needs the archive calls. */
  async function call(row: ActionRow, a: WriteAction, delay?: EndReference): Promise<Outcome | "limit"> {
    const step = selectionCall(row, a);
    if ("call" in step) return delay ? deps.perform(row, step.call, delay) : deps.perform(row, step.call);
    const archive = deps.archive();
    if (!archive) return { outcome: "failed", detail: "session changed or is unavailable" };
    return step.archive === "restore" ? archive.restore(row) : (delay ? archive.forget(row, delay) : archive.forget(row));
  }

  /**
   * Runs `a` on the selected `rows` (snapshots of them as the menu or bar
   * showed them). The targets are the rows it applies to; with none nothing
   * happens. Forget and Terminate ask first.
   */
  async function run(a: SelectionAction, rows: readonly SelectionRow[], options: RunOptions = {}): Promise<void> {
    // One live name index per run (Qt's archiveNameOccupied for every archived
    // Resume), built on first use. A second instance of a name restored in this
    // run meets the restore limit (Qt's m_restores), or `taken` once its live
    // row is listed.
    let names: Set<string> | undefined;
    const index = () => (names ??= liveNameIndex(deps.catalog.rows));
    const appliesNow = (s: SelectionRow) => applies(s, a, deps.online(s.computer), deps.updating(s.computer, s.name), index);
    if (a === "compact" || a === "expand") {
      // Presentation only: also while a batch runs. Qt's compact row limit has no client counterpart.
      together(() => {
        for (const original of rows) {
          const live = current(original);
          if (live && sameInstance(live, original) && appliesNow(live)) deps.compact.set(live.key, a === "compact");
        }
      });
      return;
    }
    // Move to project waits for the organisation stream's move (MOVE_LATER).
    if (a === "move" || running()) return;
    const targets = rows.filter((r) => appliesNow(r)).map(selectionSnapshot);
    if (!targets.length) return;
    if (a === "forget" || a === "terminate") {
      const copy = confirmationCopy(a, targets, rows.length, options.clearArchive ? { paged: !!options.paged } : undefined);
      if (!(await deps.confirm(copy)) || running()) return;
    }
    if (a === "mark_read" || a === "review_later") {
      let changed = 0;
      for (const row of targets) if (a === "mark_read" ? deps.markRead(row) : deps.markNeedsAttention(row)) changed++;
      const summary = markSummary(a, changed, targets.length);
      deps.notify(summary.differs ? "error" : "info", summary.text, summary.differs);
      return;
    }

    const execute = async (refs?: EndReference[], noticeKey?: string) => {
      setRunning(true);
      let done = 0;
      let failed = 0;
      let unknown = 0;
      const errors: string[] = [];
      const fail = (line: string, uncertain = false) => { if (uncertain) unknown++; else failed++; if (errors.length < SHOWN_ERRORS) errors.push(line); };
      let progress: number | undefined;
      try {
        for (const [at, original] of targets.entries()) {
          if (at > 0 && at % 64 === 0) await new Promise((r) => setTimeout(r, 0));
          // Each item on its own: a hook or call that throws fails this item only, and is never retried.
          try {
            const live = current(original);
            if (!live || !sameInstance(live, original) || !appliesNow(live)) { fail(changedText(original)); continue; }
            // One progress line at a time, as Qt's single notice.
            if (progress !== undefined) deps.dismiss(progress);
            progress = deps.notify("info", progressText(a, done + failed + unknown + 1, targets.length), false, noticeKey);
            const row = snapshotOf(live);
            const out = await call(row, a, refs?.[at]);
            if (out === "limit") fail(restoreLimitText(row));
            else if (out.outcome === "done") done++;
            else fail(`${selectionCall(row, a).operation}: ${out.detail}`, out.outcome === "uncertain");
          } catch (error) {
            fail(`${selectionCall(original, a).operation}: ${errorText(error)}`);
          }
        }
      } finally {
        setRunning(false);
      }
      if (progress !== undefined) deps.dismiss(progress);
      const summary = summaryText(a, done, targets.length, failed, errors, unknown);
      deps.notify(summary.failed ? "error" : "info", summary.text, summary.failed, noticeKey);
      // Qt refreshes the archive when the batch is over (SessionsWindow.cpp 1916).
      try { deps.archive()?.refresh(); } catch { /* the summary stands; a refresh is only a read */ }
    };
    if (deps.delay && !options.clearArchive && ["archive", "terminate", "forget"].includes(a)) {
      await deps.delay(targets, a, (refs, key) => execute(refs, key));
    } else await execute();
  }

  /**
   * Holds the running flag (Qt's m_pending) for a machine-wide call; false
   * when a batch or another such call already runs and nothing happened.
   */
  async function exclusive(task: () => Promise<void>): Promise<boolean> {
    if (running()) return false;
    setRunning(true);
    try { await task(); } finally { setRunning(false); }
    return true;
  }

  return { applies, run, running, exclusive };
}

let confirmer: ((copy: BatchConfirmation) => Promise<boolean>) | undefined;
/** Hands the store the dialog that confirms a selection's Forget or Terminate (ActionDialogs.tsx). */
export function bindBatchConfirm(confirm: (copy: BatchConfirmation) => Promise<boolean>) { confirmer = confirm; }

let archiveCalls: ArchiveCalls | undefined;
/** Hands the store the archive calls (store/restores.ts). */
export function bindArchiveCalls(calls: ArchiveCalls) { archiveCalls = calls; }

const app = createBatch({
  catalog: appCatalog,
  online: (computer) => !computerOffline(computer),
  updating: appUpdating,
  perform: (row, action, delay) => appPerform(row, action, { quiet: true, delay, skipDelay: !delay }),
  delay: async (rows, action, execute) => {
    const captures = rows.map((row) => {
      const step = selectionCall(row, action);
      return captureEnd(row, "call" in step ? step.call : { kind: "forget_archive", archive_id: row.archiveId ?? "" });
    });
    const { endActions } = await import("./endActions");
    await endActions.schedule(captures, execute);
  },
  notify: appNotify,
  dismiss: appDismiss,
  compact: { get: (key) => !!compactRows[key], set: (key, compact) => setCompactRows(key, compact) },
  attention: (s) => needsAttention(s as Session, computerFresh(s.computer)),
  markRead: appMarkRead,
  markNeedsAttention: appMarkNeedsAttention,
  // Without the dialog nothing destructive runs.
  confirm: (copy) => (confirmer ? confirmer(copy) : Promise.resolve(false)),
  archive: () => archiveCalls,
});

export const { applies, run, running, exclusive } = app;

/**
 * Pause all or Resume all on one computer (Qt's pauseAll and resumeAll with
 * their notices): the progress notice, then the guarded call, holding the
 * running flag meanwhile. The call is never repeated.
 */
export async function runMachine(computer: string, machine: string, kind: "pause_all" | "resume_all", count: number): Promise<void> {
  await exclusive(async () => {
    appNotify("info", `${kind === "pause_all" ? "Pausing" : "Resuming"} sessions on ${machine}…`);
    await appPerformComputer(computer, { kind, count });
  });
}
