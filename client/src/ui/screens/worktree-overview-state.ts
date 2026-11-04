import { createSignal } from "solid-js";
import { trNoop } from "../../i18n";
import { invoke } from "@tauri-apps/api/core";
import { toast } from "../../store/notices";
import type { Inspection } from "../../store/terminationCleanup";
export const PAGE = 256, LIMIT = 64;
export const CATEGORIES = [trNoop("Cleanup candidates"), trNoop("Review work"), trNoop("In use"), trNoop("Not checked"), trNoop("Main folders")];
export interface Filter { search: string; project: string; computer: string; category: number | null; show_main: boolean; sort: "size" | "activity" | "name" }
export const emptyFilter = (): Filter => ({ search: "", project: "", computer: "", category: null, show_main: false, sort: "size" });
export interface OverviewRow { id: string; path: string; branch: string; machine: string; computer: string; category: number; state: string; remains: string; size: string; fresh: boolean; sessions: string[] }
export interface Frame { id: string; reading: boolean; processed: number; total: number; incomplete: number; pending: number; repositories: number; partial: boolean; updated: number | null; page: { index: number; total: number; groups: { name: string; category: number; total: number; rows: OverviewRow[] }[]; selected: number; selected_ids: string[]; candidates: number; estimated_bytes: number | null; partial: boolean } }
export interface ManualRow { id: string; path: string; label: string; computer: string; machine: string; branch: string; size: string; status: "removed" | "kept" | "unknown" | "ready" | "checking" | "removing"; reason: string; details: string; ready: boolean; can_check: boolean; forgotten: boolean }
export interface Consents { local_files: boolean; discard_changes: boolean; unchecked_processes: boolean; merged_branches: boolean }
export const noConsents = (): Consents => ({ local_files: false, discard_changes: false, unchecked_processes: false, merged_branches: false });
export interface Manual { id: string; revision: number; rows: ManualRow[]; busy: boolean; cleaning: boolean; stopping: boolean; attempted: boolean; removed: number; kept: number; unknown: number; required: Consents }
type Call = <T>(command: string, args?: Record<string, unknown>) => Promise<T>;
/** One page and one manual snapshot only. Read requests coalesce; a changed
 * filter/page cannot be replaced by an older reply. No source/effect authority
 * is built from webview rows or cached paths. */
export function createOverview(call: Call = invoke, warn = (e: unknown) => toast("Worktrees", String(e), "warn", true)) {
  const [frame, setFrame] = createSignal<Frame>();
  const [batch, setBatch] = createSignal<Manual>();
  const [working, setWorking] = createSignal(false), [reading, setReading] = createSignal(false);
  const [generation, setGeneration] = createSignal(0);
  let id = "", closed = false, pulling = false, again = false, batchReading = false, batchAgain = false, version = 0, index = 0, filter = emptyFilter();
  const pull = async () => {
    if (!id || closed) return;
    if (pulling) { again = true; return; }
    pulling = true; setReading(true);
    try {
      do {
        again = false;
        const ticket = version, key = id;
        try {
          const value = await call<Frame>("worktree_overview_page", { id: key, filter: { ...filter }, index });
          if (!closed && id === key && ticket === version) { index = value.page.index; setFrame(value); }
        } catch (error) { if (!closed && ticket === version) warn(error); }
      } while (again && !closed);
    } catch (error) { if (!closed) warn(error); }
    finally { pulling = false; if (!closed) setReading(false); }
  };
  const updateBatch = async () => {
    if (closed || !batch()) return;
    if (batchReading) { batchAgain = true; return; }
    batchReading = true;
    try {
      do {
        batchAgain = false;
        const key = batch()!.id, value = await call<Manual>("worktree_manual_view", { id: key });
        if (!closed && batch()?.id === key && value.revision >= batch()!.revision) setBatch(value);
      } while (batchAgain && batch() && !closed);
    } catch (error) { if (!closed) warn(error); }
    finally { batchReading = false; }
  };
  const action = async (run: () => Promise<void>) => { if (closed || working()) return; setWorking(true); try { await run(); } catch (error) { if (!closed) warn(error); } finally { if (!closed) setWorking(false); } };
  return {
    frame, batch, working, reading, generation,
    async open(all = false) { await action(async () => { const value = await call<{ id: string }>("worktree_overview_open", { all }); if (closed) { void call("worktree_overview_close", { id: value.id }).catch(() => {}); return; } id = value.id; await pull(); }); },
    page(value: number) { index = value; version++; return pull(); },
    query(value: Filter) { filter = { ...value }; index = 0; version++; return pull(); },
    changed(key: string) { if (key === id) void pull(); },
    batchChanged() { void updateBatch(); },
    refresh() { return action(async () => { await call("worktree_overview_refresh", { id }); setGeneration((n) => n + 1); await pull(); }); },
    select(ids: string[], replace: boolean) { return action(async () => { await call("worktree_overview_select", { id, ids, replace }); await pull(); }); },
    candidates() { return action(async () => { await call("worktree_overview_candidates", { id, filter: { ...filter } }); await pull(); }); },
    review() { return action(async () => { const value = await call<Manual>("worktree_overview_review", { id }); if (!closed) { setBatch(value); await updateBatch(); } else void call("worktree_manual_stop", { id: value.id, close: true }).catch(() => {}); }); },
    check() { return action(async () => { const value = await call<Manual>("worktree_manual_check", { id: batch()!.id }); if (!closed) { setBatch(value); await updateBatch(); } }); },
    remove(consents: Consents) { return action(async () => { const current = batch()!; const value = await call<Manual>("worktree_manual_remove", { id: current.id, revision: current.revision, consents }); if (!closed) { setBatch(value); await updateBatch(); } }); },
    stop() { return action(async () => { const value = await call<Manual>("worktree_manual_stop", { id: batch()!.id, close: false }); if (!closed) { setBatch(value); await updateBatch(); } }); },
    async closeBatch() { if (closed || working() || batch()?.busy) return; await action(async () => { const key = batch()!.id; await call("worktree_manual_stop", { id: key, close: true }); if (!closed && batch()?.id === key) setBatch(undefined); }); if (!closed) { await call("worktree_overview_refresh", { id }).catch(warn); setGeneration((n) => n + 1); await pull(); } },
    inspect(row: string, file = "", offset = 0) { return call<Inspection>("worktree_overview_inspect", { id, row, file, offset }); },
    inspectBatch(key: string, row: string, file = "", offset = 0) { return call<Inspection>("worktree_manual_inspect", { id: key, row, file, offset }); },
    dispose() { closed = true; version++; if (batch()) void call("worktree_manual_stop", { id: batch()!.id, close: true }).catch(() => {}); if (id) void call("worktree_overview_close", { id }).catch(() => {}); },
  };
}
export type Overview = ReturnType<typeof createOverview>;
