import { beforeEach, describe, expect, it, vi } from "vitest";
// The person's switches of the question route (zerus-sa9qn.32); nothing here
// reaches a native side.
const calls = vi.hoisted(() => ({
  set: vi.fn(), invoke: vi.fn(), notify: vi.fn(), offline: vi.fn(() => false),
  state: vi.fn((): Record<string, unknown> | undefined => undefined),
}));
vi.mock("../bridge", () => ({ setQuestionRouting: calls.set }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: calls.invoke }));
vi.mock("./notices", () => ({ bus: { notify: calls.notify } }));
vi.mock("./catalog", () => ({ computerOffline: calls.offline }));
vi.mock("./desktopService", () => ({ desktopService: { state: calls.state } }));
vi.mock("./actions", () => ({ errorText: (error: unknown) => String(error instanceof Error ? error.message : error) }));
import { GLOBAL_UNCONFIRMED, SEND_OFF, SEND_OFFLINE, SEND_ON, SEND_UNKNOWN, globalRouting, routingEntry, routingOf, setGlobalRouting, toggleRouting } from "./questionRouting";
import type { Session } from "../ui/sessions/types";

function child(patch: Partial<Session> = {}): Session {
  return {
    key: `k-${Math.random()}`, computer: "", name: "claude/project/child", cmd: "claude", state: "running",
    runId: "run-1", conversationId: "conversation-1",
    parent: { machine: "m", session_id: "p", name: "codex/project/lead", conversation_id: "c", run_id: "r", state: "live" },
    questionRouting: { sendToParent: true, revision: 0 },
    ...patch,
  } as unknown as Session;
}

beforeEach(() => {
  for (const call of [calls.set, calls.invoke, calls.notify]) call.mockReset();
  calls.offline.mockReturnValue(false);
});

describe("the per-child switch in Session actions", () => {
  it("appears only for a live Claude child, checked by its host's value", () => {
    const entry = routingEntry(child())!;
    expect(entry).toMatchObject({ label: "Send questions to lead", checked: true, disabled: false, hint: SEND_ON });
    expect(routingEntry(child({ questionRouting: { sendToParent: false, revision: 2 } }))).toMatchObject({ checked: false, hint: SEND_OFF });
    expect(routingEntry(child({ parent: undefined }))).toBeUndefined();
    expect(routingEntry(child({ state: "archived" }))).toBeUndefined();
    // Codex has nothing to route in this version.
    expect(routingEntry(child({ cmd: "codex" }))).toBeUndefined();
  });

  it("is off when its host does not say, or its computer is offline", () => {
    expect(routingEntry(child({ questionRouting: undefined }))).toMatchObject({ disabled: true, checked: false, hint: SEND_UNKNOWN });
    calls.offline.mockReturnValue(true);
    expect(routingEntry(child())).toMatchObject({ disabled: true, hint: SEND_OFFLINE });
  });

  it("asks the host once with the row's revision and shows the confirmed value at once", async () => {
    const s = child();
    calls.set.mockResolvedValue({ send_to_parent: false, revision: 1 });
    await toggleRouting(s);
    expect(calls.set).toHaveBeenCalledTimes(1);
    expect(calls.set).toHaveBeenCalledWith({ computer: "", name: "claude/project/child", run_id: "run-1", conversation_id: "conversation-1" }, false, 0);
    expect(routingOf(s)).toEqual({ sendToParent: false, revision: 1 });
    // A later listing with a newer revision wins over the local value.
    const listed = { ...s, questionRouting: { sendToParent: true, revision: 2 } } as Session;
    expect(routingOf(listed)).toEqual({ sendToParent: true, revision: 2 });
  });

  it("reports a refusal and never changes the value", async () => {
    const s = child();
    calls.set.mockRejectedValue(new Error("Only you can change where a child's questions go."));
    await toggleRouting(s);
    expect(calls.notify).toHaveBeenCalledWith("error", "Only you can change where a child's questions go.", true);
    expect(routingOf(s)).toEqual({ sendToParent: true, revision: 0 });
  });

  it("never asks while the host's value is unknown or the computer is offline", async () => {
    await toggleRouting(child({ questionRouting: undefined }));
    calls.offline.mockReturnValue(true);
    await toggleRouting(child());
    expect(calls.set).not.toHaveBeenCalled();
  });
});

describe("the global switch in Settings", () => {
  it("is shown only where the service reports it", () => {
    calls.state.mockReturnValue(undefined);
    expect(globalRouting()).toBeUndefined();
    calls.state.mockReturnValue({ question_routes: null });
    expect(globalRouting()).toBeUndefined();
    calls.state.mockReturnValue({ question_routes: false });
    expect(globalRouting()).toBe(false);
  });

  it("goes through the native frontend lane with a fresh request each time", async () => {
    calls.invoke.mockResolvedValue({ accepted: true, durable: true, enabled: false });
    await setGlobalRouting(false);
    expect(calls.invoke).toHaveBeenCalledWith("desktop_service_question_routes", expect.objectContaining({ enabled: false }));
    const first = calls.invoke.mock.calls[0][1].request_id;
    await setGlobalRouting(false);
    expect(calls.invoke.mock.calls[1][1].request_id).not.toBe(first);
    expect(calls.notify).not.toHaveBeenCalled();
  });

  it("reports an unconfirmed change", async () => {
    calls.invoke.mockResolvedValue({ accepted: null, pending: true, enabled: true });
    await setGlobalRouting(true);
    expect(calls.notify).toHaveBeenCalledWith("error", GLOBAL_UNCONFIRMED, true);
  });
});
