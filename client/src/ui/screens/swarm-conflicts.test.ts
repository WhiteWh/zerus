import { describe, expect, it } from "vitest";
import { createConflicts, pageRange, type Page, type Review, type Summary } from "./swarm-conflicts";
import type { Call } from "./project-swarm";
const tick = async () => { for (let i = 0; i < 60; ++i) await Promise.resolve(); };
const summary = (i: number): Summary => ({ id: `conflict-${i}`, setting: "Project name", subject: `Project ${i}`, computer: null });
const detail = (s: Summary, generation = 1, offset = 0): Review => ({ id: `review-${s.id}-${generation}`, summary: s, explanation: "Explanation", choices: { offset, total: 75, rows: Array.from({ length: Math.min(32, 75 - offset) }, (_, i) => ({ index: offset + i, text: `Value ${offset + i}`, origins: [], shown: i === 0, removal: false })) }, chosen: null, selection: null });
function fixture() {
  let rows = Array.from({ length: 130 }, (_, i) => summary(i)), current: Review | undefined, generation = 1, refuse = false;
  const calls: { command: string; args?: Record<string, unknown> }[] = [];
  const call: Call = async <T,>(command: string, args?: Record<string, unknown>) => {
    calls.push({ command, args });
    if (refuse) throw Error("The project conflicts are too large to review.");
    if (command === "project_swarm_conflicts") { const offset = Math.min(Number(args?.page), Math.floor((rows.length - 1) / 64)) * 64; return { offset, total: rows.length, rows: rows.slice(offset, offset + 64) } as T; }
    const q = args?.query as { action: string; review?: string; conflict?: string; page?: number; choice?: number };
    if (q.action === "open") { const row = rows.find((r) => r.id === q.conflict); if (!row) throw Error("changed"); current = detail(row, generation); }
    else {
      if (!current || current.id !== q.review || !current.id.endsWith(`-${generation}`)) throw Error("This conflict changed. Review the updated choices again.");
      if (q.action === "page") current = detail(current.summary, generation, q.page! * 32);
      if (q.action === "choose") current = { ...current, chosen: q.choice!, selection: { action: "Keep this value", confirmation: false, outcome: "Set project name." } };
    }
    return current as T;
  };
  const state = createConflicts(call, () => "native-dialog", () => true, () => {});
  return { state, calls, changed: () => { ++generation; }, rows: (next: Summary[]) => { rows = next; }, refuse: () => { refuse = true; } };
}
describe("Swarm conflict native review reads", () => {
  it("keeps the reviewed conflict and choice while paging summaries", async () => {
    const f = fixture(); f.state.changed(); await tick(); await f.state.choose(1);
    const chosen = f.state.review(); await f.state.page(1);
    expect(f.state.list()?.offset).toBe(64); expect(f.state.review()).toBe(chosen);
    expect(f.state.review()?.chosen).toBe(1); expect(f.calls.every((c) => !c.command.endsWith("resolve"))).toBe(true);
  });
  it("clears the native choice when changing the values page", async () => {
    const f = fixture(); f.state.changed(); await tick(); await f.state.choose(1); await f.state.values(1);
    expect(f.state.review()?.choices.offset).toBe(32); expect(f.state.review()?.chosen).toBeNull(); expect(f.state.review()?.selection).toBeNull();
  });
  it("discards changed variants and old confirmation identity without applying", async () => {
    const f = fixture(); f.state.changed(); await tick(); await f.state.choose(1); const old = f.state.review()?.id;
    f.changed(); f.state.changed(); await tick();
    expect(f.state.review()?.id).not.toBe(old); expect(f.state.review()?.chosen).toBeNull();
    expect(f.calls.some((c) => c.command.endsWith("resolve"))).toBe(false);
  });
  it("clamps a departed last page after resolution and opens its first conflict", async () => {
    const f = fixture(); f.state.changed(); await tick(); await f.state.page(2); f.state.consume();
    f.rows(Array.from({ length: 128 }, (_, i) => summary(i))); f.state.changed(); await tick();
    expect(f.state.list()?.offset).toBe(64); expect(f.state.review()?.summary.id).toBe("conflict-64"); expect(f.state.review()?.chosen).toBeNull();
  });
  it("removes a refused oversized whole view and never exposes a prefix", async () => {
    const f = fixture(); f.state.changed(); await tick(); f.refuse(); f.state.changed(); await tick();
    expect(f.state.list()).toBeUndefined(); expect(f.state.review()).toBeUndefined(); expect(f.state.error()).toContain("too large to review");
  });
  it("coalesces a burst and ignores a late page after disposal", async () => {
    let release!: (page: Page<Summary>) => void, calls = 0;
    const call: Call = async <T,>() => { ++calls; return await new Promise<Page<Summary>>((r) => { release = r; }) as T; };
    const state = createConflicts(call, () => "native-dialog", () => true, () => {});
    state.changed(); for (let i = 0; i < 100; ++i) state.changed(); expect(calls).toBe(1);
    state.dispose(); release({ offset: 0, total: 0, rows: [] }); await tick(); expect(calls).toBe(1); expect(state.list()).toBeUndefined();
  });
  it("formats locale ranges with an en dash and of", () => {
    expect(pageRange({ offset: 64, total: 130, rows: Array(64) })).toBe("65–128 of 130");
    expect(pageRange({ offset: 8192, total: 8300, rows: Array(64) })).toBe("8,193–8,256 of 8,300");
  });
});
