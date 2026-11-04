import { afterEach, describe, expect, it, vi } from "vitest";
import { REMINDER_MS, WorktreeReminder, cleanupReminderView, type Reminder, type ReminderTarget } from "./worktree-reminder";
const tick = async () => { for (let i = 0; i < 12; i++) await Promise.resolve(); };
const target = (n: number): ReminderTarget => ({ key: `peer\nmachine-${n}\nfolder`, computer: "peer", machine: `machine-${n}`, path: "/srv/example/repo", common: "/srv/example/repo/.git" });
afterEach(() => vi.useRealTimers());
describe("Projects worktree cleanup reminder", () => {
  it("uses the mockup count and Qt singular/plural/disabled reasons", () => {
    expect(cleanupReminderView("ok", true, "/repo/.git", true, 4)).toEqual({enabled: true, label: "Clean up (4)…", title: "4 worktrees can be removed without losing work. Review them first."});
    expect(cleanupReminderView("ok", true, "/repo/.git", true, 1).title).toBe("1 worktree can be removed without losing work. Review it first.");
    expect(cleanupReminderView("ok", true, "/repo/.git", true, 0).label).toBe("Clean up…");
    expect(cleanupReminderView("plain", true, "", false, 4).title).toBe("Cleanup is available for Git repositories with linked worktrees.");
    expect(cleanupReminderView("ok", false, "/repo/.git", true, 4)).toEqual({enabled: false, label: "Clean up…", title: "Waiting for a current worktree catalog."});
    expect(cleanupReminderView("ok", true, "/repo/.git", false).title).toBe("This repository has no linked worktrees.");
  });
  it("keeps a five-minute visible cadence and stops timers and calls when hidden or disposed", async () => {
    vi.useFakeTimers(); const read = vi.fn(async () => ({count: 3, retry_ms: REMINDER_MS})); const changed = vi.fn();
    const reminder = new WorktreeReminder(read, changed);
    reminder.show(target(1), true); await tick(); expect(read).toHaveBeenCalledTimes(1); expect(changed).toHaveBeenLastCalledWith(3);
    await vi.advanceTimersByTimeAsync(REMINDER_MS - 1); expect(read).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(1); expect(read).toHaveBeenCalledTimes(2);
    reminder.show(target(1), false); await vi.advanceTimersByTimeAsync(REMINDER_MS * 3); expect(read).toHaveBeenCalledTimes(2);
    reminder.show(target(1), true); await tick(); expect(read).toHaveBeenCalledTimes(3);
    reminder.dispose(); await vi.advanceTimersByTimeAsync(REMINDER_MS * 3); expect(read).toHaveBeenCalledTimes(3);
  });
  it("coalesces rapid context changes behind the actual read and ignores its late result", async () => {
    vi.useFakeTimers(); let resolve!: (reply: Reminder) => void; const changed = vi.fn(); const calls: number[] = [];
    const reminder = new WorktreeReminder((t) => {const id = Number(t.machine.slice(8)); calls.push(id); return id === 0 ? new Promise((r) => {resolve = r;}) : Promise.resolve({count: id, retry_ms: REMINDER_MS});}, changed);
    reminder.show(target(0), true); await tick();
    for (let i = 1; i <= 100; i++) reminder.show(target(i), true);
    await tick(); expect(calls).toEqual([0]); resolve({count: 999, retry_ms: REMINDER_MS}); await tick();
    expect(calls).toEqual([0, 100]); expect(changed).not.toHaveBeenCalledWith(999); expect(changed).toHaveBeenLastCalledWith(100);
    reminder.dispose();
  });
  it("refreshes the visible panel through its one lane and discards the old count", async () => {
    vi.useFakeTimers(); let resolve!: (reply: Reminder) => void; const changed = vi.fn();
    const read = vi.fn().mockImplementationOnce(() => new Promise<Reminder>((r) => {resolve = r;})).mockResolvedValue({count: 1, retry_ms: REMINDER_MS});
    const reminder = new WorktreeReminder(read, changed);
    reminder.show(target(1), true); await tick(); reminder.refresh(); reminder.refresh(); await tick();
    expect(read).toHaveBeenCalledTimes(1);
    resolve({count: 99, retry_ms: REMINDER_MS}); await tick();
    expect(read).toHaveBeenCalledTimes(2); expect(changed).not.toHaveBeenCalledWith(99); expect(changed).toHaveBeenLastCalledWith(1);
    await vi.advanceTimersByTimeAsync(REMINDER_MS - 1); expect(read).toHaveBeenCalledTimes(2);
    reminder.show(target(1), false); reminder.refresh(); await tick(); expect(read).toHaveBeenCalledTimes(2);
    reminder.dispose(); reminder.refresh(); await vi.advanceTimersByTimeAsync(REMINDER_MS * 2); expect(read).toHaveBeenCalledTimes(2);
  });
  it("never starts a queued read after the panel hides, and failures wait before another attempt", async () => {
    vi.useFakeTimers(); let resolve!: (reply: Reminder) => void; const changed = vi.fn();
    const read = vi.fn(() => new Promise<Reminder>((r) => {resolve = r;})); const reminder = new WorktreeReminder(read, changed);
    reminder.show(target(0), true); await tick(); reminder.show(target(1), true); reminder.show(target(1), false);
    resolve({count: 99, retry_ms: 1}); await tick(); await vi.advanceTimersByTimeAsync(REMINDER_MS * 2);
    expect(read).toHaveBeenCalledTimes(1); expect(changed).not.toHaveBeenCalledWith(99); reminder.dispose();
    const failed = vi.fn(async () => {throw "unavailable";}); const retry = new WorktreeReminder(failed, vi.fn());
    retry.show(target(1), true); await tick(); await vi.advanceTimersByTimeAsync(REMINDER_MS - 1); expect(failed).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(1); expect(failed).toHaveBeenCalledTimes(2); retry.dispose();
  });
});
