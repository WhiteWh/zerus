import { beforeEach, describe, expect, it, vi } from "vitest";
vi.hoisted(() => {
  // store/ui.ts reads the page address and storage when it loads.
  const store = new Map<string, string>();
  vi.stubGlobal("location", { search: "", hash: "", href: "http://localhost/" });
  vi.stubGlobal("localStorage", { getItem: (k: string) => store.get(k) ?? null, setItem: (k: string, v: string) => { store.set(k, v); }, removeItem: (k: string) => { store.delete(k); } });
  // The dialogs module registers Solid's delegated events when it loads; nothing renders here.
  vi.stubGlobal("window", { document: { addEventListener: () => {} } });
});
// The undo window, answered at once: each capture gets its delay reference, as the native window would hand out.
const scheduled = vi.hoisted(() => [] as unknown[][]);
vi.mock("./endActions", () => ({
  endActions: {
    schedule: async (captures: unknown[], run: (refs: { id: string; index: number }[], key: string) => Promise<unknown>) => {
      scheduled.push(captures);
      return run(captures.map((_, index) => ({ id: `delay-${scheduled.length}`, index })), "test-end");
    },
  },
}));
vi.mock("../bridge", () => ({
  onTeamDecided: vi.fn(() => () => {}),
  onCreations: vi.fn(async () => () => {}), creationsView: vi.fn(async () => []),
  archivePage: vi.fn(),
  resetArchivePages: vi.fn(async () => {}),
  cancelArchivePages: vi.fn(async () => {}),
  runAction: vi.fn(),
  inspectSession: vi.fn(async () => null),
  pendingReadMarks: vi.fn(async () => []),
  attentionMarks: vi.fn(async () => ({ marks: {}, replies: {} })),
  updateAttentionMarks: vi.fn(async () => {}),
  queueReadMarks: vi.fn(async () => []),
  sessionReference: vi.fn(),
  copyCommand: vi.fn(),
  copyText: vi.fn(),
  setNotifications: vi.fn(),
  terminationCleanupCancel: vi.fn(async () => {}),
}));
import * as bridge from "../bridge";
import { applyBatch, catalog } from "./catalog";
import { end, forget } from "./actions";
import { endAfter } from "./batch";
import { actionEntries } from "../ui/sessions/ActionDialogs";
import { snapshotOf } from "../ui/sessions/actions-ui";
import { selectionSnapshot } from "../ui/sessions/selection";
import { emptyBatch, type Outcome, type ViewRow } from "../types";

// zerus-sa9qn.23 (#2086 follow-up): every end action the person starts in the
// app, and every child a parent's confirmation ends, reaches run_action with a
// delay reference. Only then does the native side run the captured scoped
// envelope (`hgs session-action` with the listed run and conversation, which
// the CLI checks under its lock), never a name-only `hgs archive` or `hgs kill`.
const mocked = vi.mocked(bridge);
const flush = async () => { for (let i = 0; i < 40; i++) await Promise.resolve(); };
const done = (): Outcome => ({ outcome: "done", receipt: {}, cleanup: null });
const view = (tag: string, extra: Partial<ViewRow> = {}): ViewRow => ({
  name: `claude/p/${tag}`, agent: "claude", state: "running", resumable: true, activity: "idle", process_state: "running", tracked: true,
  run_id: `run-${tag}`, conversation_id: `conv-${tag}`, machine: "box", created: 100, ...extra,
});
const listed = (tag: string) => catalog.rows[`\nrun:${tag}`]!;
const target = (tag: string, extra: { run_id?: string; conversation_id?: string } = {}) =>
  ({ computer: "", name: `claude/p/${tag}`, run_id: `run-${tag}`, conversation_id: `conv-${tag}`, ...extra });
/** Every call so far, and that each carried a delay reference. */
const calls = () => mocked.runAction.mock.calls.map(([t, action, delay]) => [t.name, action.kind, delay]);

let seen = 0;
function rows(...views: [string, Partial<ViewRow>?][]) {
  // Fresh names per test: the app's stores live for the whole file.
  const tags = views.map(([tag]) => `${tag}-${++seen}`);
  applyBatch({ ...emptyBatch(), upserts: views.map(([, extra], i) => ({ computer: "", key: `run:${tags[i]}`, row: view(tags[i], extra) })) });
  return tags;
}

beforeEach(() => {
  mocked.runAction.mockReset();
  mocked.runAction.mockResolvedValue(done());
  scheduled.length = 0;
});

describe("the app's end actions always carry a delay reference", () => {
  it("Move to archive from the menu", async () => {
    const [kept] = rows(["kept", { state: "stopped" }]);
    actionEntries(listed(kept)).archive!.action();
    await flush();
    expect(calls()).toEqual([[`claude/p/${kept}`, "archive", { id: "delay-1", index: 0 }]]);
    expect(scheduled[0]).toEqual([{ target: target(kept), action: { kind: "archive" }, state: "stopped" }]);
  });

  it("the Terminate confirmation's Stop and archive, and its Terminate of a session without an HGS archive", async () => {
    const [tracked, plain] = rows(["tracked"], ["plain", { tracked: false }]);
    await end(snapshotOf(listed(tracked)), "terminate");
    await end(snapshotOf(listed(plain)), "kill");
    expect(calls()).toEqual([
      [`claude/p/${tracked}`, "terminate", { id: "delay-1", index: 0 }],
      [`claude/p/${plain}`, "kill", { id: "delay-2", index: 0 }],
    ]);
    expect(scheduled.map((c) => c[0])).toEqual([
      { target: target(tracked), action: { kind: "terminate" }, state: "running" },
      { target: target(plain), action: { kind: "kill" }, state: "running" },
    ]);
  });

  it("the Forget saved session confirmation (zerus-sa9qn.27)", async () => {
    const [saved] = rows(["saved", { state: "stopped" }]);
    await forget(snapshotOf(listed(saved)), { binding: false, run: "", conversation: "" });
    expect(calls()).toEqual([[`claude/p/${saved}`, "forget", { id: "delay-1", index: 0 }]]);
    expect(scheduled[0]).toEqual([{ target: target(saved), action: { kind: "forget" }, state: "stopped" }]);
  });

  it("the Forget confirmation of an OpenCode binding, with the run and conversation it opened with (zerus-sa9qn.27)", async () => {
    const [bound] = rows(["bound", { agent: "opencode", allowed_actions: ["forget", "rename"] }]);
    const row = snapshotOf(listed(bound));
    await forget(row, { binding: true, run: `run-${bound}`, conversation: `conv-${bound}` });
    expect(calls()).toEqual([[`claude/p/${bound}`, "forget_binding", { id: "delay-1", index: 0 }]]);
    expect(scheduled[0]).toEqual([{ target: target(bound), action: { kind: "forget_binding" }, state: "running" }]);
  });

  it("each child a parent's Terminate or Move to archive confirmation ends, with its listed run and conversation", async () => {
    const [lead, worker, plain, saved] = rows(["lead"], ["worker"], ["plain", { tracked: false }], ["saved", { state: "stopped" }]);
    await endAfter("terminate", snapshotOf(listed(lead)), done(), [worker, plain].map((t) => selectionSnapshot(listed(t))));
    await flush();
    expect(calls()).toEqual([
      [`claude/p/${worker}`, "terminate", { id: "delay-1", index: 0 }],
      [`claude/p/${plain}`, "kill", { id: "delay-1", index: 1 }],
    ]);
    expect(scheduled[0]).toEqual([
      { target: target(worker), action: { kind: "terminate" }, state: "running" },
      { target: target(plain), action: { kind: "kill" }, state: "running" },
    ]);
    mocked.runAction.mockClear();
    await endAfter("archive", snapshotOf(listed(lead)), done(), [selectionSnapshot(listed(saved))]);
    await flush();
    expect(calls()).toEqual([[`claude/p/${saved}`, "archive", { id: "delay-2", index: 0 }]]);
    expect(scheduled[1]).toEqual([{ target: target(saved), action: { kind: "archive" }, state: "stopped" }]);
  });
});
