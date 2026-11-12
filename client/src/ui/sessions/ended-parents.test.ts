import { describe, expect, it, vi } from "vitest";
import { toSession } from "../../store/catalog";
import { groupRows, parentIndex, rowHeight, summarize, ROW_HEIGHT, type ListInput, type Row } from "./model";
import type { GroupingOut, ParentLinkOut, ViewRow } from "../../types";
import type { Session } from "./types";

// zerus-sa9qn.17: an ended parent with live child sessions stays in the live
// list as their container. The list model's module reads the page's query
// (store/ui.ts); the tests run without a page.
vi.hoisted(() => { vi.stubGlobal("location", new URL("http://localhost/")); });

const MACHINE = "11111111-1111-4111-8111-111111111111";
const OTHER = "22222222-2222-4222-8222-222222222222";
const id = (n: number) => `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`;
const link = (n: number, extra: Partial<ParentLinkOut> = {}): ParentLinkOut =>
  ({ machine: MACHINE, session_id: id(n), name: `kimi/p/s${n}`, conversation_id: `c${n}`, run_id: `r${n}`, state: "live", ...extra });
const session = (n: number, parent?: number | ParentLinkOut, extra: Partial<ViewRow> = {}): Session =>
  toSession("", `run:${n}`, { name: `kimi/p/s${n}`, machine: "workstation", status: "ready", session_id: id(n),
    conversation_id: `c${n}`, parent: parent === undefined ? undefined : typeof parent === "number" ? link(parent) : parent, ...extra });
const header = (hid: string) => ({ id: hid, name: hid, color: null, accessible: true, collapsed: false, is_default: hid === "ungrouped", palette: 0 });
const grouping = (headers: ReturnType<typeof header>[]): GroupingOut => ({
  loaded: true, default_project: "ungrouped", headers, ready: true, reason: null, busy: false, status: "", status_error: false, conflicts: 0,
});
/** The live list of one project `alpha` with `members` in rank order; `visible` stands for the filters. */
const rows = (members: Session[], extra: Partial<ListInput> = {}) => groupRows({
  grouping: grouping([header("alpha")]), members: new Map([["alpha", members]]), visible: () => true, online: () => true, projects: [],
  expanded: () => false, compact: () => false, parentKey: parentIndex(members, () => MACHINE), machineOf: () => MACHINE,
  containers: true, ...extra,
});
type Card = Extract<Row, { type: "card" }>;
type Box = Extract<Row, { type: "container" }>;
const shape = (out: Row[]) => out.flatMap((r) => r.type === "card" ? [`${"  ".repeat(r.depth ?? 0)}${r.session.name.slice(-2)}`]
  : r.type === "container" ? [`${"  ".repeat(r.depth ?? 0)}[${r.container.title} ${r.container.state}]`] : []);
const boxes = (out: Row[]) => out.filter((r): r is Box => r.type === "container");
const cards = (out: Row[]) => out.filter((r): r is Card => r.type === "card");

describe("ended parents keep their live child sessions (zerus-sa9qn.17)", () => {
  it("nests the children of a terminated, archived parent under an Archived container instead of the top level", () => {
    const archived = link(1, { state: "archived" });
    const out = rows([session(2, archived), session(3), session(4, archived, { status: "working" })]);
    expect(shape(out)).toEqual(["[s1 archived]", "  s2", "  s4", "s3"]);
    const [box] = boxes(out);
    expect(box.container).toMatchObject({ name: "kimi/p/s1", title: "s1", agent: "kimi", state: "archived", machine: "workstation" });
    expect(box.container.key).toBeUndefined();
    expect(box.team).toMatchObject({ total: 2, working: 1, collapsed: false });
    // Nested, so no "from PARENT" chip; the container itself names no parent.
    expect(cards(out).map((c) => c.fromParent)).toEqual([undefined, undefined, undefined]);
    expect(cards(out)[0]).toMatchObject({ depth: 1, parentKey: box.id.slice("container:".length), lastChild: false });
    expect(rowHeight(box)).toBe(ROW_HEIGHT.collapsedTeam);
  });

  it("keeps a parent gone from the child's own computer as an Ended container, but never one on another computer", () => {
    const gone = link(1, { state: "missing" });
    expect(shape(rows([session(2, gone)]))).toEqual(["[s1 ended]", "  s2"]);
    // A link forwarded from another computer is always missing there: not ended.
    const elsewhere = rows([session(2, link(1, { state: "missing", machine: OTHER }))]);
    expect(shape(elsewhere)).toEqual(["s2"]);
    expect(cards(elsewhere)[0].fromParent).toEqual({ name: "kimi/p/s1", key: undefined });
    // A live parent that this list does not show is not ended either.
    expect(shape(rows([session(2, 1)]))).toEqual(["s2"]);
  });

  it("reveals a stopped or paused parent that the filters hide as an Ended container of its listed row", () => {
    for (const state of ["stopped", "paused"]) {
      const parent = session(1, undefined, { state }), child = session(2, 1, { status: "working" });
      const out = rows([parent, child], { visible: (s) => s.key !== parent.key });
      // A listed parent keeps its own title, as its card would show it.
      expect(shape(out)).toEqual([`[${parent.title} ended]`, "  s2"]);
      expect(boxes(out)[0].container.key).toBe(parent.key);
      expect(boxes(out)[0].id).toBe(`container:${parent.key}`);
    }
    // A running parent that the filters hide keeps today's chip: it has not ended.
    const running = session(1), child = session(2, 1);
    const chip = rows([running, child], { visible: (s) => s.key !== running.key });
    expect(shape(chip)).toEqual(["s2"]);
    expect(cards(chip)[0].fromParent).toEqual({ name: "kimi/p/s1", key: running.key });
  });

  it("leaves a stopped parent that the filters show as its ordinary row", () => {
    const out = rows([session(1, undefined, { state: "stopped" }), session(2, 1)]);
    expect(shape(out)).toEqual(["s1", "  s2"]);
    expect(boxes(out)).toEqual([]);
  });

  it("leaves the live list once no live child remains", () => {
    const archived = link(1, { state: "archived" });
    expect(boxes(rows([session(2, archived), session(3, archived)]))).toHaveLength(1);
    // The last live child archived too: it is no longer a catalog row.
    expect(boxes(rows([session(3)]))).toEqual([]);
    // A hidden stopped parent whose only child is archived is hidden again.
    const parent = session(1, undefined, { state: "stopped" });
    expect(shape(rows([parent], { visible: () => false }))).toEqual([]);
  });

  it("is shown only for children the filters show", () => {
    const archived = link(1, { state: "archived" });
    const kept = session(2, archived), filtered = session(3, archived);
    const out = rows([kept, filtered], { visible: (s) => s.key === kept.key });
    expect(shape(out)).toEqual(["[s1 archived]", "  s2"]);
    expect(boxes(out)[0].team?.total).toBe(1);
    expect(shape(rows([kept, filtered], { visible: () => false }))).toEqual([]);
  });

  it("nests ended chains several levels deep: an archived root, a hidden stopped parent, its live children", () => {
    const root = link(1, { state: "archived" });
    const middle = session(2, root, { state: "stopped" }), leaf = session(3, 2, { status: "working" }), deeper = session(4, 3);
    const out = rows([middle, leaf, deeper], { visible: (s) => s.key !== middle.key });
    expect(shape(out)).toEqual(["[s1 archived]", `  [${middle.title} ended]`, "    s3", "      s4"]);
    expect(boxes(out)[0].team).toMatchObject({ total: 3, working: 1 });
  });

  it("never loops or hides a row in an ended parent cycle", () => {
    const a = session(1, 2, { state: "stopped" }), b = session(2, 1, { state: "stopped" }), child = session(3, 1);
    const out = rows([a, b, child], { visible: (s) => s.key === child.key });
    const shown = shape(out).map((s) => s.trim());
    expect(shown.filter((s) => s === "s3")).toHaveLength(1);
    expect(shown).toContain(`[${a.title} ended]`);
    expect(shown.length).toBeLessThanOrEqual(3);
  });

  it("places one container per project group, before the first child it nests", () => {
    const archived = link(1, { state: "archived" });
    const a = session(2, archived), b = session(3, archived), plain = session(4);
    const out = groupRows({
      grouping: grouping([header("alpha"), header("beta")]), members: new Map([["alpha", [plain, a]], ["beta", [b]]]), visible: () => true,
      online: () => true, projects: [], expanded: () => false, compact: () => false, parentKey: parentIndex([plain, a, b], () => MACHINE),
      machineOf: () => MACHINE, containers: true,
    });
    expect(out.map((r) => r.type === "header" ? r.name : shape([r])[0])).toEqual(["alpha", "s4", "[s1 archived]", "  s2", "beta", "[s1 archived]", "  s3"]);
    expect(boxes(out).map((r) => r.project)).toEqual(["alpha", "beta"]);
    // The header counts its sessions only, never a container.
    expect(out.filter((r) => r.type === "header").map((r) => r.type === "header" && r.total)).toEqual([2, 1]);
  });

  it("is never a session: the footer counts, archive rows and the collapsed team leave it out", () => {
    const archived = link(1, { state: "archived" });
    const out = rows([session(2, archived, { status: "working" })]);
    expect(summarize(out, () => true)).toMatchObject({ shown: 1, working: 1 });
    // Archive rows and older callers do not ask for containers.
    expect(boxes(rows([session(2, archived)], { containers: false }))).toEqual([]);
    const key = boxes(out)[0].id.slice("container:".length);
    const shut = rows([session(2, archived)], { teamCollapsed: (k) => k === key });
    expect(shape(shut)).toEqual(["[s1 archived]"]);
    expect(boxes(shut)[0].team?.collapsed).toBe(true);
  });

  it("resolves 20,000 children of one ended parent in one pass", () => {
    const archived = link(1, { state: "archived" });
    const many = Array.from({ length: 20000 }, (_, i) => session(i + 2, archived));
    const started = performance.now();
    const out = rows(many);
    expect(performance.now() - started).toBeLessThan(3000);
    expect(boxes(out)).toHaveLength(1);
    expect(cards(out)).toHaveLength(20000);
    expect(boxes(out)[0].team?.total).toBe(20000);
  });
});

describe("the live list wires containers in (zerus-sa9qn.17)", () => {
  it("lists a terminated parent's container from the real catalog, with its computer's machine for a forgotten one", async () => {
    const { createRoot } = await import("solid-js");
    const { applyBatch } = await import("../../store/catalog");
    const { applyMachines } = await import("../../store/machines");
    const { listRows } = await import("./model");
    const { emptyBatch } = await import("../../types");
    const { rowOut } = await import("../../store/testing");
    applyMachines({ ...emptyBatch(), machines: [{ computer: "", machine: { machine_id: MACHINE } as never }] });
    const child = (n: number, state: string) => rowOut("", n, { name: `kimi/p/s${n}`, session_id: id(n), conversation_id: `c${n}`,
      parent: link(n * 10, { state }) });
    applyBatch({ ...emptyBatch(), replaces: [{ computer: "", rows: [child(1, "archived"), child(2, "missing"), child(3, "live")] }] });
    createRoot((dispose) => {
      const out = listRows();
      expect(boxes(out).map((r) => `${r.container.title} ${r.container.state}`)).toEqual(["s10 archived", "s20 ended"]);
      // A live parent that is not listed keeps today's chip.
      expect(cards(out).find((r) => r.session.name === "kimi/p/s3")?.fromParent?.name).toBe("kimi/p/s30");
      dispose();
    });
  });
});
