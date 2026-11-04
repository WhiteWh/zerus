import { createSignal } from "solid-js";
import type { Call } from "./project-swarm";
export interface Page<T> { offset: number; total: number; rows: T[] }
export interface Summary { id: string; setting: string; subject: string; computer: string | null }
export interface Choice { index: number; text: string; origins: { text: string; computer: string | null; local: boolean }[]; shown: boolean; removal: boolean }
export interface Review { id: string; summary: Summary; explanation: string; choices: Page<Choice>; chosen: number | null; selection: { outcome: string; action: string; confirmation: boolean } | null }
/** Discrete reads only. A dirty event coalesces; no timer or effect retry. */
export function createConflicts(call: Call, id: () => string, ready: () => boolean, completed: () => void) {
  const [list, setList] = createSignal<Page<Summary>>(), [review, setReview] = createSignal<Review>();
  const [busy, setBusy] = createSignal(false), [error, setError] = createSignal(""), [consumed, setConsumed] = createSignal(false);
  let gone = false, dirty = false;
  const query = (q: Record<string, unknown>) => call<Review>("project_swarm_conflict_review", { id: id(), query: q });
  const run = async (work: () => Promise<void>) => {
    if (gone || busy() || !ready()) return false;
    setBusy(true); setError("");
    try { await work(); return !gone; }
    catch (e) { if (!gone) {
      const message = String(e); setError(message);
      if (message.includes("too large to review") || message.includes("Invalid project catalog conflict")) { setList(undefined); setReview(undefined); }
    } return false; }
    finally { setBusy(false); completed(); if (dirty && !gone && ready()) void refresh(); }
  };
  const adopt = (next: Review) => { if (!gone) { setReview(next); setConsumed(false); } };
  const refresh = async () => run(async () => {
    dirty = false;
    const next = await call<Page<Summary>>("project_swarm_conflicts", { id: id(), page: Math.floor((list()?.offset ?? 0) / 64) });
    if (gone) return;
    setList(next);
    if (!next.total) { setReview(undefined); setConsumed(false); return; }
    const old = review();
    if (old && !consumed()) {
      try { adopt(await query({ action: "view", review: old.id })); return; }
      catch {
        // Exact variants or review identity changed. Never reuse its choice.
        setReview(undefined);
        try { adopt(await query({ action: "open", conflict: old.summary.id })); return; } catch { /* resolved/departed */ }
      }
    }
    adopt(await query({ action: "open", conflict: next.rows[0].id }));
  });
  const changed = () => { if (!gone) { dirty = true; if (!busy() && ready()) void refresh(); } };
  return { list, review, busy, error, consumed, changed,
    page: (page: number) => run(async () => {
      const next = await call<Page<Summary>>("project_swarm_conflicts", { id: id(), page });
      if (!gone) setList(next); // summary paging never changes the reviewed item
    }),
    open: (conflict: string) => run(async () => { adopt(await query({ action: "open", conflict })); }),
    values: (page: number) => run(async () => { const old = review(); if (old) adopt(await query({ action: "page", review: old.id, page })); }),
    choose: (choice: number) => run(async () => { const old = review(); if (old) adopt(await query({ action: "choose", review: old.id, choice })); }),
    consume: () => { setConsumed(true); dirty = true; },
    dispose: () => { gone = true; dirty = false; },
  };
}
export const pageRange = (page: Page<unknown>) => `${(page.offset + 1).toLocaleString()}–${Math.min(page.offset + page.rows.length, page.total).toLocaleString()} of ${page.total.toLocaleString()}`;
