import { beforeEach, describe, expect, it, vi } from "vitest";
import * as organization from "../../store/organization";
import { applyBatch, catalog, rowId } from "../../store/catalog";
import { rowOut } from "../../store/testing";
import { emptyBatch, type GroupingOut } from "../../types";
import { applyDrop, placement } from "./organize";
import type { Session } from "./types";
import type { Row } from "./model";

// The drop half of organize.ts against a mocked organization: which edit a
// drop sends, and where `placement` reads a row's project.

// store/ui.ts reads the page's query; the tests run without a page.
vi.hoisted(() => { vi.stubGlobal("location", new URL("http://localhost/")); });

const state = vi.hoisted(() => ({
  /** Row id -> project, as the Rust side assigns them. */
  assigned: {} as Record<string, string>,
  listed: ["alpha", "beta", "ungrouped"],
  loaded: true,
  archived: {} as Record<string, Session>,
}));

vi.mock("../../store/organization", () => ({
  INBOX: "ungrouped",
  grouping: vi.fn((): Partial<GroupingOut> => ({ loaded: state.loaded, default_project: "beta" })),
  groupOf: vi.fn((id: string) => state.assigned[id] ?? "beta"),
  headerOf: vi.fn((id: string) => (state.listed.includes(id) ? { id } : undefined)),
  moveProject: vi.fn(() => Promise.resolve({ outcome: "done", conflicts: 0 })),
  moveToProject: vi.fn(() => Promise.resolve({ outcome: "done", conflicts: 0 })),
}));
vi.mock("../../store/archive", () => ({ archive: { row: (key: string) => state.archived[key] } }));
// The bulk-selection half reads the list; the drops do not.
vi.mock("./model", () => ({ listRows: () => [] }));

const mocked = vi.mocked(organization);
const A = rowId("", "run:1"), B = rowId("", "run:2"), C = rowId("", "run:3");
// Every row's own `project` is the folder's; only the assignments place it.
applyBatch({ ...emptyBatch(), upserts: [rowOut("", 1), rowOut("", 2), rowOut("", 3)] });
// Identified sessions on one computer, which drag to nest accepts (zerus-sa9qn.9).
const identity = (i: number) => ({ session_id: `00000000-0000-4000-8000-00000000000${i}`, run_id: `run-${i}`, conversation_id: `conv-${i}` });
applyBatch({ ...emptyBatch(), upserts: [rowOut("", 7, identity(7)), rowOut("", 8, identity(8)), rowOut("peer", 9, identity(9))] });
const N7 = rowId("", "run:7"), N8 = rowId("", "run:8"), N9 = rowId("peer", "run:9");

const header = (project: string): Row =>
  ({ type: "header", id: `h:${project}`, project, name: project, color: null, palette: 0, collapsed: false, total: 0, attention: 0, unread: 0, working: 0 });
const card = (id: string): Row => ({ type: "card", id, session: catalog.rows[id], expandable: false, expanded: false, compactRow: false });
/** Rows at the list's fixed heights: header 40, card 94. */
const layout = (rows: Row[]) => {
  const heights = rows.map((r) => (r.type === "header" ? 40 : 94));
  let y = 0;
  return { rows, heights, offsets: heights.map((h) => { const t = y; y += h; return t; }) };
};

beforeEach(() => {
  mocked.moveProject.mockClear();
  mocked.moveToProject.mockClear();
  state.assigned = { [A]: "alpha", [B]: "alpha", [C]: "beta" };
  state.listed = ["alpha", "beta", "ungrouped"];
  state.loaded = true;
  state.archived = {};
});

describe("project drops", () => {
  it("moves a project before another", () => {
    applyDrop({ kind: "group", id: "alpha" }, { group: "gamma", before: "beta" });
    expect(mocked.moveProject).toHaveBeenCalledTimes(1);
    expect(mocked.moveProject).toHaveBeenCalledWith("alpha", "beta");
  });

  it("moves a project to the end", () => {
    applyDrop({ kind: "group", id: "alpha" }, { group: "", line: 300, inset: 8 });
    expect(mocked.moveProject).toHaveBeenCalledWith("alpha", undefined);
  });

  it("makes no call for a drop before the dragged item itself", () => {
    applyDrop({ kind: "group", id: "alpha" }, { group: "alpha", before: "alpha" });
    applyDrop({ kind: "session", id: A }, { group: "alpha", before: A });
    expect(mocked.moveProject).not.toHaveBeenCalled();
    expect(mocked.moveToProject).not.toHaveBeenCalled();
  });
});

describe("session drops", () => {
  it("moves a session inside its own project before a row, or to its end", () => {
    applyDrop({ kind: "session", id: A }, { group: "alpha", before: B });
    expect(mocked.moveToProject).toHaveBeenLastCalledWith([catalog.rows[A]], "alpha", catalog.rows[B]);
    applyDrop({ kind: "session", id: B }, { group: "alpha" });
    expect(mocked.moveToProject).toHaveBeenLastCalledWith([catalog.rows[B]], "alpha", undefined);
    expect(mocked.moveToProject).toHaveBeenCalledTimes(2);
  });

  it("moves a session into another project", () => {
    applyDrop({ kind: "session", id: A }, { group: "beta", before: C });
    expect(mocked.moveToProject).toHaveBeenCalledWith([catalog.rows[A]], "beta", catalog.rows[C]);
    applyDrop({ kind: "session", id: C }, { group: "alpha", box: { top: 6, height: 30 } });
    expect(mocked.moveToProject).toHaveBeenLastCalledWith([catalog.rows[C]], "alpha", undefined);
  });

  it("ignores a project that is not listed, as Qt's moveSession does", () => {
    state.listed = ["alpha", "beta"];
    applyDrop({ kind: "session", id: A }, { group: "ungrouped" });
    expect(mocked.moveToProject).not.toHaveBeenCalled();
    // Before the first catalog nothing is listed yet: the store answers with its reason.
    state.loaded = false;
    applyDrop({ kind: "session", id: A }, { group: "ungrouped" });
    expect(mocked.moveToProject).toHaveBeenCalledTimes(1);
  });
});

describe("placement", () => {
  const { rows, offsets, heights } = layout([header("alpha"), card(A), card(C), header("beta"), card(B)]);
  const session = { kind: "session" as const, id: A };

  it("takes a card's project from its assignment, not from the row's folder project", () => {
    expect(catalog.rows[A].project).toBe("sample-project");
    // The upper half of the card at 40..134 places before it, in its assigned project.
    expect(placement(rows, offsets, heights, C, 140, session)).toMatchObject({ group: "beta", before: C });
    state.assigned[C] = "gamma";
    expect(placement(rows, offsets, heights, C, 140, session)).toMatchObject({ group: "gamma", before: C });
  });

  it("puts the next card as the target only when it shares the project", () => {
    // The lower half of A (40..134): C follows, but C is in beta, so A's project ends there.
    expect(placement(rows, offsets, heights, A, 120, session)).toMatchObject({ group: "alpha", before: undefined });
    state.assigned[C] = "alpha";
    expect(placement(rows, offsets, heights, A, 120, session)).toMatchObject({ group: "alpha", before: C });
  });

  it("moves a session dropped on an ended parent's container into its project, never before it (zerus-sa9qn.17)", () => {
    const box: Row = { type: "container", id: "container:ended:\nm\ns1", project: "alpha",
      container: { name: "kimi/p/s1", title: "s1", agent: "kimi", computer: "", machine: "workstation", state: "archived" } };
    const boxed = layout([header("alpha"), box, card(C)]);
    for (const y of [45, 120]) {
      const drop = placement(boxed.rows, boxed.offsets, boxed.heights, box.id, y, session);
      expect(drop).toMatchObject({ group: "alpha" });
      expect(drop.before).toBeUndefined();
      expect(drop.nest).toBeUndefined();
    }
  });

  it("sends a drop on the blank area to the Inbox's end, not the default project", () => {
    const blank = placement(rows, offsets, heights, undefined, 900, session);
    expect(blank).toMatchObject({ group: "ungrouped" });
    expect(blank.before).toBeUndefined();
    expect(placement(rows, offsets, heights, undefined, 900, { kind: "group", id: "alpha" })).toMatchObject({ group: "" });
  });

  it("places a project before the next header from the lower half of its own", () => {
    const group = { kind: "group" as const, id: "alpha" };
    expect(placement(rows, offsets, heights, "h:alpha", 5, group)).toMatchObject({ before: "alpha" });
    expect(placement(rows, offsets, heights, "h:alpha", 35, group)).toMatchObject({ before: "beta" });
  });
});

it("drops loaded archive rows before archive identities and ignores an archive row no longer loaded", () => {
  const a = { ...catalog.rows[A], key: "\narchive:a", catalogKey: "archive:a", state: "archived", archiveId: "a" } as Session;
  const b = { ...catalog.rows[B], key: "\narchive:b", catalogKey: "archive:b", state: "archived", archiveId: "b" } as Session;
  state.archived = { [a.key]: a, [b.key]: b };
  applyDrop({ kind: "session", id: a.key }, { group: "beta", before: b.key });
  expect(mocked.moveToProject).toHaveBeenLastCalledWith([a], "beta", b);
  mocked.moveToProject.mockClear();
  delete state.archived[a.key];
  applyDrop({ kind: "session", id: a.key }, { group: "beta" });
  expect(mocked.moveToProject).not.toHaveBeenCalled();
});

describe("drag to nest (zerus-sa9qn.9)", () => {
  const { rows, offsets, heights } = layout([header("alpha"), card(N7), card(N8), card(N9)]);
  const dragging = { kind: "session" as const, id: N7 };

  it("frames an identified card on the same computer when the pointer is over its middle", () => {
    // N8 spans 134..228: its middle band (about 162..200) nests, the rest still reorders.
    expect(placement(rows, offsets, heights, N8, 181, dragging)).toMatchObject({ nest: N8, box: { top: 138, height: 84 } });
    expect(placement(rows, offsets, heights, N8, 140, dragging)).toMatchObject({ before: N8 });
    expect(placement(rows, offsets, heights, N8, 220, dragging).nest).toBeUndefined();
  });

  it("never nests onto itself, another computer or an unidentified session", () => {
    expect(placement(rows, offsets, heights, N7, 87, dragging).nest).toBeUndefined();
    expect(placement(rows, offsets, heights, N9, 275, dragging).nest).toBeUndefined();
    const plain = layout([header("alpha"), card(A), card(C)]);
    expect(placement(plain.rows, plain.offsets, plain.heights, C, 181, { kind: "session", id: A }).nest).toBeUndefined();
  });
});

describe("the card a Shift key extends the range to (SessionList.cpp:589-602)", () => {
  const keys = ["a", "b", "c"];
  it("steps from the range's far end, else from the open conversation", async () => {
    const { rangeTarget } = await import("./organize");
    expect(rangeTarget(keys, "b", "a", "ArrowDown")).toBe(2);
    expect(rangeTarget(keys, "b", "a", "ArrowUp")).toBe(0);
    expect(rangeTarget(keys, undefined, "b", "ArrowDown")).toBe(2);
    expect(rangeTarget(keys, "gone", "a", "ArrowDown"), "a far end no longer listed").toBe(1);
  });
  it("goes to the first or last card for Home and End, and nowhere past either end", async () => {
    const { rangeTarget } = await import("./organize");
    expect(rangeTarget(keys, "b", "b", "Home")).toBe(0);
    expect(rangeTarget(keys, "b", "b", "End")).toBe(2);
    expect(rangeTarget(keys, "c", "c", "ArrowDown")).toBeUndefined();
    expect(rangeTarget(keys, "a", "a", "ArrowUp")).toBeUndefined();
    expect(rangeTarget(keys, undefined, undefined, "ArrowDown"), "nothing open: the first card").toBe(0);
    expect(rangeTarget(keys, undefined, undefined, "ArrowUp"), "nothing open: the last card").toBe(2);
    expect(rangeTarget([], "a", "a", "End")).toBeUndefined();
  });
});
