import { describe, expect, it, vi } from "vitest";
import { CHECK_MS, DWELL_MS, NEAR_END, createViewedReply, replyVisible, viewedCandidate, type ReplyView, type ViewedFacts, type ViewedRow } from "./viewedReply";
import type { ActivityEvent } from "../types";

// Qt's SessionsWindow::checkViewedReply (SessionsWindow.cpp:3404-3432) and
// ActivityView::replyVisible (ActivityView.cpp:1549-1574), without a window.

const REPLY = "42:1790000000.5";
const row = (extra: Partial<ViewedRow> = {}): ViewedRow => ({
  key: "\nrun:a", computer: "", name: "claude/sample-project/a", state: "running", conversationId: "conv-1", replyId: REPLY, unreadReply: true, ...extra,
});
const facts = (extra: Partial<ViewedFacts> = {}): ViewedFacts => ({
  subagent: false, row: row(), online: true, inspected: { conversation_id: "conv-1", reply_id: REPLY }, inspectError: false, tab: "activity",
  active: () => true, activityShows: () => true, terminalShowsLatest: () => false, ...extra,
});
const CANDIDATE = `\nrun:a\nconv-1\n${REPLY}`;

describe("viewedCandidate", () => {
  it("is the selected row's reply shown in Activity of an active window", () => {
    expect(viewedCandidate(facts())).toBe(CANDIDATE);
  });

  it("asks Activity for exactly the row's reply, and Terminal only on its tab", () => {
    const activityShows = vi.fn(() => true), terminalShowsLatest = vi.fn(() => true);
    viewedCandidate(facts({ activityShows, terminalShowsLatest }));
    expect(activityShows).toHaveBeenCalledWith(REPLY);
    expect(terminalShowsLatest).not.toHaveBeenCalled();
    activityShows.mockClear();
    expect(viewedCandidate(facts({ tab: "terminal", activityShows, terminalShowsLatest }))).toBe(CANDIDATE);
    expect(activityShows).not.toHaveBeenCalled();
    expect(viewedCandidate(facts({ tab: "terminal", terminalShowsLatest: () => false }))).toBeUndefined();
  });

  it("needs the reply in view: Activity's answer, the Terminal's bottom, and never another tab", () => {
    expect(viewedCandidate(facts({ activityShows: () => false }))).toBeUndefined();
    for (const tab of ["native", "processes", "taskview"]) {
      expect(viewedCandidate(facts({ tab, terminalShowsLatest: () => true }))).toBeUndefined();
    }
  });

  it("refuses an inactive window: hidden, minimised, unfocused, another page or a modal or popup open", () => {
    expect(viewedCandidate(facts({ active: () => false }))).toBeUndefined();
  });

  it("refuses a subagent's Activity", () => {
    expect(viewedCandidate(facts({ subagent: true }))).toBeUndefined();
  });

  it("refuses no selection, a read reply, an archived row and an offline computer", () => {
    expect(viewedCandidate(facts({ row: undefined }))).toBeUndefined();
    expect(viewedCandidate(facts({ row: row({ unreadReply: false }) }))).toBeUndefined();
    expect(viewedCandidate(facts({ row: row({ unreadReply: undefined }) }))).toBeUndefined();
    expect(viewedCandidate(facts({ row: row({ state: "archived" }) }))).toBeUndefined();
    expect(viewedCandidate(facts({ online: false }))).toBeUndefined();
  });

  it("needs a successful inspection of the same conversation and reply", () => {
    expect(viewedCandidate(facts({ inspectError: true }))).toBeUndefined();
    expect(viewedCandidate(facts({ inspected: undefined }))).toBeUndefined();
    expect(viewedCandidate(facts({ inspected: { conversation_id: "conv-2", reply_id: REPLY } }))).toBeUndefined();
    expect(viewedCandidate(facts({ inspected: { conversation_id: "conv-1", reply_id: "41:1790000000.1" } }))).toBeUndefined();
    expect(viewedCandidate(facts({ inspected: { conversation_id: "conv-1" } }))).toBeUndefined();
  });

  it("refuses a row without a conversation or reply, even when the inspection agrees", () => {
    expect(viewedCandidate(facts({ row: row({ conversationId: undefined }), inspected: { conversation_id: undefined, reply_id: REPLY } }))).toBeUndefined();
    expect(viewedCandidate(facts({ row: row({ replyId: "" }), inspected: { conversation_id: "conv-1", reply_id: "" } }))).toBeUndefined();
  });

  it("reads no window or view state for a row without an unread reply", () => {
    const active = vi.fn(() => true), activityShows = vi.fn(() => true);
    viewedCandidate(facts({ row: row({ unreadReply: false }), active, activityShows }));
    viewedCandidate(facts({ inspectError: true, active, activityShows }));
    expect(active).not.toHaveBeenCalled();
    expect(activityShows).not.toHaveBeenCalled();
    viewedCandidate(facts({ active: () => false, activityShows }));
    expect(activityShows).not.toHaveBeenCalled();
  });
});

describe("createViewedReply", () => {
  function setup(read = true) {
    let time = 0;
    const deps = { now: () => time, markReplyRead: vi.fn(() => read), keep: vi.fn() };
    const viewed = createViewedReply(deps);
    return { viewed, deps, advance: (ms: number) => { time += ms; } };
  }

  it("reads the reply once it stayed in view for a second, on Qt's 400 ms ticks", () => {
    const { viewed, deps, advance } = setup();
    expect(CHECK_MS).toBe(400);
    expect(DWELL_MS).toBe(1000);
    expect(viewed.check(facts())).toBe(false);
    expect(viewed.candidate()).toBe(CANDIDATE);
    advance(400); expect(viewed.check(facts())).toBe(false);
    advance(400); expect(viewed.check(facts())).toBe(false);
    expect(deps.markReplyRead).not.toHaveBeenCalled();
    advance(400); expect(viewed.check(facts())).toBe(true);
    expect(deps.markReplyRead).toHaveBeenCalledTimes(1);
    expect(deps.markReplyRead).toHaveBeenCalledWith("", "claude/sample-project/a", "conv-1", REPLY);
    expect(deps.keep).toHaveBeenCalledWith("\nrun:a");
    expect(viewed.candidate()).toBeUndefined();
  });

  it("restarts the dwell when the candidate changes: another session, conversation or reply", () => {
    for (const change of [
      { row: row({ key: "\nrun:b" }) },
      { row: row({ conversationId: "conv-2" }), inspected: { conversation_id: "conv-2", reply_id: REPLY } },
      { row: row({ replyId: "43:1790000001" }), inspected: { conversation_id: "conv-1", reply_id: "43:1790000001" } },
    ] satisfies Partial<ViewedFacts>[]) {
      const { viewed, deps, advance } = setup();
      viewed.check(facts());
      advance(900);
      expect(viewed.check(facts(change))).toBe(false);
      advance(900);
      expect(viewed.check(facts(change))).toBe(false);
      advance(100);
      expect(viewed.check(facts(change))).toBe(true);
      expect(deps.markReplyRead).toHaveBeenCalledTimes(1);
    }
  });

  it("restarts the dwell after any check without a candidate", () => {
    const { viewed, deps, advance } = setup();
    viewed.check(facts());
    advance(800);
    expect(viewed.check(facts({ activityShows: () => false }))).toBe(false);
    expect(viewed.candidate()).toBeUndefined();
    advance(400);
    expect(viewed.check(facts())).toBe(false);
    advance(400);
    expect(viewed.check(facts())).toBe(false);
    expect(deps.markReplyRead).not.toHaveBeenCalled();
    advance(600);
    expect(viewed.check(facts())).toBe(true);
  });

  it("forgets the candidate on reset (a hidden window, another selection)", () => {
    const { viewed, deps, advance } = setup();
    viewed.check(facts());
    advance(1200);
    viewed.reset();
    expect(viewed.check(facts())).toBe(false);
    expect(deps.markReplyRead).not.toHaveBeenCalled();
  });

  it("drops a subagent view's candidate", () => {
    const { viewed, advance, deps } = setup();
    viewed.check(facts());
    advance(1200);
    expect(viewed.check(facts({ subagent: true }))).toBe(false);
    expect(viewed.candidate()).toBeUndefined();
    advance(1200);
    expect(viewed.check(facts())).toBe(false);
    expect(deps.markReplyRead).not.toHaveBeenCalled();
  });

  it("never reads an archived or offline row, however long it shows", () => {
    for (const change of [{ row: row({ state: "archived" }) }, { online: false }] satisfies Partial<ViewedFacts>[]) {
      const { viewed, deps, advance } = setup();
      for (let i = 0; i < 10; i++) { viewed.check(facts(change)); advance(400); }
      expect(deps.markReplyRead).not.toHaveBeenCalled();
      expect(deps.keep).not.toHaveBeenCalled();
    }
  });

  it("keeps the row listed even when the reply changed under the mark (Qt sets m_readSelectionKey first)", () => {
    const { viewed, deps, advance } = setup(false);
    viewed.check(facts());
    advance(1000);
    expect(viewed.check(facts())).toBe(false);
    expect(deps.keep).toHaveBeenCalledWith("\nrun:a");
    expect(deps.markReplyRead).toHaveBeenCalledTimes(1);
  });

  it("reads one reply once: the next check starts a new dwell", () => {
    const { viewed, deps, advance } = setup();
    viewed.check(facts());
    advance(1000);
    viewed.check(facts());
    advance(400);
    expect(viewed.check(facts())).toBe(false);
    expect(deps.markReplyRead).toHaveBeenCalledTimes(1);
  });
});

describe("replyVisible", () => {
  const view = (extra: Partial<{ settled: boolean; below: number; viewport: number; rows: Record<string, number> }> = {}): ReplyView => {
    const v = { settled: true, below: 0, viewport: 600, rows: { "item-s42": 120 } as Record<string, number>, ...extra };
    return { settled: () => v.settled, below: () => v.below, viewport: () => v.viewport, offset: (id) => v.rows[id] };
  };
  const inspected = (extra: Record<string, unknown> = {}) => ({ conversation_id: "conv-1", reply_id: REPLY, activity: "idle", ...extra });
  const stop = (seq: number, detail = ""): ActivityEvent => ({ seq, type: "Stop", detail });

  it("is the reply's row starting within the viewport while Activity is at its end", () => {
    expect(replyVisible(view(), REPLY, [stop(42, "Done")], inspected())).toBe(true);
    // A long reply whose top scrolled above the viewport was read to its end.
    expect(replyVisible(view({ rows: { "item-s42": -900 } }), REPLY, [], inspected())).toBe(true);
    expect(replyVisible(view({ rows: { "item-s42": 600 } }), REPLY, [], inspected())).toBe(false);
  });

  it("needs Activity within 30 px of its end", () => {
    expect(NEAR_END).toBe(30);
    expect(replyVisible(view({ below: 30 }), REPLY, [], inspected())).toBe(true);
    expect(replyVisible(view({ below: 31 }), REPLY, [], inspected())).toBe(false);
  });

  it("refuses while the transcript is not settled, and reads no geometry then", () => {
    const below = vi.fn(() => 0), offset = vi.fn(() => 0);
    expect(replyVisible({ settled: () => false, below, viewport: () => 600, offset }, REPLY, [stop(42)], inspected())).toBe(false);
    expect(below).not.toHaveBeenCalled();
    expect(offset).not.toHaveBeenCalled();
  });

  it("uses the recorded response of a timeline without events", () => {
    const rows = { "item-answer-snapshot": 40 };
    expect(replyVisible(view({ rows }), REPLY, [], inspected({ last_message: "Done" }))).toBe(true);
    expect(replyVisible(view({ rows: { "item-answer-snapshot": 700 } }), REPLY, [], inspected({ last_message: "Done" }))).toBe(false);
    // Without a recorded response the snapshot row is not the reply.
    expect(replyVisible(view({ rows: { "item-answer-snapshot": 700 } }), REPLY, [], inspected({ reply_id: "other" }))).toBe(false);
  });

  it("accepts an empty final Stop of an idle session as the result", () => {
    const rows = {};
    expect(replyVisible(view({ rows }), REPLY, [stop(42)], inspected({ reply_id: "other" }))).toBe(true);
    expect(replyVisible(view({ rows }), REPLY, [stop(42)], inspected({ reply_id: "other", activity: "busy" }))).toBe(false);
    expect(replyVisible(view({ rows }), REPLY, [stop(42, "text")], inspected({ reply_id: "other" }))).toBe(false);
    expect(replyVisible(view({ rows }), REPLY, [stop(41)], inspected({ reply_id: "other" }))).toBe(false);
  });

  it("accepts loaded events at or past the reply's sequence when the inspection names the reply", () => {
    const rows = {};
    const later: ActivityEvent[] = [{ seq: 43, type: "PreToolUse", detail: "ls" }];
    expect(replyVisible(view({ rows }), REPLY, later, inspected({ activity: "busy" }))).toBe(true);
    expect(replyVisible(view({ rows }), REPLY, [{ seq: 41, type: "PreToolUse" }], inspected({ activity: "busy" }))).toBe(false);
    expect(replyVisible(view({ rows }), REPLY, later, inspected({ activity: "busy", reply_id: "other" }))).toBe(false);
    expect(replyVisible(view({ rows }), "x:1", [{ seq: 43 }], inspected({ activity: "busy", reply_id: "x:1" }))).toBe(false);
    expect(replyVisible(view({ rows }), REPLY, [], inspected({ activity: "busy" }))).toBe(false);
  });
});
