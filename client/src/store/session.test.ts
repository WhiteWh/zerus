import { describe, expect, it, vi } from "vitest";
import { createComputed, createRoot } from "solid-js";
import * as bridge from "../bridge";
import { archive } from "./archive";
import { activity, receive } from "./session";
import { composer } from "./composer";
import { questions } from "./questions";
import { tasks } from "./tasks";
import { notices } from "./actions";
import { detailTab, setDetailTab, setSelectedKey, setStateFilter } from "./ui";
import { catalog } from "./catalog";
import { grouping, groupOf, groups } from "./organization";
import { rowOut } from "./testing";
import { emptyBatch, type GroupingOut, type ArchivePage } from "../types";

// The wiring reads the page's query (`?select=`, store/ui.ts); the tests run without a page.
vi.hoisted(() => { vi.stubGlobal("location", new URL("http://localhost/")); });

const loaded = (): GroupingOut => ({
  loaded: true, default_project: "ungrouped", ready: true, reason: null, busy: false, status: "", status_error: false, conflicts: 0,
  headers: [
    { id: "alpha", name: "Alpha", color: null, accessible: true, collapsed: false, is_default: false, palette: 0 },
    { id: "ungrouped", name: "General", color: null, accessible: true, collapsed: false, is_default: true, palette: 1 },
  ],
});

describe("receive", () => {
  it("binds the native composer to the resumed key before its first Activity frame", async () => {
    let opened!: () => void;
    const ready = new Promise<void>((resolve) => { opened = resolve; });
    const open = vi.spyOn(composer, "open").mockImplementation(async (session) => {
      if (session?.key === "run:resume-new") await ready;
    });
    const follow = vi.spyOn(bridge.activityPort, "follow").mockResolvedValue();
    const unfollow = vi.spyOn(bridge.activityPort, "unfollow").mockResolvedValue();
    const turn = async () => { for (let i = 0; i < 8; i++) await Promise.resolve(); };
    try {
      const old = { ...rowOut("", 991, { name: "claude/sample-project/resume-status", conversation_id: "same", state: "paused" }), key: "run:resume-old" };
      receive({ ...emptyBatch(), upserts: [old] });
      setSelectedKey("\nrun:resume-old");
      await turn();
      expect(follow.mock.calls.map(([request]) => request.key)).toContain("run:resume-old");
      follow.mockClear();
      receive({ ...emptyBatch(), upserts: [{ ...old, key: "run:resume-new", row: { ...old.row, state: "running" } }],
        removes: [{ computer: "", key: old.key, reason: "replaced" }] });
      await turn();
      expect(open.mock.calls.at(-1)?.[0]?.key).toBe("run:resume-new");
      expect(activity.target()?.key).toBe("run:resume-new");
      expect(follow).not.toHaveBeenCalled();
      opened();
      await turn();
      expect(follow.mock.calls.map(([request]) => request.key)).toEqual(["run:resume-new"]);
    } finally {
      opened();
      setSelectedKey(undefined);
      receive({ ...emptyBatch(), removes: ["run:resume-old", "run:resume-new"].map(key => ({ computer: "", key })) });
      await turn();
      open.mockRestore();
      follow.mockRestore();
      unfollow.mockRestore();
    }
  });

  it("routes the followed project's task list from a batch", async () => {
    const read = vi.spyOn(bridge, "followTasks").mockResolvedValue(undefined);
    tasks.follow("project-task-test");
    receive({ ...emptyBatch(), tasks: [{ computer: "", key: "project-task-test", kind: "list", payload: {
      project: { id: "project-task-test" }, tasks: [{ uuid: "task-1", stage: "todo", text: "Board task" }], damaged: [],
    } }] });
    expect(tasks.tasks().map((t) => t.title)).toEqual(["Board task"]);
    tasks.follow(undefined);
    for (let i = 0; i < 6; i++) await Promise.resolve();
    read.mockRestore();
  });
  it("applies a batch's rows and its grouping in one reactive update", () => createRoot((dispose) => {
    const seen: string[] = [];
    createComputed(() => { seen.push(`${catalog.order.length} ${grouping().loaded} ${groupOf("\nrun:1")}`); });
    seen.length = 0;
    receive({ ...emptyBatch(), upserts: [rowOut("", 1)], groups: [{ computer: "", key: "run:1", group: "alpha", rank: 1 }], grouping: loaded() });
    // One run with everything applied: never the new row in the default project, nor the headers without the row.
    expect(seen).toEqual(["1 true alpha"]);
    dispose();
  }));
});

describe("composer events and the question card", () => {
  const view = (key: string, question: unknown = null) => ({
    handle: 1, key, loaded: true, rev: 0, text_epoch: 1, text: "", position: 0, anchor: 0, question, input_visible: question === null,
    attachments: [], removed: null, attaching: false, sending: false, waiting: null, uncertain: false, status: "", status_error: false,
    can_send: false, send_label: { source: "Send" }, available: true, unavailable_reason: "", block_reason: null, attachments_available: true,
    attachments_reason: "", session_status: "", session_status_error: false, session_blocks_input: false, inspect_error: false,
    cache: { visible: false, tone: "quiet", label: "", short_label: "" }, cache_warning: "", cache_details: "",
    context: { visible: false, text: "", tooltip: "", warning: false, tone: "quiet", percent: null, accessible_name: "" }, local: [],
    cancel_send: false, compacting: false,
    clear: { enabled: false, dismiss: false, title: "", text: "", confirm: "", run: "", conversation: "", cache_enabled: false, running: false, reason: "", cache_reason: "" },
  }) as never;

  it("a notice is Qt's showNotice: an error stays unless it is transient", () => {
    receive({ ...emptyBatch(), composer_events: [
      { kind: "notice", text: "Codex is installing its update in Terminal.", error: false, transient: false },
      { kind: "notice", text: "Answer delivery timed out.", error: true, transient: false },
      { kind: "notice", text: "The native queue is no longer visible.", error: true, transient: true },
    ] });
    const shown = notices().slice(-3).map((n) => [n.tone, n.text, n.sticky]);
    expect(shown).toEqual([
      ["info", "Codex is installing its update in Terminal.", false],
      ["error", "Answer delivery timed out.", true],
      ["error", "The native queue is no longer visible.", false],
    ]);
  });

  it("an open-Terminal request shows the Terminal tab of its own composer only", () => {
    setDetailTab("activity");
    composer.apply(view("studio\nclaude/sample-project/review"));
    receive({ ...emptyBatch(), composer_events: [{ kind: "open_terminal", key: "studio\nclaude/sample-project/other" }] });
    expect(detailTab()).toBe("activity");
    receive({ ...emptyBatch(), composer_events: [{ kind: "open_terminal", key: "studio\nclaude/sample-project/review" }] });
    expect(detailTab()).toBe("terminal");
    setDetailTab("activity");
  });

  it("the card follows the composer's view", () => {
    const card = { card: "question\nk", revision: 1, epoch: 1, asked: "", asked_tip: "", terminal_label: "Open Terminal", view: {} };
    composer.apply(view("studio\nclaude/sample-project/review", card));
    expect(questions.view()?.card).toBe("question\nk");
    composer.apply(view("studio\nclaude/sample-project/review"));
    expect(questions.view()).toBeUndefined();
  });
});

it("routes archived placement through receive without retaining it in live groups", async () => {
  let answer!: (p: ArchivePage) => void;
  const read = vi.spyOn(bridge, "archivePage").mockImplementation(() => new Promise((resolve) => { answer = resolve; }));
  receive({ ...emptyBatch(), computers: [{ computer: "", status: "live", attempt: 0 }] });
  setStateFilter("archived");
  archive.refresh();
  const request = read.mock.calls.at(-1)![5];
  const key = "archive:10000000-0000-4000-8000-000000000001";
  receive({ ...emptyBatch(), groups: [{ computer: "", key, group: "alpha", rank: 30, archive_request: request }] });
  answer({ rows: [{ name: "codex/sample-project/review", state: "archived", archive_id: key.slice(8), group: "ungrouped", rank: 0 }], total: 1, position: 0, next_cursor: "", previous_cursor: null });
  for (let i = 0; i < 10; i++) await Promise.resolve();
  expect(archive.row(`\n${key}`)).toMatchObject({ group: "alpha", rank: 30 });
  expect(groups[`\n${key}`]).toBeUndefined();
  expect(groupOf(`\n${key}`)).toBe("alpha");
  setStateFilter("all");
  read.mockRestore();
});
