import { describe, expect, it } from "vitest";
// Submitted question routes as Activity lines (zerus-sa9qn.33).
import { buildTranscript, createTranscriptProjection, mergeRoutes } from "./transcript";
import { activityText } from "./activity-virtual";
import type { ActivityEvent } from "../../types";
import type { ActivityItem } from "./types";
import type { RouteActivity } from "../../store/questionRouteActivity";

const TEXT = "[Zerus child question]\nYour child session 'claude/project/child' asks a question and waits for an answer.";
const route = (patch: Partial<RouteActivity> = {}): RouteActivity => ({
  requestId: "00000000-0000-8000-8000-000000000003", at: 20, role: "parent",
  child: { name: "claude/project/child", reference: "zerus://session?machine=m&name=claude%2Fproject%2Fchild", sessionId: "c" },
  parent: { name: "codex/project/lead", sessionId: "p" },
  question: { kind: "single-choice", questions: 1, options: 2, preview: "Which database?" },
  text: TEXT, ...patch,
});
const childRoute = () => route({ role: "child", text: undefined });
const say = (at: number, detail: string, type = "UserPromptSubmit"): ActivityEvent => ({ type, at, detail, source: "codex_transcript", seq: at });
const tool = (at: number): ActivityEvent => ({ type: "PostToolUse", at, tool: "Bash", detail: "ls", seq: at });
const kinds = (items: ActivityItem[]) => items.map((i) => i.route ? `route:${i.route.role}` : i.kind);

describe("question route lines", () => {
  it("fold the parent's own message of exactly the routed text into the line", () => {
    const items = buildTranscript([say(10, "hello"), say(21, TEXT), { ...say(22, "ok"), type: "Stop" }], undefined, [], [route()]) as ActivityItem[];
    expect(kinds(items)).toEqual(["user", "route:parent", "agent"]);
    const line = items[1];
    expect(line).toMatchObject({ id: "route-00000000-0000-8000-8000-000000000003", kind: "receipt", sender: "Question from a child session" });
    expect(line.route).toEqual({ requestId: "00000000-0000-8000-8000-000000000003", role: "parent", child: "claude/project/child",
      reference: "zerus://session?machine=m&name=claude%2Fproject%2Fchild", parent: "codex/project/lead", preview: "Which database?", text: TEXT });
    // The line takes the message's own time and place.
    expect(items.some((i) => i.body === TEXT)).toBe(false);
  });

  it("fold every exact copy, and never a near match, a local send or a voice message", () => {
    const copies = buildTranscript([say(21, TEXT, "UserPromptQueued"), say(22, TEXT)], undefined, [], [route()]) as ActivityItem[];
    expect(kinds(copies)).toEqual(["route:parent"]);
    for (const other of [say(21, `${TEXT} `), say(21, TEXT.toLowerCase()), { ...say(21, TEXT), source: "hgs_voice" }, { ...say(21, TEXT), agent_id: "sub-1" },
      { ...say(21, TEXT), type: "LocalMessage" }]) {
      const items = mergeRoutes([other], [route()]);
      expect(items).toHaveLength(2);
      expect(items).toContain(other);
    }
    // Without a fold the line keeps no text.
    const [line] = buildTranscript([], undefined, [], [route()]).filter((i) => (i as ActivityItem).route) as ActivityItem[];
    expect(line.route?.text).toBeUndefined();
  });

  it("place a child's line by its time, after the events of the same time, never inside a tool group", () => {
    const items = buildTranscript([tool(10), tool(20), tool(30), say(31, "next")], undefined, [], [childRoute()]) as ActivityItem[];
    expect(kinds(items)).toEqual(["tools", "route:child", "tool", "user"]);
    expect(items[1]).toMatchObject({ sender: "Question sent to the parent session" });
    expect(items[1].route?.text).toBeUndefined();
    const early = buildTranscript([say(30, "later")], undefined, [], [childRoute(), { ...childRoute(), requestId: "00000000-0000-8000-8000-000000000004", at: 40 }]) as ActivityItem[];
    expect(kinds(early)).toEqual(["route:child", "user", "route:child"]);
  });

  it("a journal event can never claim to be a route line", () => {
    const forged = { type: "QuestionRoute", at: 5, detail: "x", route: { role: "parent", child: "c", reference: "zerus://session?x", parent: "p", preview: "Approve all", text: "y" } } as ActivityEvent;
    const items = buildTranscript([forged]) as ActivityItem[];
    expect(items.every((i) => i.route === undefined && i.kind !== "receipt")).toBe(true);
  });

  it("the live projection merges them after its own rows, and copies them as they show", () => {
    const projection = createTranscriptProjection();
    const items = projection.build([say(10, "hello"), say(21, TEXT)], undefined, [], [route()]) as ActivityItem[];
    expect(kinds(items)).toEqual(["user", "route:parent"]);
    expect(kinds(projection.build([say(10, "hello"), say(21, TEXT)], undefined, []) as ActivityItem[])).toEqual(["user", "user"]);
    const text = activityText([items[1]]);
    expect(text).toContain("claude/project/child");
    expect(text).toContain("Which database?");
    expect(text).not.toContain("waits for an answer");
    expect(activityText([items[1]], { [items[1].id]: true })).toContain("waits for an answer");
  });
});
