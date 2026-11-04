import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Outcome } from "../types";
const calls = vi.hoisted(() => ({ view: vi.fn(), restore: vi.fn(), check: vi.fn(), remove: vi.fn(), overflow: vi.fn() }));
vi.mock("../bridge", () => ({ inTauri: () => false, terminationCleanupView: calls.view, terminationCleanupRestore: calls.restore,
  terminationCleanupCheck: calls.check, terminationCleanupRemove: calls.remove }));
vi.mock("./worktreeCleanup", () => ({ openWorktreeReview: calls.overflow }));
import { bus, MAX_NOTICES, sessionNoticeKey } from "./notices";
import { applyCleanup, cleanup, cleanupDialog, closeCleanup, emptyCleanup, type CleanupRow, type RestoreSource } from "./terminationCleanup";
import { clearCleanupNotices, cleanupRestoring, reportCleanup, restoreCleanup, syncCleanupNotices } from "./cleanupNotices";

const target = { computer: "example-peer", name: "claude/example/check", run_id: "run-1", conversation_id: "conversation-1" };
const source = (): RestoreSource => ({ target: { ...target }, machine: "11111111-1111-4111-8111-111111111111",
  archive_id: "22222222-2222-4222-8222-222222222222", status: "kept", overflow: false });
const row = (): CleanupRow => ({ id: "kept-1", label: "work", path: "/srv/example/work", machine: "example-peer", computer: target.computer,
  branch: "work", size: "1 KiB", status: "kept", reason: "private detail /srv/example/secret", details: "private details", can_check: true, ready: false,
  source: { ...target }, notice: "Working folder kept for review: 2 changed files.", restore: source() });
const done = (): Outcome => ({ outcome: "done", receipt: null, cleanup: null });
const flush = async () => { for (let n = 0; n < 20; ++n) await Promise.resolve(); };
let revision = 0;
function show(retained = row()) {
  applyCleanup({ ...emptyCleanup(), revision: ++revision, rows: [retained], kept: 1 });
  syncCleanupNotices();
  return bus.notices().find((n) => n.key === sessionNoticeKey(target.computer, target.name))!;
}

beforeEach(async () => {
  vi.useFakeTimers(); vi.clearAllMocks();
  vi.stubGlobal("HTMLElement", class {});
  vi.stubGlobal("document", { activeElement: undefined });
  calls.view.mockImplementation(async () => ({ ...emptyCleanup(), revision: ++revision }));
  calls.restore.mockResolvedValue(done()); calls.overflow.mockResolvedValue(undefined);
  clearCleanupNotices(); closeCleanup();
  applyCleanup({ ...emptyCleanup(), revision: ++revision }); await flush();
  for (const notice of bus.notices()) bus.dismiss(notice.id);
});
afterEach(() => { clearCleanupNotices(); vi.useRealTimers(); vi.unstubAllGlobals(); });

describe("archive cleanup recovery notices", () => {
  it("shows only native counts, selects the recorded row and performs no new check or restore", async () => {
    const warning = show(); await flush();
    expect(warning).toMatchObject({ title: "Session archived", tone: "warn", sticky: true, text: "Working folder kept for review: 2 changed files." });
    expect(warning.text).not.toContain("/srv/");
    expect(warning.actions?.map((a) => a.label)).toEqual(["Review…", "Restore session"]);
    await warning.actions![0].run();
    expect(cleanupDialog()?.selected).toBe("kept-1");
    expect(calls.check).not.toHaveBeenCalled(); expect(calls.remove).not.toHaveBeenCalled(); expect(calls.restore).not.toHaveBeenCalled();
  });

  it("pins the original restore identity, disables both actions and prevents a second call", async () => {
    let resolve!: (outcome: Outcome) => void;
    calls.restore.mockReturnValue(new Promise<Outcome>((settle) => { resolve = settle; }));
    const warning = show(); const original = source();
    const pending = warning.actions![1].run();
    expect(cleanupRestoring(original)).toBe(true);
    expect(warning.actions!.every((a) => a.disabled?.())).toBe(true);
    await restoreCleanup(original);
    expect(calls.restore).toHaveBeenCalledTimes(1); expect(calls.restore).toHaveBeenCalledWith(original);
    resolve(done()); await pending; await flush();
    expect(cleanupRestoring(original)).toBe(false); expect(cleanup().rows).toEqual([]);
    expect(bus.notices().find((n) => n.key === warning.key)).toMatchObject({ title: "Session restored", tone: "info", sticky: false });
    expect(calls.check).not.toHaveBeenCalled(); expect(calls.remove).not.toHaveBeenCalled();
  });

  it("keeps the warning and existing error after refusal, including at the notice cap", async () => {
    const warning = show();
    for (let n = 0; n < MAX_NOTICES - 1; ++n) bus.notify("warn", `Other outcome ${n}`, true);
    calls.restore.mockResolvedValue({ outcome: "failed", detail: "This session changed. Open its actions again." });
    await warning.actions![1].run();
    expect(bus.notices().some((n) => n.tone === "error" && n.text === "This session changed. Open its actions again.")).toBe(true);
    const retained = bus.notices().find((n) => n.key === warning.key)!;
    expect(retained).toMatchObject({ title: "Session archived", tone: "warn", sticky: true });
    expect(retained.actions!.every((a) => !a.disabled?.())).toBe(true);
    expect(calls.restore).toHaveBeenCalledTimes(1); expect(calls.remove).not.toHaveBeenCalled();
  });

  it("routes overflow Review to the maintained exact repository review and still offers Restore", async () => {
    const restore = { ...source(), status: "unconfirmed" as const, overflow: true };
    await reportCleanup({ outcome: "done", receipt: null,
      cleanup: { status: "unconfirmed", path: "/srv/example/work", common_dir: "/srv/example/main/.git", reason: "/private/secret" },
      cleanup_capture: { id: null, restore, notice: "Working folder kept for review: an unknown result." },
    }, target);
    const warning = bus.notices().find((n) => n.title === "Session archived")!;
    expect(warning.text).toBe("Working folder kept for review: an unknown result.");
    expect(warning.actions?.map((a) => a.label)).toEqual(["Review…", "Restore session"]);
    await warning.actions![0].run();
    expect(calls.overflow).toHaveBeenCalledWith(target.computer, "/srv/example/main/.git", "/srv/example/work", undefined);
    await warning.actions![1].run();
    expect(calls.restore).toHaveBeenCalledWith(restore);
    expect(calls.check).not.toHaveBeenCalled(); expect(calls.remove).not.toHaveBeenCalled();
  });

  it("does not offer Restore without its exact identity or resurrect a dismissed warning on a read", async () => {
    const retained = { ...row(), restore: null };
    const warning = show(retained); expect(warning.actions?.map((a) => a.label)).toEqual(["Review…"]);
    bus.dismiss(warning.id); syncCleanupNotices(); await flush();
    expect(bus.notices().some((n) => n.key === warning.key)).toBe(false);
    expect(calls.restore).not.toHaveBeenCalled();
  });
});
