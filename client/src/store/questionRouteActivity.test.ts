import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
// The read-only Activity lines of submitted question routes (zerus-sa9qn.33);
// the native read is a fixture.
const calls = vi.hoisted(() => ({ invoke: vi.fn(), enabled: vi.fn(() => true) }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: calls.invoke }));
vi.mock("./desktopService", () => ({ desktopService: { enabled: calls.enabled } }));
import { ROUTE_FOLLOW_MS, ROUTE_LIMIT, ROUTE_REFRESH_MS, parseRouteActivity, refreshRouteActivity, routeActivityOf, routeSessionOf } from "./questionRouteActivity";

const CHILD = "00000000-0000-4000-8000-000000000001";
const PARENT = "00000000-0000-4000-8000-000000000002";
const OTHER = "00000000-0000-4000-8000-000000000009";

export function childLine(): Record<string, unknown> {
  return {
    request_id: "00000000-0000-8000-8000-000000000003", at: 5_000, role: "child",
    child: { name: "claude/project/child", reference: "zerus://session?machine=m&name=claude%2Fproject%2Fchild", session_id: CHILD },
    parent: { name: "codex/project/lead", session_id: PARENT },
    question: { kind: "single-choice", questions: 1, options: 2, preview: "Which database?" },
  };
}
const parentLine = () => ({ ...childLine(), role: "parent", text: "[Zerus child question]\nYour child session asks." });

describe("the route lines' exact shape", () => {
  it("keeps every line of an exact reply, in seconds, and only the parent's text", () => {
    const [child] = parseRouteActivity({ routes: [childLine()] }, CHILD);
    expect(child).toEqual({
      requestId: "00000000-0000-8000-8000-000000000003", at: 5, role: "child",
      child: { name: "claude/project/child", reference: "zerus://session?machine=m&name=claude%2Fproject%2Fchild", sessionId: CHILD },
      parent: { name: "codex/project/lead", sessionId: PARENT },
      question: { kind: "single-choice", questions: 1, options: 2, preview: "Which database?" },
    });
    expect(parseRouteActivity({ routes: [parentLine()] }, PARENT)[0].text).toBe(parentLine().text);
    // A parent line's text is the route's own, present and bounded.
    for (const text of ["", "x".repeat(2049), 5, null]) expect(parseRouteActivity({ routes: [{ ...parentLine(), text }] }, PARENT)).toEqual([]);
    expect(parseRouteActivity({ routes: Array(ROUTE_LIMIT).fill(childLine()) }, CHILD)).toHaveLength(ROUTE_LIMIT);
    expect(parseRouteActivity({ routes: [] }, CHILD)).toEqual([]);
  });

  it("shows nothing unless every line is exactly a line about this session", () => {
    const changes: [string, (r: Record<string, any>) => unknown][] = [
      ["another session's line", (r) => { r.routes[0].child.session_id = OTHER; }],
      ["a child line with text", (r) => { r.routes[0].text = "x"; }],
      ["a parent line without text", (r) => { r.routes[0].role = "parent"; r.routes[0].parent.session_id = CHILD; r.routes[0].child.session_id = OTHER; }],
      ["another role", (r) => { r.routes[0].role = "peer"; }],
      ["an extra field", (r) => { r.routes[0].question.option_labels = ["Postgres"]; }],
      ["an extra reply key", (r) => { r.code = ""; }],
      ["a long preview", (r) => { r.routes[0].question.preview = "x".repeat(301); }],
      ["a two-line preview", (r) => { r.routes[0].question.preview = "a\nb"; }],
      ["another kind", (r) => { r.routes[0].question.kind = "approval"; }],
      ["no questions", (r) => { r.routes[0].question.questions = 0; }],
      ["a fractional count", (r) => { r.routes[0].question.options = 1.5; }],
      ["another reference", (r) => { r.routes[0].child.reference = "https://example.invalid/"; }],
      ["a non-canonical id", (r) => { r.routes[0].request_id = r.routes[0].request_id.toUpperCase().replace("8000", "800A"); }],
      ["no time", (r) => { r.routes[0].at = 0; }],
      ["the same child and parent", (r) => { r.routes[0].parent.session_id = CHILD; }],
      ["too many", (r) => { r.routes = Array(ROUTE_LIMIT + 1).fill(childLine()); }],
      ["not a list", (r) => { r.routes = {}; }],
    ];
    for (const [name, change] of changes) {
      const reply: Record<string, any> = { routes: [childLine(), childLine()] };
      reply.routes = reply.routes.map((r: unknown) => JSON.parse(JSON.stringify(r)));
      change(reply);
      expect(parseRouteActivity(reply, CHILD), name).toEqual([]);
    }
    expect(parseRouteActivity({ routes: [childLine()] }, PARENT)).toEqual([]);
    expect(parseRouteActivity(undefined, CHILD)).toEqual([]);
  });
});

describe("reading the shown session's lines", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    calls.invoke.mockReset().mockResolvedValue({ routes: [childLine()] });
    calls.enabled.mockReturnValue(true);
    refreshRouteActivity(undefined);
  });
  afterEach(() => { refreshRouteActivity(undefined); vi.useRealTimers(); });
  const flush = () => vi.advanceTimersByTimeAsync(0);

  it("reads a newly shown local session at once, then at most once per window", async () => {
    refreshRouteActivity(CHILD);
    await flush();
    expect(calls.invoke).toHaveBeenCalledTimes(1);
    expect(calls.invoke).toHaveBeenCalledWith("desktop_service_question_route_activity", { session_id: CHILD });
    expect(routeActivityOf(CHILD)).toHaveLength(1);
    expect(routeActivityOf(PARENT)).toEqual([]);
    // Activity changes within the window read once, at its end.
    for (let i = 0; i < 5; i++) refreshRouteActivity(CHILD);
    await flush();
    expect(calls.invoke).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(ROUTE_REFRESH_MS);
    expect(calls.invoke).toHaveBeenCalledTimes(2);
    // Without a change or a child to follow, nothing more.
    await vi.advanceTimersByTimeAsync(ROUTE_FOLLOW_MS * 2);
    expect(calls.invoke).toHaveBeenCalledTimes(2);
  });

  it("follows a shown child slowly and stops when nothing is shown", async () => {
    refreshRouteActivity(CHILD, true);
    await flush();
    expect(calls.invoke).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(ROUTE_FOLLOW_MS);
    expect(calls.invoke).toHaveBeenCalledTimes(2);
    refreshRouteActivity(undefined);
    expect(routeActivityOf(CHILD)).toEqual([]);
    await vi.advanceTimersByTimeAsync(ROUTE_FOLLOW_MS * 3);
    expect(calls.invoke).toHaveBeenCalledTimes(2);
  });

  it("shows another session's lines only after its own read", async () => {
    refreshRouteActivity(CHILD);
    await flush();
    calls.invoke.mockResolvedValue({ routes: [parentLine()] });
    refreshRouteActivity(PARENT);
    expect(routeActivityOf(CHILD)).toEqual([]);
    expect(routeActivityOf(PARENT)).toEqual([]);
    await flush();
    expect(routeActivityOf(PARENT)).toHaveLength(1);
  });

  it("reads nothing for a remote row, a row without a durable ID or outside service mode", async () => {
    for (const id of ["", "claude/project/child", CHILD.toUpperCase().replace("4000", "400A")]) refreshRouteActivity(id);
    refreshRouteActivity(undefined);
    calls.enabled.mockReturnValue(false);
    refreshRouteActivity(CHILD);
    await vi.advanceTimersByTimeAsync(ROUTE_FOLLOW_MS);
    expect(calls.invoke).not.toHaveBeenCalled();
    expect(routeActivityOf(CHILD)).toEqual([]);
  });

  it("a refused or older service shows no lines, quietly", async () => {
    calls.invoke.mockRejectedValue("unsupported_operation");
    refreshRouteActivity(CHILD);
    await flush();
    expect(routeActivityOf(CHILD)).toEqual([]);
    calls.invoke.mockResolvedValue({ routes: [childLine()], code: "" });
    await vi.advanceTimersByTimeAsync(ROUTE_REFRESH_MS);
    refreshRouteActivity(CHILD);
    await vi.advanceTimersByTimeAsync(ROUTE_REFRESH_MS);
    expect(routeActivityOf(CHILD)).toEqual([]);
  });
});

describe("which session's lines the Activity shows", () => {
  const row = { computer: "", key: "\nrun:r1", sessionId: CHILD };
  const live = { archive: false, subagent: false, target: "\nrun:r1" };
  it("only the live main Activity of a local session the follow targets", () => {
    expect(routeSessionOf(row, live)).toBe(CHILD);
    expect(routeSessionOf({ ...row, computer: "box", key: "box\nrun:r1" }, { ...live, target: "box\nrun:r1" })).toBeUndefined();
    expect(routeSessionOf(row, { ...live, archive: true })).toBeUndefined();
    expect(routeSessionOf(row, { ...live, subagent: true })).toBeUndefined();
    expect(routeSessionOf(row, { ...live, target: "\nrun:other" })).toBeUndefined();
    expect(routeSessionOf(row, { ...live, target: undefined })).toBeUndefined();
    expect(routeSessionOf(undefined, live)).toBeUndefined();
    expect(routeSessionOf({ computer: "", key: "\nrun:r1" }, live)).toBeUndefined();
  });
});
