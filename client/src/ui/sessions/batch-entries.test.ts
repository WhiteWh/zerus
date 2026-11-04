import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.hoisted(() => {
  // store/ui.ts reads the page address and storage when it loads.
  const store = new Map<string, string>();
  vi.stubGlobal("location", { search: "", hash: "", href: "http://localhost/" });
  vi.stubGlobal("localStorage", { getItem: (k: string) => store.get(k) ?? null, setItem: (k: string, v: string) => { store.set(k, v); }, removeItem: (k: string) => { store.delete(k); } });
  // The dialogs module registers Solid's delegated events when it loads; nothing renders here.
  vi.stubGlobal("window", { document: { addEventListener: () => {} } });
});
vi.mock("../../bridge", () => ({
  // The session entry points also initialize the bounded creation-card store.
  onCreations: vi.fn(async () => () => {}), creationsView: vi.fn(async () => []),
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
}));

import * as bridge from "../../bridge";
import { applyBatch, catalog } from "../../store/catalog";
import { run, running, runMachine } from "../../store/batch";
import { notices } from "../../store/actions";
import { actionEntries, freshActionEntry } from "./ActionDialogs";
import { emptyBatch, type Outcome } from "../../types";

// The app's stores: a selection batch turns the single-session write entries
// off (Qt's m_pending) and the selection's Forget or Terminate asks through
// the action dialogs.

const mocked = vi.mocked(bridge);
const flush = async () => { for (let i = 0; i < 10; i++) await Promise.resolve(); };

beforeEach(() => {
  vi.useFakeTimers();
  mocked.runAction.mockReset();
  applyBatch({
    ...emptyBatch(),
    upserts: [
      { computer: "", key: "run:a", row: { name: "claude/p/a", agent: "claude", state: "running", resumable: true, activity: "idle", process_state: "running", tracked: true, run_id: "run-a", conversation_id: "conv-a", machine: "box" } },
      { computer: "", key: "run:b", row: { name: "claude/p/b", agent: "claude", state: "paused", resumable: true, activity: "idle", tracked: true, run_id: "run-b", conversation_id: "conv-b", machine: "box" } },
    ],
  });
});
afterEach(() => vi.useRealTimers());

describe("single-session entries while a selection batch runs", () => {
  it("are off until the batch ends", async () => {
    let finish: (o: Outcome) => void = () => {};
    mocked.runAction.mockReturnValueOnce(new Promise((r) => { finish = r; }));
    const a = catalog.rows["\nrun:a"]!;
    const b = catalog.rows["\nrun:b"]!;
    expect(actionEntries(b, undefined, { context: true }).change.disabled).toBe(false);
    expect(freshActionEntry(b)?.disabled).toBe(false);
    const batch = run("pause", [a]);
    await flush();
    expect(running()).toBe(true);
    const context = actionEntries(b, undefined, { context: true });
    expect([context.change.disabled, context.rename.disabled, context.archive?.disabled, context.end.disabled]).toEqual([true, true, true, true]);
    expect(context.change.label).toBe("Resume session");
    expect(actionEntries(b).change.label).toBe("Updating…");
    expect(freshActionEntry(b)?.disabled).toBe(true);
    finish({ outcome: "done", receipt: {}, cleanup: null });
    await batch;
    expect(running()).toBe(false);
    expect(actionEntries(b, undefined, { context: true }).change.disabled).toBe(false);
  });
});

describe("Pause all and Resume all (runMachine)", () => {
  const texts = () => notices().map((n) => `${n.tone}:${n.text}`);
  it("announces, holds the running flag for the one guarded call and words the outcome", async () => {
    let finish: (o: Outcome) => void = () => {};
    mocked.runAction.mockReturnValueOnce(new Promise((r) => { finish = r; }));
    const before = texts().length;
    const call = runMachine("mac", "mac mini", "pause_all", 3);
    await flush();
    expect(running()).toBe(true);
    expect(texts().slice(before)).toEqual(["info:Pausing sessions on mac mini…"]);
    expect(mocked.runAction).toHaveBeenCalledWith({ computer: "mac", name: "", run_id: "", conversation_id: "" }, { kind: "pause_all", count: 3 });
    // A second machine-wide call or a batch cannot start meanwhile.
    await runMachine("", "box", "resume_all", 1);
    expect(mocked.runAction).toHaveBeenCalledTimes(1);
    finish({ outcome: "done", receipt: {}, cleanup: null });
    await call;
    expect(running()).toBe(false);
    expect(texts().slice(before)).toEqual(["info:Pausing sessions on mac mini…", "info:Session updated. Refreshing…"]);
  });
  it("shows a failure's detail and never retries", async () => {
    mocked.runAction.mockResolvedValueOnce({ outcome: "failed", detail: "timed out" });
    const before = texts().length;
    await runMachine("", "box", "resume_all", 2);
    expect(texts().slice(before)).toEqual(["info:Resuming sessions on box…", "error:timed out"]);
    expect(mocked.runAction).toHaveBeenCalledTimes(1);
    expect(mocked.runAction).toHaveBeenCalledWith(expect.anything(), { kind: "resume_all", count: 2 });
    expect(running()).toBe(false);
  });
});
