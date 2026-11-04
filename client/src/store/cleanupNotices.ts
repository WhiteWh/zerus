// Loaded only for retained cleanup outcomes or their review dialog.
import { createSignal } from "solid-js";
import * as bridge from "../bridge";
import type { Outcome } from "../types";
import type { ActionRow } from "../ui/sessions/actions-ui";
import { bus, sessionNoticeKey, type NoticeAction } from "./notices";
import { cleanup, closeCleanup, openCleanup, refreshCleanup, type CleanupRow, type RestoreSource } from "./terminationCleanup";
import { tr } from "../i18n";

interface Warning {
  key: string;
  text: string;
  id?: string;
  restore?: RestoreSource | null;
  review(): void | Promise<void>;
}
const seen = new Map<string, string>();
const owned = new Map<string, string>();
// Only visible notices need callback captures. The native store owns the rows.
const warnings = new Map<string, Warning>();
const [restoring, setRestoring] = createSignal(new Set<string>());
const keyOf = (source: RestoreSource) => sessionNoticeKey(source.target.computer, source.target.name);
export const cleanupRestoring = (source: RestoreSource) => restoring().has(keyOf(source));
export const anyCleanupRestore = () => restoring().size > 0;
const errorText = (error: unknown) => typeof error === "string" ? error : error instanceof Error ? error.message : String(error);

function publish(warning: Warning) {
  const disabled = () => restoring().has(warning.key) || !!warning.id && !cleanup().rows.some((r) => r.id === warning.id && !r.busy);
  const actions: NoticeAction[] = [{ label: tr("Review…"), style: "ghost", disabled, run: async () => {
    try { await warning.review(); }
    catch (error) { bus.notify("error", errorText(error), true); publish(warning); }
  } }];
  if (warning.restore) actions.push({ label: tr("Restore session"), style: "primary", icon: "play", disabled, run: () => restoreCleanup(warning.restore!) });
  bus.notify("warn", warning.text, true, warning.key, tr("Session archived"), { actions });
  warnings.set(warning.key, warning);
  const showing = new Set(bus.notices().map((n) => n.key));
  for (const key of warnings.keys()) if (!showing.has(key)) warnings.delete(key);
}

export function syncCleanupNotices() {
  const rows = cleanup().rows;
  const ids = new Set(rows.map((r) => r.id));
  for (const [key, id] of owned) if (!ids.has(id)) {
    bus.clearNotice(key); owned.delete(key); warnings.delete(key);
  }
  for (const id of seen.keys()) if (!ids.has(id)) seen.delete(id);
  for (const row of rows) {
    if (!row.source || !row.notice) continue;
    const key = sessionNoticeKey(row.source.computer, row.source.name);
    const signature = `${row.notice}\n${row.restore?.archive_id ?? ""}`;
    if (seen.get(row.id) === signature) continue;
    seen.set(row.id, signature);
    owned.set(key, row.id);
    publish({ key, id: row.id, text: row.notice, restore: row.restore, review: () => openCleanup(row.id) });
  }
}

/** Uses the same exact archive writer as the Archive view; never retries removal. */
export async function restoreCleanup(source: RestoreSource): Promise<void> {
  const key = keyOf(source);
  if (restoring().has(key)) return;
  const warning = warnings.get(key);
  setRestoring((old) => new Set(old).add(key));
  try {
    const outcome = await bridge.terminationCleanupRestore(source);
    if (outcome.outcome !== "done") throw new Error(outcome.detail);
    owned.delete(key); warnings.delete(key);
    bus.notify("info", "", false, key, tr("Session restored"));
    await refreshCleanup();
    if (!cleanup().rows.length) closeCleanup();
  } catch (error) {
    bus.notify("error", errorText(error), true);
    // The refusal must not evict its recovery notice at the notice cap.
    if (warning) publish(warning);
  } finally {
    setRestoring((old) => { const next = new Set(old); next.delete(key); return next; });
  }
}

/** Called once for the original action outcome, including rows beyond the cap. */
export async function reportCleanup(outcome: Extract<Outcome, { outcome: "done" }>, row: Pick<ActionRow, "computer" | "name">) {
  const report = outcome.cleanup;
  if (!report || !["kept", "unconfirmed"].includes(report.status)) return;
  await refreshCleanup();
  syncCleanupNotices();
  const captured = outcome.cleanup_capture;
  if (captured?.id && cleanup().rows.some((r) => r.id === captured.id)) return;
  const key = sessionNoticeKey(row.computer, row.name);
  publish({ key, text: captured?.notice ?? tr("Working folder kept for review."), restore: captured?.restore,
    review: async () => {
      const { openWorktreeReview } = await import("./worktreeCleanup");
      await openWorktreeReview(row.computer, report.common_dir, report.path, document.activeElement instanceof HTMLElement ? document.activeElement : undefined);
    },
  });
}

/** A review-row Restore shares the notice's writer and busy state. */
export function restoreCleanupRow(row: CleanupRow) { return row.restore ? restoreCleanup(row.restore) : Promise.resolve(); }

export function clearCleanupNotices() {
  for (const key of warnings.keys()) bus.clearNotice(key);
  warnings.clear(); owned.clear(); seen.clear();
}
