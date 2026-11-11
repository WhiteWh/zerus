import { describe, expect, it, vi } from "vitest";
import { createRoot } from "solid-js";
import { createClearInput } from "./clearInput";
import type { ClearOutcome, ComposerState } from "../bridge";
import type { Target } from "../types";

// zerus-wkj9l: Terminal's Clear input reads the shown session's composer and
// clears only the exact text the person confirmed, for that session.
const A: Target = { computer: "", name: "claude/p/a", run_id: "ra", conversation_id: "ca" };
const B: Target = { computer: "peer", name: "codex/p/b", run_id: "rb", conversation_id: "cb" };
const BINDING = "a".repeat(64);
const residue = (text: string, clearable = true): ComposerState => ({ binding: BINDING, state: "text", text, clearable, reason: clearable ? "" : "Wait until the agent is idle" });
const flush = async () => { for (let n = 0; n < 8; n++) await Promise.resolve(); };

function setup(read: (t: Target) => Promise<ComposerState | null>, clear?: (t: Target, b: string, text: string) => Promise<ClearOutcome>) {
  const port = { read: vi.fn(read), clear: vi.fn(clear ?? (async () => ({ status: "cleared", error: "", composer: null }) as ClearOutcome)) };
  return { port, store: createRoot(() => createClearInput(port)) };
}

describe("Clear input (zerus-wkj9l)", () => {
  it("reads the shown session and drops an answer for a session no longer shown", async () => {
    let release!: (value: ComposerState) => void;
    const { store } = setup((t) => t === A ? new Promise((resolve) => { release = resolve; }) : Promise.resolve(residue("b text")));
    store.follow("a", A);
    store.follow("b", B);
    release(residue("a text"));
    await flush();
    expect(store.state()).toEqual({ key: "b", composer: residue("b text") });
    store.follow(undefined);
    await flush();
    expect(store.state()).toBeUndefined();
  });

  it("clears only the confirmed text of the session read, with its binding", async () => {
    const { store, port } = setup(async () => residue("[Image #1] residue"));
    store.follow("a", A);
    await flush();
    expect(await store.clear("b", "[Image #1] residue")).toBeUndefined();
    const outcome = await store.clear("a", "[Image #1] residue");
    expect(outcome?.status).toBe("cleared");
    expect(port.clear).toHaveBeenCalledExactlyOnceWith(A, BINDING, "[Image #1] residue");
    // It reads the composer again afterwards.
    await flush();
    expect(port.read.mock.calls.length).toBeGreaterThanOrEqual(2);
  });

  it("never clears a composer read as not clearable, and a lost answer is uncertain", async () => {
    const busy = setup(async () => residue("residue", false));
    busy.store.follow("a", A);
    await flush();
    expect(await busy.store.clear("a", "residue")).toBeUndefined();
    expect(busy.port.clear).not.toHaveBeenCalled();
    const lost = setup(async () => residue("residue"), async () => { throw new Error("channel closed"); });
    lost.store.follow("a", A);
    await flush();
    expect(await lost.store.clear("a", "residue")).toEqual({ status: "uncertain", error: "Error: channel closed", composer: null });
  });

  it("runs one clear at a time", async () => {
    let finish!: (value: ClearOutcome) => void;
    const { store, port } = setup(async () => residue("residue"), () => new Promise((resolve) => { finish = resolve; }));
    store.follow("a", A);
    await flush();
    const first = store.clear("a", "residue");
    expect(store.clearing()).toBe(true);
    expect(await store.clear("a", "residue")).toBeUndefined();
    finish({ status: "cleared", error: "", composer: null });
    await first;
    expect(port.clear).toHaveBeenCalledTimes(1);
  });
});
