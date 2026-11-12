import { createSignal } from "solid-js";
import * as bridge from "../bridge";
import { toast } from "./notices";
import type { Target } from "../types";
import { tr } from "../i18n";
export interface Consents { local_files: boolean; discard_changes: boolean; unchecked_processes: boolean; merged_branches: false }
export interface RestoreSource { target: Target; machine: string; archive_id: string; status: "kept" | "unconfirmed"; overflow: boolean }
export interface RetainedCapture { id: string | null; restore: RestoreSource | null; notice: string }
export type ProgramRetirement = "expired" | "source_unavailable" | "holder_unavailable" | "offer_unavailable" | "stop_unconfirmed" | "cleanup_settled" | "cleanup_unconfirmed";
export interface CleanupRow { id: string; folder_key?: string; can_dismiss?: boolean; can_remove_anyway?: boolean; sessions?: string[]; processes?: { pid: number; label: string }[]; label: string; path: string; machine: string; computer: string; branch: string; size: string; status: "removed" | "kept" | "review" | "unknown" | "ready" | "checking" | "removing" | "stopping"; reason: string; details: string; can_check: boolean; can_force?: boolean; force_revision?: number | null; program_retirement?: ProgramRetirement | null; ready: boolean; busy?: boolean; activity?: string | null; source?: Target | null; notice?: string | null; restore?: RestoreSource | null }
export interface CleanupView { revision: number; rows: CleanupRow[]; busy: boolean; cleaning?: boolean; can_clear?: boolean; removed: number; kept: number; unknown: number; required: Consents; text: string; title: string }
export interface Inspection {
  path: string; common_dir: string; files?: { path: string; status: string }[]; files_partial?: boolean;
  changes?: { text?: string; error?: string }; comparison?: { text?: string; error?: string }; commits?: { text?: string; error?: string };
  review?: { sessions?: string[]; processes?: {pid: number; command?: string; label?: string}[]; reasons?: {code: string; message: string}[]; process_count?: number; unreadable_processes?: unknown[]; untracked?: number; changes?: number }; file?: { path: string; entries?: { path: string; directory: boolean; symlink: boolean }[]; next_offset?: number; binary?: boolean; text?: string; truncated?: boolean };
}
export const emptyConsents = (): Consents => ({ local_files: false, discard_changes: false, unchecked_processes: false, merged_branches: false });
export const emptyCleanup = (): CleanupView => ({ revision: 0, rows: [], busy: false, removed: 0, kept: 0, unknown: 0, required: emptyConsents(), text: "", title: "" });
const [cleanup, setCleanup] = createSignal(emptyCleanup());
const [cleanupDialog, setCleanupDialog] = createSignal<{ origin?: HTMLElement; selected?: string }>();
export { cleanup, cleanupDialog };
let noticeModule: Promise<typeof import("./cleanupNotices")> | undefined;
export function applyCleanup(view: CleanupView) { if (view.revision >= cleanup().revision) {
  setCleanup(view);
  if (noticeModule || view.rows.some((r) => r.notice || r.busy && r.source)) void (noticeModule ??= import("./cleanupNotices")).then((m) => m.syncCleanupNotices());
} }
let reading = false, again = false;
export async function refreshCleanup() {
  if (reading) { again = true; return; }
  reading = true;
  try { do { again = false; applyCleanup(await bridge.terminationCleanupView()); } while (again); }
  catch (error) { toast(tr("Working folders"), String(error), "warn", true); }
  finally { reading = false; }
}
if (bridge.inTauri()) {
  void bridge.onTerminationCleanupChanged(() => { void refreshCleanup(); }).then(() => refreshCleanup());
  void bridge.onTerminationFolderRemoved((label) => toast(tr("Folder removed"), label));
}
export const openCleanup = (selected?: string) => { setCleanupDialog({ selected, origin: document.activeElement instanceof HTMLElement ? document.activeElement : undefined }); };
export const closeCleanup = () => setCleanupDialog(undefined);
async function update(call: Promise<CleanupView>) { try { applyCleanup(await call); } catch (error) { toast(tr("Working folders"), String(error), "warn", true); } }
export const checkCleanup = (id: string) => update(bridge.terminationCleanupCheck(id));
export const removeCleanup = (consents: Consents) => update(bridge.terminationCleanupRemove(cleanup().revision, consents));
/** Clears only natively settled results; closing the review never calls this. */
export async function clearCleanupResults() { try { applyCleanup(await bridge.terminationCleanupKeep(cleanup().revision)); (await import("./cleanupNotices")).syncCleanupNotices(); } catch (error) { toast(tr("Working folders"), String(error), "warn", true); } }

/** Presentation only: every member retains its original opaque action handle. */
export interface CleanupFolder { key: string; rows: CleanupRow[]; primary: CleanupRow; status: CleanupRow["status"]; sources: Target[] }
const statusOrder: CleanupRow["status"][] = ["removed", "ready", "kept", "review", "checking", "removing", "stopping", "unknown"];
export function groupCleanupRows(rows: CleanupRow[]): CleanupFolder[] {
  const folders = new Map<string, CleanupRow[]>();
  for (const row of rows) {
    const key = row.folder_key || (row.path ? JSON.stringify([row.computer, row.path]) : row.id);
    const members = folders.get(key) ?? []; members.push(row); folders.set(key, members);
  }
  const priority = (r: CleanupRow) => r.busy ? 100 : r.can_force ? 90 : r.can_dismiss === false ? 80 : r.status === "unknown" ? 70 : r.ready ? 60 : 0;
  return [...folders].map(([key, members]) => ({ key, rows: members,
    primary: members.reduce((best, r) => priority(r) > priority(best) ? r : best),
    status: members.reduce((status, r) => statusOrder.indexOf(r.status) > statusOrder.indexOf(status) ? r.status : status, members[0].status),
    sources: members.flatMap((r) => r.source ? [r.source] : []).filter((source, i, all) => all.findIndex((s) => s.computer === source.computer && s.name === source.name && s.run_id === source.run_id) === i),
  }));
}
export const dismissCleanup = (id: string) => update(bridge.terminationCleanupDismiss(id, cleanup().revision));

export interface ManualCleanupView { confirmation: string; path: string; computer: string; branch: string; size: string; changes: number; untracked: number; ignored: string[]; ignored_count: number; ignored_size: string; required: Consents; ready: boolean; reason: string; sessions: string[]; processes: {pid: number; label: string}[] }
