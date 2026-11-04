import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createRoot } from "solid-js";
import { createCatalog, toSession } from "./catalog";
import { createActions, withArchived, type Followed, type Selection } from "./actions";
import { createMarks, type MarksPort } from "./marks";
import { notificationsState, snapshotOf } from "../ui/sessions/actions-ui";
import * as bridge from "../bridge";
import { emptyBatch, type Action, type Batch, type Details, type Outcome, type RowOut, type ViewRow } from "../types";

vi.mock("../bridge", () => ({
  runAction: vi.fn(),
  terminationCleanupConfirm: vi.fn(),
  inspectSession: vi.fn(),
  pendingReadMarks: vi.fn(),
  attentionMarks: vi.fn(async () => ({ marks: {}, replies: {} })),
  updateAttentionMarks: vi.fn(async () => {}),
  queueReadMarks: vi.fn(async () => []),
  sessionReference: vi.fn(),
  copyCommand: vi.fn(),
  copyText: vi.fn(),
  setNotifications: vi.fn(),
}));

const mocked = vi.mocked(bridge);
const view = (extra: Partial<ViewRow> = {}): ViewRow => ({
  name: "claude/sample-project/a", status: "ready", phase: "idle", unread: false, run_id: "run-1", conversation_id: "conv-1", state: "running",
  tracked: true, agent: "claude", ...extra,
});
const rowOut = (extra: Partial<ViewRow> = {}, key = "run:a"): RowOut => ({ computer: "", key, row: view(extra) });
/** The catalog's own row for a view row, as the menus and dialogs snapshot it. */
const session = (extra: Partial<ViewRow> = {}, key = "run:a") => toSession("", key, view(extra));
const batchOf = (part: Partial<Batch>): Batch => ({ ...emptyBatch(), ...part });
const done = (cleanup: Outcome extends infer O ? (O extends { cleanup: infer C } ? C : never) : never = null): Outcome => ({ outcome: "done", receipt: {}, cleanup });
const details = (extra: Partial<Details> = {}): Details => ({
  run_id: "run-1", conversation_id: "conv-1", auth_required: false, expected_id: "", resume_message_can_send: true, allowed_actions: null, action_reasons: {},
  interrupt_supported: false, turn_started: 0, phase: "", ...extra,
});
const flush = async () => { for (let i = 0; i < 5; i++) await Promise.resolve(); };

function setup(entry = rowOut(), online: (computer: string) => boolean = () => true, selection: () => Selection = () => ({})) {
  const catalog = createCatalog();
  catalog.applyBatch(batchOf({ upserts: [entry] }));
  const actions = createActions(catalog.catalog, online, selection);
  return { ...catalog, ...actions };
}

beforeEach(() => {
  vi.useFakeTimers();
  vi.setSystemTime(1_790_000_000_000);
  mocked.runAction.mockReset();
  mocked.terminationCleanupConfirm.mockReset();
  mocked.inspectSession.mockReset();
  mocked.pendingReadMarks.mockReset();
  mocked.sessionReference.mockReset();
  mocked.copyCommand.mockReset();
  mocked.copyText.mockReset();
  mocked.copyText.mockResolvedValue(undefined);
  mocked.pendingReadMarks.mockResolvedValue([]);
  mocked.inspectSession.mockResolvedValue(details());
});
afterEach(() => vi.useRealTimers());

describe("perform", () => {
  it("sets the pending state while the action runs and clears it after", async () => {
    await createRoot(async (dispose) => {
      const a = setup();
      let finish: (o: Outcome) => void = () => {};
      mocked.runAction.mockReturnValue(new Promise((r) => { finish = r; }));
      expect(a.updating("", "claude/sample-project/a")).toBe(false);
      const p = a.perform(session(), { kind: "pause" });
      expect(a.updating("", "claude/sample-project/a")).toBe(true);
      finish(done());
      await p;
      expect(a.updating("", "claude/sample-project/a")).toBe(false);
      dispose();
    });
  });

  it("refuses without a call when the listed row has another run than the menu's", async () => {
    await createRoot(async (dispose) => {
      const a = setup();
      const menu = session();
      a.applyBatch(batchOf({ upserts: [rowOut({ run_id: "run-2" })] }));
      const out = await a.perform(menu, { kind: "pause" });
      expect(mocked.runAction).not.toHaveBeenCalled();
      expect(out.outcome).toBe("failed");
      expect(a.notices().map((n) => n.text)).toEqual(["This session changed. Open its actions again."]);
      expect(a.notices()[0].tone).toBe("info");
      dispose();
    });
  });

  it("never evicts the notice being added, even over five sticky ones", () => {
    createRoot((dispose) => {
      const a = setup();
      for (let i = 0; i < 5; i++) a.notify("info", `Worktree kept ${i}`, true);
      a.notify("info", "Archive: 5 of 5 completed.");
      const texts = a.notices().map((n) => n.text);
      expect(texts).toHaveLength(5);
      expect(texts).toContain("Archive: 5 of 5 completed.");
      expect(texts).not.toContain("Worktree kept 0");
      dispose();
    });
  });

  it("refuses when the row is gone", async () => {
    await createRoot(async (dispose) => {
      const a = setup();
      await a.perform(session({}, "run:zz"), { kind: "pause" });
      expect(mocked.runAction).not.toHaveBeenCalled();
      expect(a.notices()).toHaveLength(1);
      dispose();
    });
  });

  it("words the success of each action", async () => {
    await createRoot(async (dispose) => {
      const a = setup();
      mocked.runAction.mockResolvedValue(done());
      const cases: [Action, string | undefined][] = [
        [{ kind: "rename", new_name: "claude/sample-project/b", archive_id: null }, "Session renamed. Refreshing…"],
        [{ kind: "pause" }, "Session updated. Refreshing…"],
        [{ kind: "resume" }, "Session updated. Refreshing…"],
        [{ kind: "archive" }, "Session updated. Refreshing…"],
        [{ kind: "terminate" }, "Session updated. Refreshing…"],
        [{ kind: "kill" }, "Session updated. Refreshing…"],
      ];
      for (const [action, text] of cases) {
        for (const n of a.notices()) a.dismiss(n.id);
        await a.perform(session(), action);
        expect(a.notices().map((n) => n.text)).toEqual(text ? [text] : []);
        if (text) {
          expect(a.notices()[0].tone).toBe("info");
          expect(a.notices()[0].sticky).toBe(false);
          vi.advanceTimersByTime(6000);
          expect(a.notices()).toEqual([]);
        }
      }
      dispose();
    });
  });

  it("words Stop as Qt does: sent, already ended, or failed", async () => {
    await createRoot(async (dispose) => {
      const a = setup();
      const stop: Action = { kind: "interrupt", turn_started: 1793000000.5 };
      mocked.runAction.mockResolvedValueOnce(done());
      await a.perform(session(), stop);
      expect(a.notices()).toMatchObject([{ tone: "info", sticky: false,
        text: "Interrupt sent to claude/sample-project/a (this computer). Waiting for the agent to stop…" }]);
      vi.advanceTimersByTime(6000);
      mocked.runAction.mockResolvedValueOnce({ outcome: "failed", detail: "hgs: This turn is no longer working" });
      await a.perform(session(), stop);
      expect(a.notices()).toMatchObject([{ tone: "info", sticky: false,
        text: "Stop claude/sample-project/a (this computer): the turn already ended or is waiting for input. No action needed." }]);
      vi.advanceTimersByTime(6000);
      mocked.runAction.mockResolvedValueOnce({ outcome: "failed", detail: "Request timed out. Check Terminal before retrying." });
      await a.perform(session(), stop);
      expect(a.notices()).toMatchObject([{ tone: "error", sticky: true,
        text: "Stop claude/sample-project/a (this computer) failed: Request timed out. Check Terminal before retrying.\nOpen Terminal to check the agent before trying again." }]);
      expect(mocked.runAction.mock.calls[0]).toEqual([{ computer: "", name: "claude/sample-project/a", run_id: "run-1", conversation_id: "conv-1" }, stop]);
      dispose();
    });
  });

  it("keeps a failure as a sticky error", async () => {
    await createRoot(async (dispose) => {
      const a = setup();
      mocked.runAction.mockResolvedValue({ outcome: "failed", detail: "hgs pause failed" });
      await a.perform(session(), { kind: "pause" });
      vi.advanceTimersByTime(60_000);
      expect(a.notices()).toMatchObject([{ tone: "error", text: "hgs pause failed", sticky: true }]);
      dispose();
    });
  });

  it("shows an unknown outcome (a rejected call) as a failure and never retries it", async () => {
    await createRoot(async (dispose) => {
      const a = setup();
      mocked.runAction.mockRejectedValue("lost the answer");
      const out = await a.perform(session(), { kind: "pause" });
      expect(out).toEqual({ outcome: "failed", detail: "lost the answer" });
      expect(a.notices()[0]).toMatchObject({ tone: "error", sticky: true });
      vi.advanceTimersByTime(20_000);
      expect(mocked.runAction).toHaveBeenCalledTimes(1);
      dispose();
    });
  });

  it("keeps cleanup warnings generic without a structured row and never copies CLI reasons", async () => {
    await createRoot(async (dispose) => {
      const a = setup();
      mocked.runAction.mockResolvedValue(done({ status: "kept", path: "/home/example/wt", common_dir: "/home/example/.git", reason: "uncommitted changes" }));
      await a.perform(session(), { kind: "archive" });
      expect(a.notices().find((n) => n.sticky)).toMatchObject({ tone: "warn", text: "Working folder kept for review." });
      for (const n of a.notices()) a.dismiss(n.id);
      mocked.runAction.mockResolvedValue(done({ status: "unconfirmed", path: "/home/example/wt", common_dir: "/home/example/.git", reason: null }));
      await a.perform(session(), { kind: "terminate" });
      expect(a.notices().find((n) => n.sticky)?.text).toBe("Working folder kept for review.");
      for (const n of a.notices()) a.dismiss(n.id);
      mocked.runAction.mockResolvedValue(done({ status: "removed", path: "/home/example/wt", common_dir: "/home/example/.git", reason: null }));
      await a.perform(session(), { kind: "archive" });
      expect(a.notices().map((n) => n.text)).toContain("Worktree removed automatically. The branch and archived conversation are kept.");
      dispose();
    });
  });

  it("calls runAction exactly once per perform, even 10 s later", async () => {
    await createRoot(async (dispose) => {
      const a = setup();
      mocked.runAction.mockResolvedValue({ outcome: "failed", detail: "x" });
      await a.perform(session(), { kind: "resume" });
      vi.advanceTimersByTime(10_000);
      await Promise.resolve();
      expect(mocked.runAction).toHaveBeenCalledTimes(1);
      expect(mocked.runAction).toHaveBeenCalledWith({ computer: "", name: "claude/sample-project/a", run_id: "run-1", conversation_id: "conv-1" }, { kind: "resume" });
      dispose();
    });
  });

  it("replaces a keyed notice in place of a second one and clears it by its key", () => {
    createRoot((dispose) => {
      const a = setup();
      a.notify("info", "Session updated. Refreshing…");
      a.notify("error", "Project sync unavailable: hgs did not answer in time", true, "project-sync");
      a.notify("warn", "2 project conflict(s) need review in Swarm.", true, "project-sync");
      expect(a.notices().map((n) => n.text)).toEqual(["Session updated. Refreshing…", "2 project conflict(s) need review in Swarm."]);
      a.clearNotice("project-sync");
      expect(a.notices().map((n) => n.text)).toEqual(["Session updated. Refreshing…"]);
      a.clearNotice("project-sync");
      expect(a.notices()).toHaveLength(1);
      dispose();
    });
  });

  it("lets other stores raise notices through notify", async () => {
    await createRoot(async (dispose) => {
      const a = setup();
      a.notify("info", "1 reply marked as read.");
      a.notify("error", "This session is no longer available. Open its actions again.", true);
      expect(a.notices()).toMatchObject([
        { tone: "info", text: "1 reply marked as read.", sticky: false },
        { tone: "error", text: "This session is no longer available. Open its actions again.", sticky: true },
      ]);
      vi.advanceTimersByTime(6000);
      expect(a.notices().map((n) => n.tone)).toEqual(["error"]);
      dispose();
    });
  });

  it("caps the notices at 5, dropping non-sticky ones before a sticky warning", async () => {
    await createRoot(async (dispose) => {
      const a = setup();
      mocked.runAction.mockResolvedValueOnce({ outcome: "failed", detail: "kept" });
      await a.perform(session(), { kind: "pause" });
      mocked.runAction.mockResolvedValue(done());
      // A repeated passing result shows once (the bus coalesces it).
      for (let i = 0; i < 6; i++) await a.perform(session(), { kind: "pause" });
      expect(a.notices().map((n) => n.text)).toEqual(["kept", "Session updated. Refreshing…"]);
      for (let i = 0; i < 6; i++) a.notify("info", `passing ${i}`);
      expect(a.notices()).toHaveLength(5);
      expect(a.notices()[0]).toMatchObject({ text: "kept", sticky: true });
      mocked.runAction.mockResolvedValue({ outcome: "failed", detail: "x" });
      for (let i = 0; i < 8; i++) await a.perform(session(), { kind: "pause" });
      expect(a.notices()).toHaveLength(5);
      expect(a.notices().every((n) => n.sticky)).toBe(true);
      dispose();
    });
  });

  it("quietly shows only the worktree lines, for a selection batch", async () => {
    await createRoot(async (dispose) => {
      const a = setup();
      mocked.runAction.mockResolvedValueOnce(done()).mockResolvedValueOnce({ outcome: "failed", detail: "x" });
      expect(await a.perform(session(), { kind: "pause" }, { quiet: true })).toMatchObject({ outcome: "done" });
      expect(await a.perform(session(), { kind: "forget_binding" }, { quiet: true })).toEqual({ outcome: "failed", detail: "x" });
      expect(await a.perform(session({ run_id: "run-2" }), { kind: "pause" }, { quiet: true })).toMatchObject({ outcome: "failed" });
      expect(a.notices()).toEqual([]);
      mocked.runAction.mockResolvedValueOnce(done({ status: "kept", path: "/home/example/wt", common_dir: "/home/example/.git", reason: "uncommitted changes" }));
      await a.perform(session(), { kind: "terminate" }, { quiet: true });
      expect(a.notices()).toEqual([expect.objectContaining({ tone: "warn", text: "Working folder kept for review.", sticky: true })]);
      mocked.runAction.mockResolvedValueOnce(done({ status: "removed", path: "/home/example/wt", common_dir: "/home/example/.git", reason: null }));
      await a.perform(session(), { kind: "archive" }, { quiet: true });
      expect(a.notices().map((n) => n.text)).toEqual(["Working folder kept for review.", "Worktree removed automatically. The branch and archived conversation are kept."]);
      dispose();
    });
  });

  it("returns each notice's id, so it can be dismissed", async () => {
    await createRoot(async (dispose) => {
      const a = setup();
      const first = a.notify("info", "one");
      const second = a.notify("info", "two");
      expect(first).not.toBe(second);
      a.dismiss(first);
      expect(a.notices().map((n) => n.text)).toEqual(["two"]);
      dispose();
    });
  });
});

describe("targets", () => {
  it("names a session without a run for kill, rename, pause, resume and archive", async () => {
    await createRoot(async (dispose) => {
      const bare = rowOut({ run_id: null, conversation_id: null, tracked: false }, "name:claude/sample-project/a");
      const a = setup(bare);
      mocked.runAction.mockResolvedValue(done());
      const row = toSession("", bare.key, bare.row);
      const actions: Action[] = [{ kind: "kill" }, { kind: "rename", new_name: "claude/sample-project/b", archive_id: null }, { kind: "pause" }, { kind: "resume" }, { kind: "archive" }];
      for (const action of actions) await a.perform(row, action);
      expect(mocked.runAction.mock.calls.map((c) => c[0])).toEqual(actions.map(() => ({ computer: "", name: "claude/sample-project/a", run_id: "", conversation_id: "" })));
      dispose();
    });
  });

  it("keeps interrupt, mark read and terminate to an identified run", async () => {
    await createRoot(async (dispose) => {
      const bare = rowOut({ run_id: null, conversation_id: null }, "name:claude/sample-project/a");
      const a = setup(bare);
      const row = toSession("", bare.key, bare.row);
      for (const action of [{ kind: "interrupt", turn_started: 1 }, { kind: "mark_read", reply_id: null, attention_signature: null }, { kind: "terminate" }] as Action[]) {
        expect((await a.perform(row, action)).outcome).toBe("failed");
      }
      expect(mocked.runAction).not.toHaveBeenCalled();
      // Each refusal says so; the repeated passing notice shows once.
      expect(a.notices().map((n) => n.text)).toEqual(["This session changed. Open its actions again."]);
      dispose();
    });
  });
});

describe("rechecks after a confirmation", () => {
  it("ends the session with the dialog's call while it still runs", async () => {
    await createRoot(async (dispose) => {
      const a = setup();
      mocked.runAction.mockResolvedValue(done());
      await a.end(session(), "kill");
      expect(mocked.runAction.mock.calls[0][1]).toEqual({ kind: "kill" });
      dispose();
    });
  });

  it("makes no call and no notice when the machine went offline before the click", async () => {
    await createRoot(async (dispose) => {
      const a = setup(rowOut(), () => false);
      expect((await a.end(session(), "terminate")).outcome).toBe("failed");
      expect(mocked.runAction).not.toHaveBeenCalled();
      expect(a.notices()).toEqual([]);
      dispose();
    });
  });

  it("refuses with Qt's text when the session stopped running meanwhile", async () => {
    await createRoot(async (dispose) => {
      const a = setup();
      const dialog = session();
      a.applyBatch(batchOf({ upserts: [rowOut({ state: "paused" })] }));
      expect((await a.end(dialog, "terminate")).outcome).toBe("failed");
      const menu = session();
      a.applyBatch(batchOf({ upserts: [rowOut({ state: "running" })] }));
      a.applyBatch(batchOf({ upserts: [rowOut({ state: "stopped" })] }));
      await a.change(menu);
      expect(mocked.runAction).not.toHaveBeenCalled();
      expect(a.notices()).toMatchObject([
        { tone: "error", sticky: true, text: "The session state changed. Open its actions again." },
        { tone: "error", sticky: true, text: "The session state changed. Open its actions again." },
      ]);
      dispose();
    });
  });

  it("pauses a running row from its menu and resumes a stopped one", async () => {
    await createRoot(async (dispose) => {
      const a = setup();
      mocked.runAction.mockResolvedValue(done());
      await a.change(session());
      a.applyBatch(batchOf({ upserts: [rowOut({ state: "stopped" })] }));
      await a.change(session({ state: "stopped" }));
      expect(mocked.runAction.mock.calls.map((c) => c[1])).toEqual([{ kind: "pause" }, { kind: "resume" }]);
      dispose();
    });
  });
});

describe("detailsFor", () => {
  const state = (extra: Record<string, unknown> = {}) => ({ run_id: "run-1", conversation_id: "conv-1", phase: "tool", turn_started: 5, interrupt_supported: true, ...extra });

  it("takes the followed session's state value and runs no inspect", async () => {
    await createRoot(async (dispose) => {
      const a = setup();
      const d = a.detailsFor(session(), true, { value: state(), settled: true });
      await flush();
      expect(d).toMatchObject({ run_id: "run-1", interrupt_supported: true, turn_started: 5, phase: "tool" });
      expect(mocked.inspectSession).not.toHaveBeenCalled();
      dispose();
    });
  });

  it("inspects once the subscription answered without a value of this run, and not before", async () => {
    await createRoot(async (dispose) => {
      const a = setup();
      const waiting: Followed = { value: undefined, settled: false };
      expect(a.detailsFor(session(), true, waiting)).toBeUndefined();
      await flush();
      expect(mocked.inspectSession).not.toHaveBeenCalled();
      const other: Followed = { value: state({ run_id: "run-0" }), settled: true };
      expect(a.detailsFor(session(), true, other)).toBeUndefined();
      await flush();
      expect(mocked.inspectSession).toHaveBeenCalledTimes(1);
      expect(mocked.inspectSession).toHaveBeenCalledWith({ computer: "", name: "claude/sample-project/a", run_id: "run-1", conversation_id: "conv-1" }, null);
      expect(a.detailsFor(session(), true, other)?.run_id).toBe("run-1");
      expect(mocked.inspectSession).toHaveBeenCalledTimes(1);
      dispose();
    });
  });

  it("never inspects while the machine is offline", async () => {
    await createRoot(async (dispose) => {
      const a = setup();
      expect(a.detailsFor(session(), false)).toBeUndefined();
      await flush();
      expect(mocked.inspectSession).not.toHaveBeenCalled();
      dispose();
    });
  });

  it("refetches on a run change and drops a reply for another run", async () => {
    await createRoot(async (dispose) => {
      const a = setup();
      a.detailsFor(session(), true);
      await flush();
      expect(a.detailsFor(session(), true)?.run_id).toBe("run-1");
      a.applyBatch(batchOf({ upserts: [rowOut({ run_id: "run-2" })] }));
      expect(a.detailsFor(session({ run_id: "run-2" }), true)).toBeUndefined();
      await flush();
      expect(mocked.inspectSession).toHaveBeenCalledTimes(2);
      expect(a.detailsFor(session({ run_id: "run-2" }), true)).toBeUndefined();
      expect(mocked.inspectSession).toHaveBeenCalledTimes(2);
      dispose();
    });
  });

  it("refetches when the state changes and runs one inspect per session at a time", async () => {
    await createRoot(async (dispose) => {
      const a = setup();
      let finish: (d: Details) => void = () => {};
      mocked.inspectSession.mockReturnValueOnce(new Promise((r) => { finish = r; }));
      a.detailsFor(session(), true);
      a.detailsFor(session({ state: "paused" }), true);
      expect(mocked.inspectSession).toHaveBeenCalledTimes(1);
      finish(details());
      await flush();
      expect(mocked.inspectSession).toHaveBeenCalledTimes(2);
      dispose();
    });
  });
});

describe("pending guard", () => {
  it("makes no call and no notice for a second write while one is pending", async () => {
    await createRoot(async (dispose) => {
      const a = setup();
      let finish: (o: Outcome) => void = () => {};
      mocked.runAction.mockReturnValue(new Promise((r) => { finish = r; }));
      const first = a.perform(session(), { kind: "pause" });
      expect(a.interrupting("", "claude/sample-project/a")).toBe(false);
      await a.perform(session(), { kind: "pause" });
      expect(mocked.runAction).toHaveBeenCalledTimes(1);
      expect(a.notices()).toEqual([]);
      finish(done());
      await first;
      dispose();
    });
  });

  it("sends Stop beside a pending write, and a running Stop shows no Updating", async () => {
    await createRoot(async (dispose) => {
      const a = setup();
      const finishes: ((o: Outcome) => void)[] = [];
      mocked.runAction.mockImplementation(() => new Promise((r) => { finishes.push(r); }));
      const write = a.perform(session(), { kind: "pause" });
      const stop = a.perform(session(), { kind: "interrupt", turn_started: 1 });
      expect(mocked.runAction).toHaveBeenCalledTimes(2);
      expect(a.updating("", "claude/sample-project/a")).toBe(true);
      expect(a.interrupting("", "claude/sample-project/a")).toBe(true);
      finishes[0](done());
      await write;
      expect(a.updating("", "claude/sample-project/a")).toBe(false);
      expect(a.interrupting("", "claude/sample-project/a")).toBe(true);
      await a.perform(session(), { kind: "interrupt", turn_started: 1 });
      expect(mocked.runAction).toHaveBeenCalledTimes(2);
      finishes[1](done());
      await stop;
      expect(a.interrupting("", "claude/sample-project/a")).toBe(false);
      dispose();
    });
  });

  it("treats a busy rejection as a silent no-op", async () => {
    await createRoot(async (dispose) => {
      const a = setup();
      mocked.runAction.mockRejectedValue("busy");
      await a.perform(session(), { kind: "pause" });
      expect(a.notices()).toEqual([]);
      dispose();
    });
  });
});

describe("copy", () => {
  it("copies silently and reports a failure as a sticky error", async () => {
    await createRoot(async (dispose) => {
      const a = setup();
      await a.copy("claude/sample-project/a");
      expect(mocked.copyText).toHaveBeenCalledWith("claude/sample-project/a");
      expect(a.notices()).toEqual([]);
      mocked.copyText.mockRejectedValue("Nothing to copy.");
      await a.copy("x");
      expect(a.notices()).toEqual([expect.objectContaining({ tone: "error", sticky: true, title: "Not copied", text: "Nothing to copy." })]);
      dispose();
    });
  });

  it("copies a command with the archive id only for an archived row", async () => {
    await createRoot(async (dispose) => {
      const a = setup();
      mocked.copyCommand.mockResolvedValue("hgs attach x");
      await a.copyCommand(session());
      expect(mocked.copyCommand).toHaveBeenCalledWith("", "claude/sample-project/a", null);
      expect(mocked.copyText).toHaveBeenCalledWith("hgs attach x");
      a.applyBatch(batchOf({ upserts: [rowOut({ state: "archived", archive_id: "ar1" })] }));
      await a.copyCommand(session({ state: "archived", archive_id: "ar1" }));
      expect(mocked.copyCommand).toHaveBeenLastCalledWith("", "claude/sample-project/a", "ar1");
      dispose();
    });
  });

  it("copies the reference of the details' run, then says so", async () => {
    await createRoot(async (dispose) => {
      const a = setup();
      mocked.sessionReference.mockResolvedValue("zerus://session?x=1");
      await a.copyReference(session(), details({ run_id: "run-9", conversation_id: "conv-9" }));
      expect(mocked.sessionReference).toHaveBeenCalledWith({ computer: "", name: "claude/sample-project/a", run_id: "run-9", conversation_id: "conv-9" }, null);
      expect(mocked.copyText).toHaveBeenCalledWith("zerus://session?x=1");
      expect(a.notices()).toEqual([expect.objectContaining({ tone: "info", title: "Copied", text: "The session reference is on the clipboard", sticky: false })]);
      a.applyBatch(batchOf({ upserts: [rowOut({ state: "archived", archive_id: "ar1" })] }));
      await a.copyReference(session({ state: "archived", archive_id: "ar1" }));
      expect(mocked.sessionReference).toHaveBeenLastCalledWith({ computer: "", name: "claude/sample-project/a", run_id: "run-1", conversation_id: "conv-1" }, "ar1");
      dispose();
    });
  });

  it("shows a reference failure as a sticky error and copies nothing", async () => {
    await createRoot(async (dispose) => {
      const a = setup();
      mocked.sessionReference.mockRejectedValue("hgs: no such run");
      await a.copyReference(session());
      expect(mocked.copyText).not.toHaveBeenCalled();
      expect(a.notices()).toEqual([expect.objectContaining({ tone: "error", sticky: true, text: "hgs: no such run" })]);
      dispose();
    });
  });
});

describe("header copy identity", () => {
  it("copies the session ID and says what was copied", async () => {
    await createRoot(async (dispose) => {
      const a = setup();
      await a.copyIdentity(session());
      expect(mocked.copyText).toHaveBeenCalledWith("conv-1");
      expect(a.notices()).toEqual([expect.objectContaining({ title: "Copied", text: "The session ID is on the clipboard", tone: "info" })]);
      dispose();
    });
  });
  it("copies the run ID as a fallback and names it", async () => {
    await createRoot(async (dispose) => {
      const a = setup();
      await a.copyIdentity(session(), details({ conversation_id: "" }));
      expect(mocked.copyText).toHaveBeenCalledWith("run-1");
      expect(a.notices()[0].text).toBe("The run ID is on the clipboard");
      dispose();
    });
  });
  it("copies a selected child's conversation or agent ID and names the result", async () => {
    await createRoot(async (dispose) => {
      const a = setup(rowOut(), () => true, () => ({ key: "\nrun:a", subagent: "child" }));
      await a.copyIdentity(session(), details(), { id: "child", conversationId: "child-conversation" });
      expect(mocked.copyText).toHaveBeenLastCalledWith("child-conversation");
      expect(a.notices().at(-1)?.text).toBe("The session ID is on the clipboard");
      await a.copyIdentity(session(), details(), { id: "child" });
      expect(mocked.copyText).toHaveBeenLastCalledWith("child");
      expect(a.notices().at(-1)?.text).toBe("The agent ID is on the clipboard");
      dispose();
    });
  });
  it("copies nothing after the selected child or parent identity changes", async () => {
    await createRoot(async (dispose) => {
      const a = setup(rowOut(), () => true, () => ({ key: "\nrun:a", subagent: "new-child" }));
      expect((await a.copyIdentity(session(), details(), { id: "old-child" })).outcome).toBe("failed");
      expect((await a.copyIdentity(session())).outcome).toBe("failed");
      a.applyBatch(batchOf({ upserts: [rowOut({ run_id: "new-run" })] }));
      expect((await a.copyIdentity(session(), details(), { id: "new-child" })).outcome).toBe("failed");
      expect(mocked.copyText).not.toHaveBeenCalled();
      dispose();
    });
  });
  it("copies nothing without an ID, and a clipboard failure never reports success", async () => {
    await createRoot(async (dispose) => {
      const a = setup(rowOut({ run_id: undefined, conversation_id: undefined }));
      expect((await a.copyIdentity(session({ run_id: undefined, conversation_id: undefined }))).outcome).toBe("failed");
      expect(mocked.copyText).not.toHaveBeenCalled();
      a.applyBatch(batchOf({ upserts: [rowOut()] }));
      mocked.copyText.mockRejectedValue("Clipboard unavailable");
      expect((await a.copyIdentity(session())).outcome).toBe("failed");
      expect(a.notices()).toEqual([expect.objectContaining({ tone: "error", sticky: true })]);
      dispose();
    });
  });
});

describe("copy click-time recheck", () => {
  const gone = "This session is no longer available. Open its actions again.";
  it("copies nothing and says so when the row is gone", async () => {
    await createRoot(async (dispose) => {
      const a = setup();
      const old = session();
      a.applyBatch(batchOf({ removes: [{ computer: "", key: "run:a" }] }));
      for (const out of [await a.copyName(old), await a.copyCommand(old), await a.copyFolder(old), await a.copyReference(old)]) expect(out.outcome).toBe("failed");
      expect(mocked.copyText).not.toHaveBeenCalled();
      expect(a.notices()).toHaveLength(4);
      expect(a.notices()[0]).toMatchObject({ text: gone, sticky: true });
      dispose();
    });
  });

  it("copies nothing when the conversation changed", async () => {
    await createRoot(async (dispose) => {
      const a = setup();
      const old = session();
      a.applyBatch(batchOf({ upserts: [rowOut({ conversation_id: "conv-2" })] }));
      await a.copyName(old);
      expect(mocked.copyText).not.toHaveBeenCalled();
      expect(a.notices()).toEqual([expect.objectContaining({ text: gone, sticky: true })]);
      dispose();
    });
  });

  it("copies nothing when created changed", async () => {
    await createRoot(async (dispose) => {
      const a = setup(rowOut({ created: 1_790_000_000 }));
      const old = session({ created: 1_790_000_000 });
      a.applyBatch(batchOf({ upserts: [rowOut({ created: 1_790_000_001 })] }));
      await a.copyName(old);
      expect(mocked.copyText).not.toHaveBeenCalled();
      expect(a.notices()).toEqual([expect.objectContaining({ text: gone, sticky: true })]);
      dispose();
    });
  });

  it("copies the current name and folder of a renamed row", async () => {
    await createRoot(async (dispose) => {
      const a = setup();
      const old = session();
      a.applyBatch(batchOf({ upserts: [rowOut({ name: "claude/sample-project/b", cwd: "/home/example/b" })] }));
      await a.copyName(old);
      expect(mocked.copyText).toHaveBeenLastCalledWith("claude/sample-project/b");
      await a.copyFolder(old);
      expect(mocked.copyText).toHaveBeenLastCalledWith("/home/example/b");
      expect(a.notices()).toEqual([]);
      dispose();
    });
  });

  it("copies nothing for an archived row without an archive id", async () => {
    await createRoot(async (dispose) => {
      const a = setup(rowOut({ state: "archived" }));
      const out = await a.copyCommand(session({ state: "archived" }));
      expect(out.outcome).toBe("failed");
      expect(mocked.copyCommand).not.toHaveBeenCalled();
      expect(mocked.copyText).not.toHaveBeenCalled();
      dispose();
    });
  });
});

describe("toggleMute", () => {
  const quietPort = (): MarksPort => ({
    attentionMarks: async () => ({ marks: {}, replies: {} }),
    updateAttentionMarks: async () => {},
    queueReadMarks: async () => [],
    pendingReadMarks: async () => [],
  });
  const shared = (extra: Partial<ViewRow> = {}, key = "run:a"): RowOut => rowOut({ shared_notifications: true, notification_revision: 3, muted: false, created: 1_790_000_000, ...extra }, key);
  const prefs = (muted: boolean, revision: number, conversation = "conv-1") => ({ conversation_id: conversation, muted, revision, suppressed: {} });

  async function setupMute(rows: RowOut[] = [shared()], online: (computer: string) => boolean = () => true) {
    const marks = createMarks(quietPort(), { load: () => [], save: () => {} });
    const catalog = createCatalog(marks);
    catalog.applyBatch(batchOf({ upserts: rows }));
    await marks.load();
    const actions = createActions(catalog.catalog, online, () => ({}), marks);
    const receive = (b: Batch) => { catalog.applyBatch(b); marks.observe(b); };
    const row = (key = "run:a") => catalog.catalog.rows[`\n${key}`];
    return { ...actions, marks, receive, row, snap: (key = "run:a") => snapshotOf(row(key)) };
  }
  const pending = <T,>() => {
    let resolve: (v: T) => void = () => {};
    let reject: (e: unknown) => void = () => {};
    const promise = new Promise<T>((a, b) => { resolve = a; reject = b; });
    return { promise, resolve, reject };
  };

  beforeEach(() => mocked.setNotifications.mockReset());

  it("asks the host to flip the value at the listed revision, then shows the confirmed value", async () => {
    await createRoot(async (dispose) => {
      const a = await setupMute();
      mocked.setNotifications.mockResolvedValue(prefs(true, 4));
      await a.toggleMute(a.snap());
      expect(mocked.setNotifications).toHaveBeenCalledWith({ computer: "", name: "claude/sample-project/a", run_id: "run-1", conversation_id: "conv-1" }, true, 3);
      expect(a.row().muted).toBe(true);
      expect(a.notices()).toEqual([]);
      // An older listing does not roll it back; a newer one replaces it.
      a.receive(batchOf({ upserts: [shared({ notification_revision: 3, muted: false })] }));
      expect(a.row()).toMatchObject({ muted: true, notificationConflict: false });
      expect(a.marks.overrideCount()).toBe(1);
      // The row reaching the confirmed revision and value ends the override; the core keeps it from there.
      a.receive(batchOf({ upserts: [shared({ notification_revision: 4, muted: true })] }));
      expect(a.row().muted).toBe(true);
      expect(a.marks.overrideCount()).toBe(0);
      a.receive(batchOf({ upserts: [shared({ notification_revision: 5, muted: false })] }));
      expect(a.row().muted).toBe(false);
      dispose();
    });
  });

  it("ends the override when a higher revision lists, and when its conversation leaves the listing", async () => {
    await createRoot(async (dispose) => {
      const a = await setupMute([shared(), shared({ conversation_id: "conv-2" }, "run:b")]);
      mocked.setNotifications.mockResolvedValue(prefs(true, 4));
      await a.toggleMute(a.snap());
      expect(a.marks.overrideCount()).toBe(1);
      a.receive(batchOf({ removes: [{ computer: "", key: "run:b" }] }));
      expect(a.marks.overrideCount()).toBe(1);
      a.receive(batchOf({ removes: [{ computer: "", key: "run:a" }] }));
      expect(a.marks.overrideCount()).toBe(0);
      dispose();
    });
  });

  it("never hides the core's conflict behind a newer override", async () => {
    await createRoot(async (dispose) => {
      const a = await setupMute();
      mocked.setNotifications.mockResolvedValue(prefs(true, 4));
      await a.toggleMute(a.snap());
      a.receive(batchOf({ upserts: [shared({ notification_revision: 3, muted: false, notification_error: "Conflicting shared notification preferences. Refresh the session." })] }));
      expect(a.row()).toMatchObject({ muted: true, notificationConflict: true });
      dispose();
    });
  });

  it("unmutes a muted row", async () => {
    await createRoot(async (dispose) => {
      const a = await setupMute([shared({ muted: true })]);
      mocked.setNotifications.mockResolvedValue(prefs(false, 4));
      await a.toggleMute(a.snap());
      expect(mocked.setNotifications.mock.calls[0].slice(1)).toEqual([false, 3]);
      expect(a.row().muted).toBe(false);
      dispose();
    });
  });

  it("keeps the override on the same revision with another value and marks a conflict", async () => {
    await createRoot(async (dispose) => {
      const a = await setupMute();
      mocked.setNotifications.mockResolvedValue(prefs(true, 4));
      await a.toggleMute(a.snap());
      a.receive(batchOf({ upserts: [shared({ notification_revision: 4, muted: false })] }));
      expect(a.row()).toMatchObject({ muted: true, notificationConflict: true });
      // The conflict holds until a listing agrees: the override stays.
      expect(a.marks.overrideCount()).toBe(1);
      a.receive(batchOf({ upserts: [shared({ notification_revision: 4, muted: false })] }));
      expect(a.row()).toMatchObject({ muted: true, notificationConflict: true });
      expect(notificationsState(a.snap(), true, a.pendingNotifications("", "conv-1"))).toMatchObject({ enabled: false, hint: "Conflicting shared notification preferences. Refresh the session." });
      dispose();
    });
  });

  it("refuses a reply older than the row, or equal with another value, with a sticky notice", async () => {
    await createRoot(async (dispose) => {
      const a = await setupMute();
      const first = pending<Record<string, unknown>>();
      mocked.setNotifications.mockReturnValueOnce(first.promise);
      const call = a.toggleMute(a.snap());
      a.receive(batchOf({ upserts: [shared({ notification_revision: 5, muted: false })] }));
      first.resolve(prefs(true, 4));
      await call;
      expect(a.row().muted).toBe(false);
      expect(a.notices()).toMatchObject([{ tone: "error", sticky: true, text: "The shared notification setting changed while this request was pending. Refresh the session." }]);

      const second = pending<Record<string, unknown>>();
      mocked.setNotifications.mockReturnValueOnce(second.promise);
      const again = a.toggleMute(a.snap());
      second.resolve(prefs(true, 5));
      await again;
      expect(a.notices()).toHaveLength(2);
      expect(a.row().muted).toBe(false);
      dispose();
    });
  });

  it("shows a failure as a sticky error and keeps the row", async () => {
    await createRoot(async (dispose) => {
      const a = await setupMute();
      const failing = pending<Record<string, unknown>>();
      mocked.setNotifications.mockReturnValueOnce(failing.promise);
      const call = a.toggleMute(a.snap());
      failing.reject("Update the configured local hgs to support shared notification preferences.");
      await call;
      expect(a.notices()).toMatchObject([{ tone: "error", sticky: true, text: "Update the configured local hgs to support shared notification preferences." }]);
      expect(a.row().muted).toBe(false);
      expect(a.pendingNotifications("", "conv-1")).toEqual({ conversation: false, total: 0 });
      dispose();
    });
  });

  it("drops a late answer for a row that is no longer the same run, conversation or created", async () => {
    for (const next of [{ run_id: "run-2" }, { conversation_id: "conv-2" }, { created: 1_790_000_001 }, { state: "archived" }]) {
      await createRoot(async (dispose) => {
        const a = await setupMute();
        const slow = pending<Record<string, unknown>>();
        mocked.setNotifications.mockReturnValueOnce(slow.promise);
        const call = a.toggleMute(a.snap());
        a.receive(batchOf({ upserts: [shared({ ...next, muted: false })] }));
        slow.resolve(prefs(true, 4));
        await call;
        expect(a.row().muted).toBe(false);
        expect(a.notices()).toEqual([]);
        dispose();
      });
    }
    await createRoot(async (dispose) => {
      const a = await setupMute();
      const slow = pending<Record<string, unknown>>();
      mocked.setNotifications.mockReturnValueOnce(slow.promise);
      const call = a.toggleMute(a.snap());
      a.receive(batchOf({ upserts: [shared({ run_id: "run-2" })] }));
      slow.reject("boom");
      await call;
      expect(a.notices()).toEqual([]);
      dispose();
    });
  });

  it("raises the gone notice and calls nothing for a changed conversation, a changed created or a gone row", async () => {
    const gone = "This session is no longer available. Open its actions again.";
    await createRoot(async (dispose) => {
      for (const change of [{ conversation_id: "conv-2" }, { created: 1_790_000_001 }, "gone"] as const) {
        const a = await setupMute();
        const shown = a.snap();
        if (change === "gone") a.receive(batchOf({ removes: [{ computer: "", key: "run:a" }] }));
        else a.receive(batchOf({ upserts: [shared(change)] }));
        await a.toggleMute(shown);
        expect(a.notices()).toEqual([expect.objectContaining({ text: gone, sticky: true })]);
      }
      expect(mocked.setNotifications).not.toHaveBeenCalled();
      dispose();
    });
  });

  it("sends the confirmed override's revision when it is higher than the row's", async () => {
    await createRoot(async (dispose) => {
      const a = await setupMute();
      mocked.setNotifications.mockResolvedValueOnce(prefs(true, 4)).mockResolvedValueOnce(prefs(false, 5));
      await a.toggleMute(a.snap());
      expect(a.row()).toMatchObject({ muted: true, notificationRevision: 3 });
      await a.toggleMute(a.snap());
      expect(mocked.setNotifications.mock.calls[1].slice(1)).toEqual([false, 4]);
      dispose();
    });
  });

  it("shows the pending reason when the host lane answers busy", async () => {
    await createRoot(async (dispose) => {
      const a = await setupMute();
      const lane = pending<Record<string, unknown>>();
      mocked.setNotifications.mockReturnValueOnce(lane.promise);
      const call = a.toggleMute(a.snap());
      lane.reject("busy");
      await call;
      expect(a.notices()).toEqual([expect.objectContaining({ text: "A shared notification change is pending. Wait for its result." })]);
      dispose();
    });
  });

  it("does nothing when the row changed since the menu opened", async () => {
    await createRoot(async (dispose) => {
      for (const next of [{ run_id: "run-2" }, { notification_revision: 4 }, { muted: true }, { shared_notifications: false }]) {
        const a = await setupMute();
        const shown = a.snap();
        a.receive(batchOf({ upserts: [shared(next)] }));
        await a.toggleMute(shown);
      }
      const gone = await setupMute();
      const shown = gone.snap();
      gone.receive(batchOf({ removes: [{ computer: "", key: "run:a" }] }));
      await gone.toggleMute(shown);
      const offline = await setupMute([shared()], () => false);
      await offline.toggleMute(offline.snap());
      expect(mocked.setNotifications).not.toHaveBeenCalled();
      expect(offline.notices()).toEqual([]);
      dispose();
    });
  });

  it("allows one pending change per conversation and sixteen in all", async () => {
    await createRoot(async (dispose) => {
      const rows = Array.from({ length: 18 }, (_, i) => shared({ name: `claude/sample-project/s${i}`, conversation_id: `conv-${i}` }, `run:${i}`));
      const a = await setupMute(rows);
      const hold = pending<Record<string, unknown>>();
      mocked.setNotifications.mockImplementation(() => hold.promise);
      void a.toggleMute(a.snap("run:0"));
      void a.toggleMute(a.snap("run:0"));
      expect(mocked.setNotifications).toHaveBeenCalledTimes(1);
      expect(a.pendingNotifications("", "conv-0")).toEqual({ conversation: true, total: 1 });
      expect(notificationsState(a.snap("run:0"), true, a.pendingNotifications("", "conv-0"))).toMatchObject({ enabled: false, hint: "A shared notification change is pending. Wait for its result." });
      for (let i = 1; i < 18; i++) void a.toggleMute(a.snap(`run:${i}`));
      expect(mocked.setNotifications).toHaveBeenCalledTimes(16);
      expect(a.pendingNotifications("", "conv-17").total).toBe(16);
      hold.resolve(prefs(true, 4, "other"));
      await flush();
      expect(a.pendingNotifications("", "conv-17").total).toBe(0);
      dispose();
    });
  });
});

describe("forget", () => {
  const opened = (extra: Partial<{ binding: boolean; run: string; conversation: string }> = {}) => ({ binding: false, run: "run-1", conversation: "conv-1", ...extra });
  const oc = { agent: "opencode", state: "running", allowed: "forget rename" } as Partial<ViewRow>;

  it("forgets a saved session with kill and the write notice", async () => {
    await createRoot(async (dispose) => {
      const a = setup(rowOut({ state: "paused" }));
      mocked.runAction.mockResolvedValue(done());
      const out = await a.forget(session({ state: "paused" }), opened());
      expect(out.outcome).toBe("done");
      expect(mocked.runAction.mock.calls[0][1]).toEqual({ kind: "forget" });
      expect(a.notices()).toMatchObject([{ tone: "info", text: "Session updated. Refreshing…" }]);
      dispose();
    });
  });

  it("shows a failed forget as a sticky detail", async () => {
    await createRoot(async (dispose) => {
      const a = setup(rowOut({ state: "paused" }));
      mocked.runAction.mockResolvedValue({ outcome: "failed", detail: "no way" });
      await a.forget(session({ state: "paused" }), opened());
      expect(a.notices()).toMatchObject([{ tone: "error", text: "no way", sticky: true }]);
      dispose();
    });
  });

  it("forgets an OpenCode binding with its run and conversation", async () => {
    await createRoot(async (dispose) => {
      const a = setup(rowOut(oc));
      mocked.runAction.mockResolvedValue(done());
      await a.forget(session(oc), opened({ binding: true }));
      expect(mocked.runAction.mock.calls[0][0]).toMatchObject({ run_id: "run-1", conversation_id: "conv-1" });
      expect(mocked.runAction.mock.calls[0][1]).toEqual({ kind: "forget_binding" });
      expect(a.notices()).toMatchObject([{ tone: "info", text: "Zerus forgot claude/sample-project/a. The conversation stays in OpenCode." }]);
      dispose();
    });
  });

  it("shows a failed binding forget as a sticky error", async () => {
    await createRoot(async (dispose) => {
      const a = setup(rowOut(oc));
      mocked.runAction.mockResolvedValue({ outcome: "failed", detail: "stale" });
      await a.forget(session(oc), opened({ binding: true }));
      expect(a.notices()).toMatchObject([{ tone: "error", text: "stale", sticky: true }]);
      dispose();
    });
  });

  it("uses plain forget for a binding without a run id", async () => {
    await createRoot(async (dispose) => {
      const bare = { ...oc, run_id: undefined } as Partial<ViewRow>;
      const a = setup(rowOut(bare));
      mocked.runAction.mockResolvedValue(done());
      await a.forget(session(bare), opened({ binding: true, run: "" }));
      expect(mocked.runAction.mock.calls[0][1]).toEqual({ kind: "forget" });
      dispose();
    });
  });

  it("refuses with the sticky state-changed notice when the binding kind changed", async () => {
    await createRoot(async (dispose) => {
      const a = setup(rowOut({ state: "paused" }));
      const dialog = session({ state: "paused" });
      a.applyBatch(batchOf({ upserts: [rowOut({ ...oc, state: "paused" })] }));
      await a.forget(dialog, opened());
      expect(mocked.runAction).not.toHaveBeenCalled();
      expect(a.notices()).toMatchObject([{ tone: "error", text: "The session state changed. Open its actions again.", sticky: true }]);
      dispose();
    });
  });

  it("refuses when a saved session started running", async () => {
    await createRoot(async (dispose) => {
      const a = setup(rowOut({ state: "paused" }));
      const dialog = session({ state: "paused" });
      a.applyBatch(batchOf({ upserts: [rowOut({ state: "running" })] }));
      await a.forget(dialog, opened());
      expect(mocked.runAction).not.toHaveBeenCalled();
      expect(a.notices()).toMatchObject([{ text: "The session state changed. Open its actions again.", sticky: true }]);
      dispose();
    });
  });

  it("refuses a scoped forget when the run or conversation changed", async () => {
    await createRoot(async (dispose) => {
      const a = setup(rowOut(oc));
      const dialog = session(oc);
      a.applyBatch(batchOf({ upserts: [rowOut({ ...oc, run_id: "run-2" })] }));
      await a.forget(dialog, opened({ binding: true }));
      expect(mocked.runAction).not.toHaveBeenCalled();
      expect(a.notices()).toMatchObject([{ text: "The session state changed. Open its actions again.", sticky: true }]);
      dispose();
    });
  });

  it("makes no call and no notice offline", async () => {
    await createRoot(async (dispose) => {
      const off = setup(rowOut({ state: "paused" }), () => false);
      await off.forget(session({ state: "paused" }), opened());
      expect(mocked.runAction).not.toHaveBeenCalled();
      expect(off.notices()).toEqual([]);
      dispose();
    });
  });

  it("raises the sticky state-changed notice when the row vanished", async () => {
    await createRoot(async (dispose) => {
      const gone = setup(rowOut({ state: "paused" }));
      gone.applyBatch(batchOf({ removes: [{ computer: "", key: "run:a" }] }));
      await gone.forget(session({ state: "paused" }), opened());
      expect(mocked.runAction).not.toHaveBeenCalled();
      expect(gone.notices()).toMatchObject([{ tone: "error", text: "The session state changed. Open its actions again.", sticky: true }]);
      dispose();
    });
  });
});

describe("archived rows (the Archive view)", () => {
  const ARCHIVE_ID = "10000000-0000-4000-8000-000000000001";
  const archived = () => toSession("", `archive:${ARCHIVE_ID}`, view({ state: "archived", status: "ended", archive_id: ARCHIVE_ID, archived_at: 1_789_000_000 }));

  it("an archived selection inspects with its archive id (inspect --archive ID)", async () => {
    await createRoot(async (dispose) => {
      const a = setup();
      const row = archived();
      expect(a.detailsFor(row, true, { value: undefined, settled: true })).toBeUndefined();
      await flush();
      expect(mocked.inspectSession).toHaveBeenCalledTimes(1);
      expect(mocked.inspectSession).toHaveBeenCalledWith({ computer: "", name: "claude/sample-project/a", run_id: "run-1", conversation_id: "conv-1" }, ARCHIVE_ID);
      expect(a.detailsFor(row, true, { value: undefined, settled: true })?.run_id).toBe("run-1");
      dispose();
    });
  });

  it("the click-time recheck finds a loaded archived row, so Copy restore command passes its archive id", async () => {
    await createRoot(async (dispose) => {
      const catalog = createCatalog();
      const row = archived();
      mocked.copyCommand.mockResolvedValue("hgs resume claude/sample-project/a --archive id");
      const plain = createActions(catalog.catalog);
      expect(await plain.copyCommand(snapshotOf(row))).toEqual({ outcome: "failed", detail: "This session is no longer available. Open its actions again." });
      const a = createActions(withArchived(catalog.catalog, (key) => (key === row.key ? row : undefined)));
      expect(await a.copyCommand(snapshotOf(row))).toMatchObject({ outcome: "done" });
      expect(mocked.copyCommand).toHaveBeenCalledWith("", "claude/sample-project/a", ARCHIVE_ID);
      expect(a.stillListed(snapshotOf(row))?.archiveId).toBe(ARCHIVE_ID);
      dispose();
    });
  });

  it("an archived rename runs with its archive id while the archived row is loaded", async () => {
    await createRoot(async (dispose) => {
      const catalog = createCatalog();
      const row = archived();
      mocked.runAction.mockResolvedValue(done());
      const a = createActions(withArchived(catalog.catalog, (key) => (key === row.key ? row : undefined)));
      const rename = { kind: "rename", new_name: "claude/sample-project/b", archive_id: ARCHIVE_ID } as const;
      expect(await a.perform(snapshotOf(row), rename)).toMatchObject({ outcome: "done" });
      expect(mocked.runAction).toHaveBeenCalledWith({ computer: "", name: "claude/sample-project/a", run_id: "run-1", conversation_id: "conv-1" }, rename);
      dispose();
    });
  });
});


describe("explicit cleanup confirmation", () => {
  it("holds the existing pending lane and never falls back to legacy terminate", async () => {
    await createRoot(async (dispose) => {
      const a = setup();
      let settle!: (out: Outcome) => void;
      mocked.terminationCleanupConfirm.mockReturnValue(new Promise((resolve) => { settle = resolve; }));
      const pending = a.end(session(), "terminate", "opaque-rust-offer");
      expect(a.updating("", "claude/sample-project/a")).toBe(true);
      await a.end(session(), "terminate", "opaque-rust-offer");
      expect(mocked.terminationCleanupConfirm).toHaveBeenCalledTimes(1);
      expect(mocked.terminationCleanupConfirm).toHaveBeenCalledWith("opaque-rust-offer");
      settle({ outcome: "failed", detail: "Unknown. Check the session before retrying." });
      expect((await pending).outcome).toBe("failed");
      expect(mocked.runAction).not.toHaveBeenCalled();
      expect(a.updating("", "claude/sample-project/a")).toBe(false);
      dispose();
    });
  });
  it("refuses a changed run before submitting a retained cleanup offer", async () => {
    await createRoot(async (dispose) => {
      const a = setup();
      const opening = session();
      a.applyBatch(batchOf({ upserts: [rowOut({ run_id: "replacement" })] }));
      expect((await a.end(opening, "terminate", "old-offer")).outcome).toBe("failed");
      expect(mocked.terminationCleanupConfirm).not.toHaveBeenCalled();
      expect(mocked.runAction).not.toHaveBeenCalled();
      dispose();
    });
  });
});
