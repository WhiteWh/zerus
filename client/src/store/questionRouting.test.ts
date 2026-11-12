import { beforeEach, describe, expect, it, vi } from "vitest";
// The person's switches of the question route (zerus-sa9qn.32); nothing here
// reaches a native side.
const calls = vi.hoisted(() => ({
  set: vi.fn(), invoke: vi.fn(), notify: vi.fn(), offline: vi.fn(() => false),
  state: vi.fn((): Record<string, unknown> | undefined => undefined),
  enabled: vi.fn(() => false),
}));
vi.mock("../bridge", () => ({ setQuestionRouting: calls.set }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: calls.invoke }));
vi.mock("./notices", () => ({ bus: { notify: calls.notify } }));
vi.mock("./catalog", () => ({ computerOffline: calls.offline }));
vi.mock("./desktopService", () => ({ desktopService: { state: calls.state, enabled: calls.enabled } }));
vi.mock("./actions", () => ({ errorText: (error: unknown) => String(error instanceof Error ? error.message : error) }));
import { GLOBAL_UNCONFIRMED, LATCH_UNCONFIRMED, SEND_LATCHED, SEND_OFF, SEND_OFFLINE, SEND_ON, SEND_UNKNOWN, globalRouting, latchOf, routingEntry, routingOf, setGlobalRouting, toggleRouting } from "./questionRouting";
import type { Session } from "../ui/sessions/types";
import contract from "../../../mobile/android/app/src/test/resources/question-routing-contract.json";

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

describe("the shared question routing contract", () => {
  it("shows the switch for live Claude children only, as the phones do", () => {
    for (const c of contract.availability) {
      const row = c.row as Record<string, unknown> & { name: string; cmd: string; state: string; parent?: { name: string } };
      const s = child({ name: row.name, cmd: row.cmd, state: row.state,
        parent: row.parent ? { machine: "m", session_id: "p", name: row.parent.name, conversation_id: "c", run_id: "r", state: "live" } : undefined,
        questionRouting: row.question_routing
          ? { sendToParent: (row.question_routing as { send_to_parent: boolean }).send_to_parent, revision: (row.question_routing as { revision: number }).revision }
          : undefined } as Partial<Session>);
      const entry = routingEntry(s);
      expect(!!entry, c.name).toBe(c.shown);
      if (!entry) continue;
      expect(!entry.disabled, c.name).toBe(c.enabled);
      expect(entry.label.endsWith(c.parent ?? ""), c.name).toBe(true);
    }
  });
});

describe("the person's latch on a child of this computer (zerus-sa9qn.50)", () => {
  const SESSION = "00000000-0000-4000-8000-000000000001";
  const local = (patch: Partial<Session> = {}) => child({ sessionId: SESSION, ...patch } as Partial<Session>);
  /** The service's status and latch replies, and the host's. */
  function service(latch: { person_off: boolean; revision: number }, reply?: (args: Record<string, unknown>) => unknown) {
    calls.invoke.mockImplementation(async (command: string, args: Record<string, unknown>) => {
      if (command === "desktop_service_question_route_child_status") return { ...latch };
      if (command === "desktop_service_question_route_child") {
        return reply ? reply(args) : { accepted: true, durable: true, op: "question_route_child", session_id: args.session_id,
          person_off: args.person_off, revision: latch.revision + 1, applied: true };
      }
      throw new Error(`unexpected ${command}`);
    });
  }
  beforeEach(() => calls.enabled.mockReturnValue(true));
  const latchCalls = () => calls.invoke.mock.calls.filter(([command]) => command === "desktop_service_question_route_child");

  it("turning off latches first, at the service's revision, then turns the host off", async () => {
    const s = local({ key: "latch-off" });
    service({ person_off: false, revision: 0 });
    calls.set.mockResolvedValue({ send_to_parent: false, revision: 1 });
    await toggleRouting(s);
    expect(latchCalls()).toHaveLength(1);
    expect(latchCalls()[0][1]).toMatchObject({ session_id: SESSION, run_id: "run-1", conversation_id: "conversation-1", person_off: true, expected_revision: 0 });
    expect(calls.set).toHaveBeenCalledWith(expect.anything(), false, 0);
    expect(latchOf(s)).toEqual({ personOff: true, revision: 1 });
    expect(routingEntry(s)).toMatchObject({ checked: false, hint: SEND_LATCHED });
  });

  it("an agent's host file on still shows off; turning on clears the latch first", async () => {
    const s = local({ key: "latch-on", questionRouting: { sendToParent: true, revision: 4 } });
    service({ person_off: true, revision: 3 });
    // The first read of the latch is shown as it arrives.
    latchOf(s);
    await vi.waitFor(() => expect(latchOf(s)).toEqual({ personOff: true, revision: 3 }));
    expect(routingEntry(s)).toMatchObject({ checked: false, hint: SEND_LATCHED });
    calls.set.mockResolvedValue({ send_to_parent: true, revision: 4 });
    await toggleRouting(s);
    expect(latchCalls()[0][1]).toMatchObject({ person_off: false, expected_revision: 3 });
    expect(calls.set).toHaveBeenCalledWith(expect.anything(), true, 4);
    expect(routingEntry(s)).toMatchObject({ checked: true, hint: SEND_ON });
  });

  it("turning on never reaches the host unless the service cleared the latch", async () => {
    const s = local({ key: "latch-refused", questionRouting: { sendToParent: false, revision: 1 } });
    service({ person_off: true, revision: 2 }, () => ({ accepted: false, durable: false, code: "conflict" }));
    await toggleRouting(s);
    expect(calls.set).not.toHaveBeenCalled();
    expect(calls.notify).toHaveBeenCalledWith("error", LATCH_UNCONFIRMED, true);
  });

  it("turning off reaches the host even when the latch was not confirmed: off sends less", async () => {
    const s = local({ key: "latch-off-unconfirmed" });
    service({ person_off: false, revision: 0 }, () => { throw new Error("frontend_receipt_unconfirmed"); });
    calls.set.mockResolvedValue({ send_to_parent: false, revision: 1 });
    await toggleRouting(s);
    expect(calls.set).toHaveBeenCalledWith(expect.anything(), false, 0);
    expect(calls.notify).toHaveBeenCalledWith("error", LATCH_UNCONFIRMED, true);
  });

  it("a remote child, a child without a durable ID or a desktop outside service mode uses only its host", async () => {
    calls.set.mockResolvedValue({ send_to_parent: false, revision: 1 });
    for (const s of [local({ key: "remote", computer: "box" }), child({ key: "no-id" }), local({ key: "no-service" })]) {
      if (s.key === "no-service") calls.enabled.mockReturnValue(false);
      await toggleRouting(s);
      expect(latchOf(s)).toBeUndefined();
    }
    expect(calls.invoke).not.toHaveBeenCalled();
    expect(calls.set).toHaveBeenCalledTimes(3);
  });
});
