import { describe, expect, it } from "vitest";
import { toSession } from "../../store/catalog";
import {
  MOVE_LATER, SELECTION_ACTIONS, changedText, confirmationCopy, forgetsBinding, markSummary, progressText, restoreLimitText, sameInstance,
  selectionApplies, selectionCall, selectionOperation, selectionSnapshot, selectionText, summaryText, type ApplyContext, type SelectionAction,
} from "./selection";
import type { ViewRow } from "../../types";

const row = (extra: Partial<ViewRow> = {}, key = "run:a", computer = "") => toSession(computer, key, {
  name: "claude/proj/a", agent: "claude", state: "running", resumable: true, activity: "idle", process_state: "running", run_id: "r1",
  conversation_id: "c1", machine: "box", created: 100, tracked: true, ...extra,
});
const ctx = (extra: Partial<ApplyContext> = {}): ApplyContext => ({ online: true, updating: false, compact: false, attention: false, occupied: false, ...extra });
const binding = (extra: Partial<ViewRow> = {}) => row({ agent: "opencode", allowed_actions: ["forget", "rename"], ...extra });
const WRITES: SelectionAction[] = ["pause", "resume", "archive", "forget", "terminate"];

describe("selection texts", () => {
  it("are Qt's selectionActionText in the bar's order", () => {
    expect(SELECTION_ACTIONS).toEqual(["compact", "expand", "mark_read", "review_later", "pause", "resume", "archive", "move", "forget", "terminate"]);
    expect(SELECTION_ACTIONS.map(selectionText)).toEqual([
      "Compact session rows", "Expand session rows", "Mark as read", "Mark needs attention", "Pause", "Resume / restore", "Move to archive",
      "Move to project", "Forget", "Terminate",
    ]);
    expect(MOVE_LATER).toBe("Arrives with project organisation.");
  });
  it("words the queue's progress, refusals and summaries", () => {
    expect(progressText("pause", 2, 5)).toBe("Pause: 2 / 5…");
    expect(changedText(row())).toBe("box / claude/proj/a: session changed or is unavailable");
    expect(restoreLimitText(row())).toBe("claude/proj/a: earlier restores still need a refresh");
    expect(summaryText("archive", 3, 3, 0, [])).toEqual({ text: "Move to archive: 3 of 3 completed.", failed: false });
    expect(summaryText("terminate", 1, 5, 4, ["a: x", "b: y", "c: z", "d: w"])).toEqual({
      text: "Terminate: 1 of 5 completed. 4 failed or changed; refresh before retrying.\na: x\nb: y\nc: z", failed: true,
    });
    expect(markSummary("mark_read", 2, 2)).toEqual({ text: "Mark as read: 2 of 2 sessions updated.", differs: false });
    expect(markSummary("review_later", 1, 3)).toEqual({ text: "Mark needs attention: 1 of 3 sessions updated.", differs: true });
  });
});

describe("sameInstance", () => {
  it("needs the same name, archive id, created, run, conversation and state", () => {
    const a = row();
    expect(sameInstance(a, row())).toBe(true);
    expect(sameInstance(a, row({ name: "claude/proj/b" }))).toBe(false);
    expect(sameInstance(a, row({ archive_id: "x" }))).toBe(false);
    expect(sameInstance(a, row({ created: 101 }))).toBe(false);
    expect(sameInstance(a, row({ run_id: "r2" }))).toBe(false);
    expect(sameInstance(a, row({ conversation_id: "c2" }))).toBe(false);
    expect(sameInstance(a, row({ state: "paused" }))).toBe(false);
  });
  it("ignores what the queue does not compare", () => {
    expect(sameInstance(row(), row({ activity: "busy", status: "working", unread: true, summary: "new" }))).toBe(true);
  });
  it("treats a missing and an empty identity as the same", () => {
    expect(sameInstance(row({ run_id: undefined }), row({ run_id: "" }))).toBe(true);
  });
});

describe("selectionApplies", () => {
  it("compacts an expanded row and expands a compact one, whatever its state", () => {
    for (const extra of [{}, { state: "archived", archive_id: "a1" }] as Partial<ViewRow>[]) {
      expect(selectionApplies(row(extra), "compact", ctx({ online: false, updating: true }))).toBe(true);
      expect(selectionApplies(row(extra), "compact", ctx({ compact: true }))).toBe(false);
      expect(selectionApplies(row(extra), "expand", ctx({ compact: true, online: false }))).toBe(true);
      expect(selectionApplies(row(extra), "expand", ctx())).toBe(false);
    }
  });
  it("always applies Move", () => {
    expect(selectionApplies(row({ state: "archived" }), "move", ctx({ online: false, updating: true }))).toBe(true);
  });
  it("marks read a live row that needs attention, and review later one that does not", () => {
    expect(selectionApplies(row(), "mark_read", ctx({ attention: true, online: false, updating: true }))).toBe(true);
    expect(selectionApplies(row(), "mark_read", ctx())).toBe(false);
    expect(selectionApplies(row({ state: "archived" }), "mark_read", ctx({ attention: true }))).toBe(false);
    expect(selectionApplies(row(), "review_later", ctx({ online: false }))).toBe(true);
    expect(selectionApplies(row(), "review_later", ctx({ attention: true }))).toBe(false);
    expect(selectionApplies(row({ state: "archived" }), "review_later", ctx())).toBe(false);
  });
  it("needs the computer online and no write pending for every write", () => {
    const rows: Record<string, ReturnType<typeof row>> = {
      pause: row(), resume: row({ state: "paused" }), archive: row({ state: "paused" }), forget: row({ state: "paused" }), terminate: row(),
    };
    for (const a of WRITES) {
      expect(selectionApplies(rows[a], a, ctx())).toBe(true);
      expect(selectionApplies(rows[a], a, ctx({ online: false }))).toBe(false);
      expect(selectionApplies(rows[a], a, ctx({ updating: true }))).toBe(false);
    }
  });
  it("needs the operation allowed, by its reported actions or by the agent", () => {
    expect(selectionApplies(row({ allowed_actions: ["resume"] }), "pause", ctx())).toBe(false);
    expect(selectionApplies(row({ allowed_actions: ["pause"] }), "pause", ctx())).toBe(true);
    expect(selectionApplies(row({ state: "paused", allowed_actions: ["pause"] }), "resume", ctx())).toBe(false);
    expect(selectionApplies(row({ state: "paused", allowed_actions: ["forget"] }), "archive", ctx())).toBe(false);
    expect(selectionApplies(row({ state: "paused", allowed_actions: ["archive"] }), "forget", ctx())).toBe(false);
    expect(selectionApplies(row({ allowed_actions: ["pause"] }), "terminate", ctx())).toBe(false);
    // An OpenCode session without reported actions only renames and forgets.
    expect(selectionApplies(row({ agent: "opencode" }), "pause", ctx())).toBe(false);
    expect(selectionApplies(row({ agent: "opencode", state: "paused" }), "resume", ctx())).toBe(false);
    expect(selectionApplies(row({ agent: "opencode" }), "terminate", ctx())).toBe(false);
  });
  it("restores an archived row by the restore operation", () => {
    const archived = { state: "archived", archive_id: "a1", resumable: true };
    expect(selectionOperation(row(archived), "resume")).toBe("restore");
    expect(selectionOperation(row({ state: "paused" }), "resume")).toBe("resume");
    expect(selectionApplies(row({ ...archived, allowed_actions: ["restore"] }), "resume", ctx())).toBe(true);
    expect(selectionApplies(row({ ...archived, allowed_actions: ["resume"] }), "resume", ctx())).toBe(false);
  });
  it("pauses a running, resumable, idle row whose process has not exited", () => {
    expect(selectionApplies(row(), "pause", ctx())).toBe(true);
    expect(selectionApplies(row({ state: "paused" }), "pause", ctx())).toBe(false);
    expect(selectionApplies(row({ resumable: false }), "pause", ctx())).toBe(false);
    expect(selectionApplies(row({ activity: "busy" }), "pause", ctx())).toBe(false);
    expect(selectionApplies(row({ process_state: "exited" }), "pause", ctx())).toBe(false);
  });
  it("resumes a resumable row that is not running; an archived one needs its id and a free name", () => {
    expect(selectionApplies(row({ state: "paused" }), "resume", ctx())).toBe(true);
    expect(selectionApplies(row({ state: "stopped" }), "resume", ctx())).toBe(true);
    expect(selectionApplies(row(), "resume", ctx())).toBe(false);
    expect(selectionApplies(row({ state: "paused", resumable: false }), "resume", ctx())).toBe(false);
    expect(selectionApplies(row({ state: "archived", archive_id: "a1" }), "resume", ctx())).toBe(true);
    expect(selectionApplies(row({ state: "archived" }), "resume", ctx())).toBe(false);
    expect(selectionApplies(row({ state: "archived", archive_id: "a1" }), "resume", ctx({ occupied: true }))).toBe(false);
    expect(selectionApplies(row({ state: "archived", archive_id: "a1", resumable: false }), "resume", ctx())).toBe(false);
  });
  it("archives a paused or stopped row", () => {
    expect(selectionApplies(row({ state: "paused" }), "archive", ctx())).toBe(true);
    expect(selectionApplies(row({ state: "stopped" }), "archive", ctx())).toBe(true);
    expect(selectionApplies(row(), "archive", ctx())).toBe(false);
    expect(selectionApplies(row({ state: "archived", archive_id: "a1" }), "archive", ctx())).toBe(false);
  });
  it("forgets a row that is not running; an archived one needs its id", () => {
    expect(selectionApplies(row({ state: "paused" }), "forget", ctx())).toBe(true);
    expect(selectionApplies(row({ state: "stopped" }), "forget", ctx())).toBe(true);
    expect(selectionApplies(row(), "forget", ctx())).toBe(false);
    expect(selectionApplies(row({ state: "archived", archive_id: "a1" }), "forget", ctx())).toBe(true);
    expect(selectionApplies(row({ state: "archived" }), "forget", ctx())).toBe(false);
  });
  it("forgets an OpenCode binding, even running, scoped to its run and conversation", () => {
    expect(forgetsBinding(binding())).toBe(true);
    expect(forgetsBinding(binding({ state: "archived" }))).toBe(false);
    expect(forgetsBinding(row({ state: "paused" }))).toBe(false);
    expect(forgetsBinding(binding({ allowed_actions: ["forget", "terminate"] }))).toBe(false);
    expect(selectionApplies(binding(), "forget", ctx())).toBe(true);
    expect(selectionApplies(binding({ state: "paused" }), "forget", ctx())).toBe(true);
    expect(selectionApplies(binding({ run_id: "" }), "forget", ctx())).toBe(false);
    expect(selectionApplies(binding({ conversation_id: "" }), "forget", ctx())).toBe(false);
    // Without reported actions an OpenCode session forgets its binding too.
    expect(selectionApplies(row({ agent: "opencode" }), "forget", ctx())).toBe(true);
  });
  it("terminates a running row", () => {
    expect(selectionApplies(row(), "terminate", ctx())).toBe(true);
    expect(selectionApplies(row({ activity: "busy" }), "terminate", ctx())).toBe(true);
    expect(selectionApplies(row({ state: "paused" }), "terminate", ctx())).toBe(false);
  });
});

describe("selectionCall", () => {
  it("pauses, resumes and archives by name", () => {
    expect(selectionCall(row(), "pause")).toEqual({ operation: "pause claude/proj/a", call: { kind: "pause" } });
    expect(selectionCall(row({ state: "paused" }), "resume")).toEqual({ operation: "resume claude/proj/a", call: { kind: "resume" } });
    expect(selectionCall(row({ state: "paused" }), "archive")).toEqual({ operation: "archive claude/proj/a", call: { kind: "archive" } });
  });
  it("restores and forgets an archived row through the archive calls", () => {
    expect(selectionCall(row({ state: "archived", archive_id: "a1" }), "resume")).toEqual({ operation: "restore claude/proj/a", archive: "restore" });
    expect(selectionCall(row({ state: "archived", archive_id: "a1" }), "forget")).toEqual({ operation: "forget archive claude/proj/a", archive: "forget" });
  });
  it("forgets a binding by its run and conversation, a saved session by kill", () => {
    expect(selectionCall(binding(), "forget")).toEqual({ operation: "forget claude/proj/a", call: { kind: "forget_binding" } });
    expect(selectionCall(row({ state: "paused" }), "forget")).toEqual({ operation: "kill claude/proj/a", call: { kind: "forget" } });
  });
  it("terminates a tracked Codex, Claude or Kimi session into Archive and kills any other", () => {
    for (const agent of ["codex", "claude", "kimi"]) {
      expect(selectionCall(row({ agent }), "terminate")).toEqual({ operation: "terminate claude/proj/a", call: { kind: "terminate" } });
    }
    expect(selectionCall(row({ tracked: false }), "terminate")).toEqual({ operation: "kill claude/proj/a", call: { kind: "kill" } });
    expect(selectionCall(row({ agent: "deepseek" }), "terminate")).toEqual({ operation: "kill claude/proj/a", call: { kind: "kill" } });
  });
});

describe("confirmationCopy", () => {
  const two = [row({ machine: "box" }, "run:a"), row({ name: "claude/proj/b", machine: "mac" }, "run:b")];
  it("asks to terminate the targets", () => {
    expect(confirmationCopy("terminate", two, 2)).toEqual({
      title: "Terminate",
      text: "Terminate 2 selected sessions?\n\nRunning processes will stop. Tracked Codex, Claude and Kimi conversations are kept in Archive; native conversation history is kept.",
      details: ["box / claude/proj/a", "mac / claude/proj/b"],
      confirm: "Terminate 2 sessions",
    });
  });
  it("asks to forget saved entries without bindings", () => {
    expect(confirmationCopy("forget", two, 2)).toMatchObject({
      title: "Forget",
      text: "Forget 2 selected sessions?\n\nOnly these saved or archived entries are removed. Live sessions and native conversation history are kept.",
      confirm: "Forget 2 sessions",
    });
  });
  it("explains bindings among the targets", () => {
    expect(confirmationCopy("forget", [two[0], binding()], 2).text).toBe(
      "Forget 2 selected sessions?\n\nThese saved or archived entries are removed. Other live sessions and native conversation history are kept."
      + "\n\nOpenCode sessions: only Zerus forgets them; their conversations stay in OpenCode.");
    expect(confirmationCopy("forget", [binding()], 1).text).toBe(
      "Forget 1 selected sessions?\n\nOpenCode sessions: only Zerus forgets them; their conversations stay in OpenCode.");
  });
  it("counts the selected rows it leaves as they are", () => {
    expect(confirmationCopy("terminate", two, 5).text.endsWith("\n\n3 unavailable or inapplicable sessions will be left as they are.")).toBe(true);
    expect(confirmationCopy("forget", two, 3).text.endsWith("\n\n1 unavailable or inapplicable sessions will be left as they are.")).toBe(true);
  });
  it("words Clear archive, loaded or not", () => {
    const archived = [row({ state: "archived", archive_id: "a1" })];
    expect(confirmationCopy("forget", archived, 1, { paged: false })).toEqual({
      title: "Clear archive",
      text: "Remove 1 archived sessions from HGS on the selected machines?\n\nOnly these saved or archived entries are removed. Live sessions and native conversation history are kept.",
      details: ["box / claude/proj/a"],
      confirm: "Clear archive",
    });
    expect(confirmationCopy("forget", archived, 2, { paged: true }).text).toBe(
      "Remove 1 loaded archived sessions in the current date and machine scope? Unloaded entries are kept."
      + "\n\nOnly these saved or archived entries are removed. Live sessions and native conversation history are kept."
      + "\n\n1 unavailable or inapplicable sessions will be left as they are.");
  });
});

describe("selectionSnapshot", () => {
  it("keeps the action fields and what attention reads, as plain values", () => {
    const s = row({ reply_id: "q" });
    const snap = selectionSnapshot(s);
    expect(snap).toMatchObject({ key: s.key, runId: "r1", replyId: "q", status: s.status, reviewLater: false, unread: false });
    expect(Object.keys(snap)).not.toContain("title");
  });
});


it("counts unknown batch outcomes separately with the same bounded Qt sentence", () => {
  expect(summaryText("archive", 1, 3, 1, ["known", "unknown"], 1)).toEqual({ text: "Move to archive: 1 of 3 completed. 1 failed or changed, 1 unknown; refresh before retrying.\nknown\nunknown", failed: true });
  expect(summaryText("archive", 0, 2, 0, ["a", "b", "c", "omitted"], 2)).toEqual({ text: "Move to archive: 0 of 2 completed. 2 unknown; refresh before retrying.\na\nb\nc", failed: true });
});
