import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createStore } from "solid-js/store";
import { createRoot, createSignal } from "solid-js";
import { createEndActions, endTitles, type EndPort } from "./endActions";
import { pendingEnd, sameEnd, type EndCapture } from "./endActionBusy";
import { createNoticeBus, NOTICE_MS } from "./notices";

const capture = (name = "claude/example", kind = "archive"): EndCapture => ({ target: { computer: "test", name, run_id: "run-1", conversation_id: "conversation-1" }, action: { kind } as EndCapture["action"], state: kind === "archive" ? "paused" : "running" });
const tick = async () => { await Promise.resolve(); await Promise.resolve(); await Promise.resolve(); };
function fixture() {
  const bus = createNoticeBus(), waits = new Map<string, (ready: boolean) => void>();
  const calls: string[] = []; let next = 0;
  const port: EndPort = {
    begin: async (id, captures) => { calls.push(`begin ${id} ${captures.length}`); },
    wait: (id) => new Promise((resolve) => { waits.set(id, resolve); }),
    cancel: async (id) => { calls.push(`cancel ${id}`); waits.get(id)?.(false); },
  };
  const app = createEndActions(port, bus, () => `id-${++next}`, () => Date.now());
  return { app, bus, calls, ready: (id = "id-1") => waits.get(id)?.(true), port };
}

describe("confirmed end action grace", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());
  it("only paints time; even a late browser timer never releases native timing", async () => {
    const f = fixture(), effect = vi.fn(async () => "done");
    const job = f.app.schedule([capture()], effect); await tick();
    const notice = f.bus.notices()[0], label = notice.actions![0].label as () => string;
    expect(notice).toMatchObject({ title: "Archiving session", text: "claude/example", sticky: true, dismissible: false });
    expect(label()).toBe("Undo (3)"); expect(pendingEnd("test", "claude/example")).toBe(true);
    vi.advanceTimersByTime(1100); expect(label()).toBe("Undo (2)"); expect(effect).not.toHaveBeenCalled();
    vi.advanceTimersByTime(5000); expect(label()).toBe("Undo (1)"); expect(effect).not.toHaveBeenCalled();
    f.ready(); expect(await job).toBe("done");
    expect(effect).toHaveBeenCalledExactlyOnceWith([{ id: "id-1", index: 0 }], "session-end:test\nclaude/example");
    expect(pendingEnd("test", "claude/example")).toBe(false);
  });
  it.each(["archive", "terminate", "kill", "forget", "forget_binding", "forget_archive", "clear_context"])("Undo cancels %s without any writer", async (kind) => {
    const f = fixture(), effect = vi.fn();
    const job = f.app.schedule([capture(`agent/${kind}`, kind)], effect); await tick();
    await f.bus.notices()[0].actions![0].run(); await job;
    expect(effect).not.toHaveBeenCalled(); expect(f.calls).toContain("cancel id-1");
    expect(f.bus.notices()[0].text).toMatch(/cancelled$/);
  });
  it("names single and batch sessions with the session list's own titles", async () => {
    const rows = [
      { computer: "other", name: "codex/example/a", title: "elsewhere" },
      { computer: "test", name: "codex/example/a", title: "a" },
      { computer: "test", name: "codex/example/b", title: "" },
      { computer: "test", name: "codex/example/c", title: "Renamed c" },
    ];
    const names = ["codex/example/a", "codex/example/b", "codex/example/c", "codex/example/d"].map((n) => capture(n));
    expect(endTitles(names, rows)).toEqual(["a", "codex/example/b", "Renamed c"]);
    expect(endTitles([capture("codex/example/missing")], rows)).toEqual(["codex/example/missing"]);
    const bus = createNoticeBus(), app = createEndActions({ begin: async () => {}, wait: () => new Promise(() => {}), cancel: async () => {} }, bus,
      () => "id-1", () => Date.now(), (captures) => endTitles(captures, rows));
    void app.schedule([names[0]], vi.fn()); await tick();
    expect(bus.notices()[0].text).toBe("a");
    app.close();
    const batch = createNoticeBus(), many = createEndActions({ begin: async () => {}, wait: () => new Promise(() => {}), cancel: async () => {} }, batch,
      () => "id-2", () => Date.now(), (captures) => endTitles(captures, rows));
    void many.schedule(names, vi.fn()); await tick();
    expect(batch.notices()[0].text).toBe("a, codex/example/b, Renamed c and 1 more");
    many.close();
  });
  it("one batch has one Undo and exact indexed references; Undo cancels the entire batch", async () => {
    const f = fixture(), effect = vi.fn(); const rows = [capture("a"), capture("b"), capture("c")];
    const job = f.app.schedule(rows, effect); await tick();
    expect(f.bus.notices()).toHaveLength(1); expect(f.bus.notices()[0].title).toBe("Archiving 3 sessions"); expect(f.bus.notices()[0].text).toBe("a, b, c");
    f.ready(); await job;
    expect(effect.mock.calls[0][0]).toEqual([0, 1, 2].map((index) => ({ id: "id-1", index })));
    const cancelled = f.app.schedule(rows, effect); await tick(); await f.bus.notices()[0].actions![0].run(); await cancelled;
    expect(effect).toHaveBeenCalledTimes(1);
    const larger = f.app.schedule([...rows, capture("agent/project/d"), capture("agent/project/e")], effect); await tick();
    const notice = f.bus.notices().find((n) => n.dismissible === false)!;
    expect(notice.text).toBe("a, b, c and 2 more"); await notice.actions![0].run(); await larger;
    expect(effect).toHaveBeenCalledTimes(1);
  });
  it("keeps five visible Undo controls and annotates the newest on a sixth without native begin", async () => {
    const f = fixture(), effect = vi.fn();
    const jobs = Array.from({ length: 5 }, (_, i) => f.app.schedule([capture(`row${i}`)], effect)); await tick();
    const original = [...f.bus.notices()];
    await f.app.schedule([capture("sixth")], effect);
    expect(f.bus.notices()).toEqual(original); expect(f.calls.filter((c) => c.startsWith("begin"))).toHaveLength(5);
    expect(original[4].note!()).toBe("Five actions are waiting. Undo one or wait a moment.");
    f.bus.notify("error", "ordinary notice", true); expect(f.bus.notices()).toEqual(original);
    vi.advanceTimersByTime(NOTICE_MS - 1); expect(original[4].note!()).not.toBe("");
    await f.app.schedule([capture("sixth")], effect); vi.advanceTimersByTime(1); expect(original[4].note!()).not.toBe("");
    vi.advanceTimersByTime(NOTICE_MS); expect(original[4].note!()).toBe("");
    for (const n of original) await n.actions![0].run(); await Promise.all(jobs);
    expect(effect).not.toHaveBeenCalled(); expect(pendingEnd("test", "sixth")).toBe(false);
  });
  it("a changed source cancels a single window without waiting or dispatch", async () => {
    await createRoot(async (dispose) => {
      const f = fixture(), effect = vi.fn(); const [same, setSame] = createSignal(true);
      const job = f.app.schedule([capture("changing")], effect, { current: same }); await tick();
      setSame(false); await tick(); await job;
      expect(effect).not.toHaveBeenCalled(); expect(f.bus.notices()[0].text).toBe("Archive cancelled: the session changed."); dispose();
    });
  });
  it("ignores presentation/read changes but cancels an actually replaced target", async () => {
    await createRoot(async (dispose) => {
      const f = fixture(), effect = vi.fn();
      const captured = { key: "target", computer: "test", name: "changing", runId: "run-1", conversationId: "conversation-1", state: "running", unread: true, activity: "idle" };
      const [row, update] = createStore({ ...captured });
      const job = f.app.schedule([capture("changing", "clear_context")], effect, { current: () => sameEnd(captured, row) }); await tick();
      update({ unread: false, activity: "working" }); await tick();
      expect(f.bus.notices()[0].title).toBe("Clearing session");
      expect(f.calls).not.toContain("cancel id-1");
      update({ conversationId: "replacement" }); await tick(); await job;
      expect(effect).not.toHaveBeenCalled();
      expect(f.bus.notices()[0].text).toBe("Clear cancelled: the session changed.");
      dispose();
    });
  });
  it("quit/page destruction makes Undo inert and drops pending calls, even if native wait answered true", async () => {
    const f = fixture(), effect = vi.fn();
    const job = f.app.schedule([capture("quitting")], effect); await tick(); const undo = f.bus.notices()[0].actions![0];
    f.app.close(); f.ready(); await job; await undo.run();
    expect(effect).not.toHaveBeenCalled(); expect(undo.disabled!()).toBe(true);
    await f.app.schedule([capture("late")], effect); expect(f.calls.filter((c) => c.startsWith("begin"))).toHaveLength(1);
  });
  it("registration failure releases presentation busy and preserves the refusal text", async () => {
    const f = fixture(); f.port.begin = async () => { throw "Too much session data selected at once. Select fewer sessions."; };
    await f.app.schedule([capture("refused")], vi.fn());
    expect(pendingEnd("test", "refused")).toBe(false); expect(f.bus.notices()[0].text).toBe("Too much session data selected at once. Select fewer sessions.");
  });
  it("closing before admission acknowledgement cancels a late registration without dispatch", async () => {
    const f = fixture(); let admit!: () => void; f.port.begin = () => new Promise((r) => { admit = r; });
    const effect = vi.fn(), job = f.app.schedule([capture("slow-admission")], effect); f.app.close(); admit(); await job;
    expect(effect).not.toHaveBeenCalled(); expect(f.calls).toContain("cancel id-1");
  });
});
