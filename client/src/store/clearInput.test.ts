import { describe, expect, it, vi } from "vitest";
import { createRoot } from "solid-js";
import { clearableText, composerTarget, createClearInput } from "./clearInput";
import { draftKey } from "./draftKeys";
import type { ComposerView } from "../types";
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
    const offered = () => "[Image #1] residue";
    expect(await store.clear("b", "[Image #1] residue", offered)).toBeUndefined();
    const outcome = await store.clear("a", "[Image #1] residue", offered);
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
    expect(await busy.store.clear("a", "residue", () => "residue")).toBeUndefined();
    expect(busy.port.clear).not.toHaveBeenCalled();
    const lost = setup(async () => residue("residue"), async () => { throw new Error("channel closed"); });
    lost.store.follow("a", A);
    await flush();
    expect(await lost.store.clear("a", "residue", () => "residue")).toEqual({ status: "uncertain", error: "Error: channel closed", composer: null });
  });

  it("runs one clear at a time", async () => {
    let finish!: (value: ClearOutcome) => void;
    const { store, port } = setup(async () => residue("residue"), () => new Promise((resolve) => { finish = resolve; }));
    store.follow("a", A);
    await flush();
    const first = store.clear("a", "residue", () => "residue");
    expect(store.clearing()).toBe(true);
    expect(await store.clear("a", "residue", () => "residue")).toBeUndefined();
    finish({ status: "cleared", error: "", composer: null });
    await first;
    expect(port.clear).toHaveBeenCalledTimes(1);
  });

  it("re-checks the offer as the person confirms: nothing leaves once it is withdrawn", async () => {
    const { store, port } = setup(async () => residue("residue"));
    store.follow("a", A);
    await flush();
    expect(await store.clear("a", "residue", () => undefined)).toBeUndefined();
    expect(await store.clear("a", "residue", () => "other text")).toBeUndefined();
    expect(port.clear).not.toHaveBeenCalled();
  });
});

describe("Clear input is offered only while nothing is sent to the session (G1 of #2094)", () => {
  const SESSION_ID = "6f1c2a4e-1d2b-4c3d-8e4f-5a6b7c8d9e0f";
  // The list row's key and the composer's draft key differ: run:<name> against id:<session id>.
  const s = { key: "\nrun:claude/p/a", catalogKey: "run:claude/p/a", computer: "", name: "claude/p/a", sessionId: SESSION_ID, status: "ready" as const };
  const draft = draftKey({ computer: "", key: s.catalogKey, name: s.name, archive_id: null, session_id: SESSION_ID });
  const view = (extra: Partial<ComposerView> = {}) =>
    ({ key: draft, sending: false, delivery_pending: false, waiting: null, local: [], ...extra }) as unknown as ComposerView;
  const read = { key: s.key, composer: residue("[Image #1] residue") };

  it("hides it for each delivery state of this session and while its agent works", () => {
    expect(draft).not.toBe(s.key);
    expect(clearableText(read, s, view({ sending: true }), false)).toBeUndefined();
    expect(clearableText(read, s, view({ waiting: "Waiting for the agent" }), false)).toBeUndefined();
    expect(clearableText(read, s, view({ delivery_pending: true }), false)).toBeUndefined();
    expect(clearableText(read, s, view({ local: [{ status: "sending" } as ComposerView["local"][number]] }), false)).toBeUndefined();
    expect(clearableText(read, { ...s, status: "working" }, view(), false)).toBeUndefined();
    expect(clearableText(read, s, view(), true)).toBeUndefined();
  });

  it("offers the read text otherwise, including while another session sends", () => {
    expect(clearableText(read, s, view(), false)).toBe("[Image #1] residue");
    expect(clearableText(read, s, view({ key: "\nid:00000000-0000-4000-8000-000000000001", sending: true }), false)).toBe("[Image #1] residue");
    expect(clearableText(read, s, undefined, false)).toBe("[Image #1] residue");
    expect(clearableText({ ...read, key: "\nrun:other" }, s, view(), false)).toBeUndefined();
  });
});

describe("Composer reads stay bounded (zerus-wkj9l.2)", () => {
  it("keeps at most one read in flight for the shown session", async () => {
    const pending: Array<(value: ComposerState) => void> = [];
    const { store, port } = setup(() => new Promise((resolve) => { pending.push(resolve); }));
    store.follow("a", A);
    for (let n = 0; n < 5; n++) void store.refresh();
    expect(port.read).toHaveBeenCalledTimes(1);
    pending[0](residue("a text"));
    await flush();
    expect(store.state()).toEqual({ key: "a", composer: residue("a text") });
    void store.refresh();
    expect(port.read).toHaveBeenCalledTimes(2);
    // Another session is read at once; the first session's slow read is dropped.
    store.follow("b", B);
    expect(port.read).toHaveBeenCalledTimes(3);
    expect(port.read.mock.calls[2][0]).toBe(B);
    pending[1](residue("late a text"));
    pending[2](residue("b text"));
    await flush();
    expect(store.state()).toEqual({ key: "b", composer: residue("b text") });
  });

  it("drops a read that started before a clear and reads again once it settles", async () => {
    const pending: Array<(value: ComposerState) => void> = [];
    let first = true;
    const { store, port } = setup(() => {
      if (first) { first = false; return Promise.resolve(residue("residue")); }
      return new Promise((resolve) => { pending.push(resolve); });
    });
    store.follow("a", A);
    await flush();
    void store.refresh(); // a poll still in flight when the clear finishes
    expect(port.read).toHaveBeenCalledTimes(2);
    expect((await store.clear("a", "residue", () => "residue"))?.status).toBe("cleared");
    await flush();
    expect(port.read).toHaveBeenCalledTimes(2);
    expect(store.state()).toBeUndefined(); // nothing is offered right after a clear
    pending[0](residue("stale poll")); // read before the clear: never shown
    await flush();
    expect(port.read).toHaveBeenCalledTimes(3);
    expect(store.state()).toBeUndefined();
    pending[1]({ binding: BINDING, state: "empty", text: "", clearable: false, reason: "The input is empty" });
    await flush();
    expect(store.state()?.composer.state).toBe("empty");
  });

  it("reads only a live run of an agent whose composer can be read", () => {
    const row = { computer: "peer", name: "claude/p/a", runId: "r", conversationId: "c", state: "running" as const, cmd: "claude" };
    expect(composerTarget(row)).toEqual({ computer: "peer", name: "claude/p/a", run_id: "r", conversation_id: "c" });
    for (const cmd of ["codex", "kimi"]) expect(composerTarget({ ...row, cmd })).toBeDefined();
    for (const cmd of ["opencode", "dsh", "sh", ""]) expect(composerTarget({ ...row, cmd })).toBeUndefined();
    expect(composerTarget({ ...row, state: "archived" as const })).toBeUndefined();
    expect(composerTarget({ ...row, runId: "" })).toBeUndefined();
    expect(composerTarget(undefined)).toBeUndefined();
  });
});
