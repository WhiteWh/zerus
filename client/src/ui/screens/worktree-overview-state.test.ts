import { describe, expect, it } from "vitest";
import { createOverview, emptyFilter, noConsents, type Frame, type Manual } from "./worktree-overview-state";
const tick = async () => { for (let i = 0; i < 12; i++) await Promise.resolve(); };
const frame = (index = 0): Frame => ({ id: "opaque-overview", reading: false, processed: 1, total: 1, incomplete: 0, pending: 0, repositories: 1, partial: false, updated: 1000, page: { index, total: 600, groups: [], selected: 1, selected_ids: ["opaque-row"], candidates: 2, estimated_bytes: 10, partial: false } });
const manual = (busy = false): Manual => ({ id: "opaque-review", revision: 4, rows: [], busy, cleaning: false, stopping: false, attempted: false, removed: 0, kept: 0, unknown: 0, required: noConsents() });
describe("bounded overview controller", () => {
  it("coalesces page/filter changes and refuses an older response without a GUI catalog cache", async () => {
    const requests: { command: string; args?: Record<string, unknown> }[] = [];
    let release!: (value: Frame) => void;
    const call = async <T,>(command: string, args?: Record<string, unknown>): Promise<T> => {
      requests.push({ command, args });
      if (command === "worktree_overview_open") return { id: "opaque-overview" } as T;
      if (command === "worktree_overview_page") {
        if (args?.index === 1) return await new Promise<Frame>((resolve) => { release = resolve; }) as T;
        return frame(args?.index as number) as T;
      }
      return undefined as T;
    };
    const state = createOverview(call, (e) => { throw Error(String(e)); });
    await state.open();
    const waiting = state.page(1); await tick();
    void state.query({ ...emptyFilter(), search: "latest" }); void state.page(2);
    expect(requests.filter((r) => r.command === "worktree_overview_page")).toHaveLength(2);
    release(frame(1)); await waiting;
    expect(state.frame()?.page.index).toBe(2);
    const last = requests.filter((r) => r.command === "worktree_overview_page").at(-1)!;
    expect(last.args).toMatchObject({ index: 2, filter: { search: "latest" } });
    expect(requests.some((r) => r.command.includes("remove"))).toBe(false);
    state.dispose();
  });
  it("settles an obsolete failed page into the latest query instead of losing the request", async () => {
    let fail!: (error: unknown) => void, count = 0;
    const warnings: unknown[] = [];
    const call = async <T,>(command: string): Promise<T> => {
      if (command === "worktree_overview_open") return { id: "opaque-overview" } as T;
      if (command === "worktree_overview_page") { count++; if (count === 2) return await new Promise<Frame>((_, reject) => { fail = reject; }) as T; return frame(count) as T; }
      return undefined as T;
    };
    const state = createOverview(call, (e) => warnings.push(e)); await state.open();
    const waiting = state.page(1); await tick(); void state.query({ ...emptyFilter(), project: "project-b" }); fail("old page failed"); await waiting;
    expect(count).toBe(3); expect(state.frame()?.page.index).toBe(3); expect(warnings).toEqual([]); state.dispose();
  });
  it("opening, selection and review run no removal; only explicit confirmation delivers exact opaque context", async () => {
    const requests: { command: string; args?: Record<string, unknown> }[] = [];
    let current = manual(false);
    const call = async <T,>(command: string, args?: Record<string, unknown>): Promise<T> => {
      requests.push({ command, args });
      if (command === "worktree_overview_open") return { id: "opaque-overview" } as T;
      if (command === "worktree_overview_page") return frame() as T;
      if (command === "worktree_overview_review") return manual(true) as T;
      if (command === "worktree_manual_view") return current as T;
      if (command === "worktree_manual_remove") { current = { ...manual(), revision: 6, attempted: true, unknown: 1 }; return current as T; }
      return undefined as T;
    };
    const state = createOverview(call, (e) => { throw Error(String(e)); }); await state.open(); await state.select(["opaque-row"], true); await state.review();
    expect(state.batch()?.busy).toBe(false); // Completion may precede the open reply.
    expect(requests.filter((r) => r.command.includes("remove"))).toEqual([]);
    const consents = { ...noConsents(), local_files: true };
    await state.remove(consents); state.batchChanged(); await tick();
    expect(requests.filter((r) => r.command === "worktree_manual_remove")).toEqual([{ command: "worktree_manual_remove", args: { id: "opaque-review", revision: 4, consents } }]);
    expect(state.batch()?.unknown).toBe(1); state.dispose();
    expect(requests.filter((r) => r.command === "worktree_manual_remove")).toHaveLength(1);
  });
  it("closing during open disposes the eventual opaque handle without querying or mutating its rows", async () => {
    let release!: (value: { id: string }) => void; const requests: string[] = [];
    const call = async <T,>(command: string): Promise<T> => { requests.push(command); if (command === "worktree_overview_open") return await new Promise<{ id: string }>((r) => { release = r; }) as T; return undefined as T; };
    const state = createOverview(call); const waiting = state.open(); state.dispose(); release({ id: "late-view" }); await waiting;
    expect(requests).toEqual(["worktree_overview_open", "worktree_overview_close"]); expect(state.frame()).toBeUndefined();
  });
});
