import { describe, expect, it } from "vitest";
import { createRoot } from "solid-js";
import { createModelServers, INTERVAL, MAX_READS, UP_POLL, type Clock, type Port } from "./modelServers";
import type { ModelServer, ModelServerStatus } from "../bridge";

// Qt's ModelServers::Controller over client/core's hgs calls.

const tick = () => new Promise((resolve) => setTimeout(resolve, 0));
const server = (name: string, over: Partial<ModelServer> = {}): ModelServer => ({
  name, base_url: "http://x", kind: "openai", ssh: "", has_key: false, up_minutes: 0, contexts: null, status: ["s"], up: ["u"], down: ["d"], logs: [],
  read_only: false, agent_url: "http://x/v1", models_url: "http://x/v1/models", status_text: "s", up_text: "u", down_text: "d", logs_text: "", ...over,
});
const status = (models: { name: string; state: string }[] = []): ModelServerStatus => ({
  error: "", detail: "", error_label: { source: "" }, problem: { source: "" }, source: "command",
  models: models.map((m) => ({ ...m, label: { source: m.state }, since: 0, usable: m.state === "running", can_down: m.state === "running", can_up: m.state === "stopped" })),
  free: -1, total: -1, memory: { source: "" }, low: false, used: 0, raw: [], running: models.filter((m) => m.state === "running").length,
  active: models.filter((m) => m.state === "running").map((m) => m.name),
});

function fixture(over: Partial<Port> = {}) {
  const calls: string[] = [];
  const pending: (() => void)[] = [];
  let now = 1_000;
  const timers: { every?: () => void; after: (() => void)[] } = { after: [] };
  const clock: Clock = { now: () => now, every: (_ms, run) => { timers.every = run; return () => { timers.every = undefined; }; }, after: (_ms, run) => { timers.after.push(run); } };
  const port: Port = {
    list: async () => { calls.push("list"); return [server("a"), server("b"), server("c")]; },
    status: (name) => { calls.push(`status ${name}`); return new Promise((resolve) => pending.push(() => resolve(status([{ name: "m", state: "running" }])))); },
    up: async (s, m) => { calls.push(`up ${s} ${m}`); return { started: true, id: "r1", limit_ms: 60_000, outcome: null }; },
    upResult: async (s, m, id, expired) => { calls.push(`up-result ${id} ${expired}`); return { running: false, outcome: { text: { source: "Up %1 on %2 finished: exit 0. Nothing starts it again on its own.", args: [{ kind: "verbatim", value: m }, { kind: "verbatim", value: s }] }, error: false } }; },
    sessions: async () => ({}),
    down: async (s, m) => { calls.push(`down ${s} ${m}`); return m === "bad" ? { source: "exit %1", args: [{ kind: "verbatim", value: "3" }] } : null; },
    logs: async () => ({ lines: [], truncated: false }),
    save: async () => [server("a")],
    remove: async () => [],
    test: async () => ({ tone: "ok", text: { source: "1 model" }, raw: [] }),
    testLogs: async () => ({ tone: "ok", text: { source: "x" }, raw: [] }),
    session: async (...args) => { calls.push(`session ${args.join(" ")}`); },
    ...over,
  };
  const store = createRoot(() => createModelServers(port, clock));
  return { store, calls, pending, timers, advance: (ms: number) => { now += ms; } };
}

describe("model servers", () => {
  it("reads at most two servers at a time, only while the tab shows, and every minute", async () => {
    const { store, calls, pending, timers } = fixture();
    await store.refreshList();
    expect(store.available()).toBe(true);
    expect(calls).toEqual(["list"]);
    store.setActive(true);
    expect(calls.filter((c) => c.startsWith("status"))).toHaveLength(MAX_READS);
    expect(store.entry("c").reading).toBe(true);
    store.refreshStatus("a");
    expect(calls.filter((c) => c === "status a")).toHaveLength(1);
    pending.shift()!(); await tick();
    expect(calls).toContain("status c");
    expect(store.entry("a").known).toBe(true);
    expect(store.entry("a").status?.running).toBe(1);
    expect(timers.every).toBeTypeOf("function");
    store.setActive(false);
    expect(timers.every).toBeUndefined();
    void INTERVAL;
  });

  it("an hgs without model servers lists none, so there is no tab", async () => {
    const { store } = fixture({ list: async () => { throw new Error("unknown command"); } });
    await store.refreshList();
    expect(store.listed()).toBe(true);
    expect(store.available()).toBe(false);
  });

  it("Up detaches, then reads its own outcome until it ends", async () => {
    const { store, calls, timers } = fixture({ status: async () => status() });
    await store.refreshList();
    store.up("a", "m");
    expect(store.upRunning("a", "m")).toBe(true);
    store.up("a", "m");
    await tick();
    expect(calls.filter((c) => c === "up a m")).toHaveLength(1);
    expect(store.notice()?.text).toBe("Up m on a is running once; its result shows here when it ends.");
    expect(timers.after).toHaveLength(1);
    void UP_POLL;
    timers.after.shift()!(); await tick();
    expect(calls).toContain("up-result r1 false");
    expect(store.upRunning("a", "m")).toBe(false);
    expect(store.notice()).toEqual({ text: "Up m on a finished: exit 0. Nothing starts it again on its own.", error: false });
  });

  it("a refused Up says so at once", async () => {
    const { store } = fixture({
      status: async () => status(),
      up: async () => ({ started: false, id: "", limit_ms: 0, outcome: { text: { source: "Up %1 on %2 failed: %3", args: [{ kind: "verbatim", value: "m" }, { kind: "verbatim", value: "a" }, { kind: "verbatim", value: "busy" }] }, error: true } }),
    });
    await store.refreshList();
    store.up("a", "m"); await tick();
    expect(store.notice()).toEqual({ text: "Up m on a failed: busy", error: true });
    expect(store.upRunning("a", "m")).toBe(false);
  });

  it("Down runs once per model, one after the other, and names each failure", async () => {
    const { store, calls } = fixture({ status: async () => status() });
    await store.refreshList();
    await store.down("a", ["m", "bad"]);
    expect(calls.filter((c) => c.startsWith("down"))).toEqual(["down a m", "down a bad"]);
    expect(store.notice()).toEqual({ text: "Down on a failed: bad: exit 3", error: true });
    await store.down("a", ["m"]);
    expect(store.notice()).toEqual({ text: "Down ran once on a.", error: false });
  });

  it("a new session starts through hgs and says so", async () => {
    const { store, calls } = fixture();
    await store.startSession("codex", "/work/app", "small", "a", "small");
    expect(calls.at(-1)).toBe("session codex /work/app small a small");
    expect(store.notice()?.text).toBe("Starting small on a with codex…");
  });
});
