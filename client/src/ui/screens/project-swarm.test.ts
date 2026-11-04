import { describe, expect, it } from "vitest";
import { createSwarm, syncText, seedMappings, initialAlias, type Call, type SwarmView } from "./project-swarm";
const tick = async () => { for (let i = 0; i < 12; ++i) await Promise.resolve(); };
const frame = (): SwarmView => ({ id: "opaque-dialog", catalog: { swarm_id: "catalog", conflicts: 0, computers: [] }, projects: [], aliases: ["fixture-peer"], preview: null, status: "", error: false, busy: false, changing: false, can_sync: true, can_join: false, can_preview: true, mode: "config" });
const port = (read: (command: string, args?: Record<string, unknown>) => Promise<unknown>): Call => async <T,>(command: string, args?: Record<string, unknown>) => await read(command, args) as T;
describe("Project swarm event controller", () => {
  it("opens read-only and coalesces a burst into one newest view without polling", async () => {
    const calls: string[] = []; let release!: (v: SwarmView) => void, reads = 0;
    const state = createSwarm(port(async (command) => {
      calls.push(command);
      if (command === "project_swarm_open") return frame();
      if (command === "project_swarm_view") { ++reads; return reads === 1 ? await new Promise<SwarmView>((r) => { release = r; }) : frame(); }
    }));
    await state.open();
    for (let i = 0; i < 100; ++i) state.changed();
    expect(reads).toBe(1);
    release(frame()); await tick();
    expect(reads).toBe(2);
    expect(calls).toEqual(["project_swarm_open", "project_swarm_view", "project_swarm_view"]);
    state.dispose();
  });
  it("never lets an old status read overwrite the explicit action's newer view", async () => {
    let release!: (v: SwarmView) => void, reads = 0;
    const fresh = { ...frame(), mode: "preview", preview: { id: "new-preview", alias: "fixture-peer", initialized: true, same_swarm: true, projects: [] }, can_join: true };
    const state = createSwarm(port(async (command) => {
      if (command === "project_swarm_open") return frame();
      if (command === "project_swarm_view") return ++reads === 1 ? await new Promise<SwarmView>((r) => { release = r; }) : fresh;
      if (command === "project_swarm_preview") return fresh;
    }));
    await state.open();
    expect(await state.preview("fixture-peer")).toBe(true);
    expect(state.frame()?.preview?.id).toBe("new-preview");
    release({ ...frame(), status: "older source" }); await tick();
    expect(state.frame()?.preview?.id).toBe("new-preview");
    expect(state.frame()?.status).not.toBe("older source");
    state.dispose();
  });
  it("delivers an exact Join once and never retries a lost result from an event", async () => {
    const calls: { command: string; args?: Record<string, unknown> }[] = [];
    let current = { ...frame(), can_join: true }, release!: (v: SwarmView) => void;
    const state = createSwarm(port(async (command, args) => {
      calls.push({ command, args });
      if (command === "project_swarm_open" || command === "project_swarm_view") return current;
      if (command === "project_swarm_join") return await new Promise<SwarmView>((r) => { release = r; });
    }));
    await state.open(); await tick();
    const first = state.join([["exact-local", "exact-peer"]], false);
    expect(await state.join([], true)).toBe(false);
    current = { ...frame(), error: true, status: "reply lost", can_join: false };
    release(current); await first; await tick();
    for (let i = 0; i < 20; ++i) state.changed();
    await tick();
    expect(await state.join([], true)).toBe(false);
    expect(calls.filter((r) => r.command === "project_swarm_join")).toEqual([{ command: "project_swarm_join", args: { id: "opaque-dialog", mappings: [["exact-local", "exact-peer"]], keep: false } }]);
    expect(state.frame()?.error).toBe(true);
    state.dispose();
  });
  it("closes a late open handle after unmount without reading or starting anything", async () => {
    const calls: { command: string; args?: Record<string, unknown> }[] = [];
    let release!: (v: SwarmView) => void;
    const state = createSwarm(port(async (command, args) => {
      calls.push({ command, args });
      if (command === "project_swarm_open") return await new Promise<SwarmView>((r) => { release = r; });
    }));
    const waiting = state.open(); state.dispose(); release(frame()); await waiting;
    expect(calls).toEqual([{ command: "project_swarm_open", args: undefined }, { command: "project_swarm_close", args: { id: "opaque-dialog" } }]);
    expect(state.frame()).toBeUndefined();
  });
  it("keeps changing calls open and refuses a mismatched native view identity", async () => {
    let next = { ...frame(), changing: true }, closes = 0;
    const state = createSwarm(port(async (command) => {
      if (command === "project_swarm_open" || command === "project_swarm_view") return next;
      if (command === "project_swarm_close") ++closes;
    }));
    await state.open(); await tick();
    expect(await state.close()).toBe(false); expect(closes).toBe(0);
    next = { ...frame(), id: "other-dialog", changing: false }; state.changed(); await tick();
    expect(state.frame()?.id).toBe("opaque-dialog");
    expect(state.error()).toContain("expired");
    state.dispose();
  });
  it("retains the native error text without an outdated warning or dot separator", () => {
    expect(syncText({ id: "peer", name: "Peer", access: "fixture-peer", sync: { state: "error", text: "fixture connection refused" }, tasks_outdated: true })).toBe("fixture connection refused");
    expect(syncText({ id: "peer", name: "Peer", access: "fixture-peer", sync: { state: "waiting" }, tasks_outdated: true })).toBe("Waiting to sync. Update Zerus there to share tasks");
  });
  it("submits only the exact review once and never retries Resolve on an event", async () => {
    let resolves = 0;
    const state = createSwarm(port(async (command, args) => {
      if (command === "project_swarm_open" || command === "project_swarm_view") return frame();
      if (command === "project_swarm_resolve") { ++resolves; expect(args).toEqual({ id: "opaque-dialog", review: "captured-review", confirmed: true }); throw Error("reply lost"); }
    }));
    await state.open(); await tick(); expect(await state.resolve("captured-review", true)).toBe(false);
    state.changed(); await tick(); expect(resolves).toBe(1); state.dispose();
  });
});

it("preselects identical project IDs honestly, while distinct and prototype-shaped IDs stay separate", () => {
  const local = [{ id: "ungrouped", name: "Inbox" }, { id: "local-only", name: "Same name" }, { id: "constructor", name: "Opaque ID" }];
  const peer = [{ id: "ungrouped", name: "Inbox" }, { id: "remote-only", name: "Same name" }];
  const mapping = seedMappings(local, peer);
  expect([...mapping]).toEqual([["ungrouped", "ungrouped"]]);
  expect(mapping.get("local-only")).toBeUndefined(); expect(mapping.get("constructor")).toBeUndefined();
});
it("selects the notice's exact unconnected alias after choices arrive, with a saved-choice fallback", () => {
  expect(initialAlias(["already-connected", "new-peer"], "new-peer")).toBe("new-peer");
  expect(initialAlias(["already-connected"], "not-configured")).toBe("already-connected");
  expect(initialAlias([], "new-peer")).toBe("");
});
