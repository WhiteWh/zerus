import { describe, expect, it } from "vitest";
import { WorktreeReadLane } from "./worktree-read-lane";
const tick = async () => {for (let i = 0; i < 8; i++) await Promise.resolve();};
describe("project worktree read lane", () => {
  it("holds the slot for the actual read, coalesces rapid selection and settles obsolete requests", async () => {
    const lane = new WorktreeReadLane(); let release!: (value: Record<string, unknown>) => void; const calls: number[] = [];
    const first = lane.read(() => {calls.push(0); return new Promise((r) => {release = r;});}); await tick();
    const results = Array.from({length: 100}, (_, i) => lane.read(async () => {calls.push(i + 1); return {id: i};}).catch((e) => e));
    await tick(); expect(calls).toEqual([0]); release({id: "old"}); await first; await tick();
    expect(calls).toEqual([0, 100]); const values = await Promise.all(results); expect(values.slice(0, 99)).toEqual(Array(99).fill("Folder changed.")); expect(values[99]).toEqual({id: 99});
  });
  it("disposal drops the queued context without cancelling or replacing the actual read", async () => {
    const lane = new WorktreeReadLane(); let release!: (value: Record<string, unknown>) => void; let called = false;
    const first = lane.read(() => new Promise((r) => {release = r;})); await tick();
    const waiting = lane.read(async () => {called = true; return {};}).catch((e) => e); lane.close(); release({done: true});
    expect(await first).toEqual({done: true}); expect(await waiting).toBe("Worktree panel closed."); await tick(); expect(called).toBe(false); await expect(lane.read(async () => ({}))).rejects.toBe("Worktree panel closed.");
  });
});
