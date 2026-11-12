import { describe, expect, it, vi } from "vitest";
import { toSession } from "../../store/catalog";
import { childChoiceText, childOffer, childStayText, liveDescendants, CHILD_DEPTH } from "./end-children";
import { parentIndex } from "./model";
import { selectionSnapshot } from "./selection";
import type { ParentLinkOut, ViewRow } from "../../types";
import type { Session } from "./types";

// zerus-sa9qn.17: the live child sessions a parent's Terminate or Archive
// confirmation names. The list model's module reads the page's query.
vi.hoisted(() => { vi.stubGlobal("location", new URL("http://localhost/")); });

const MACHINE = "11111111-1111-4111-8111-111111111111";
const OTHER = "22222222-2222-4222-8222-222222222222";
const id = (n: number) => `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`;
const link = (n: number, extra: Partial<ParentLinkOut> = {}): ParentLinkOut =>
  ({ machine: MACHINE, session_id: id(n), name: `kimi/p/s${n}`, conversation_id: `c${n}`, run_id: `r${n}`, state: "live", ...extra });
const session = (n: number, parent?: number | ParentLinkOut, extra: Partial<ViewRow> = {}, computer = ""): Session =>
  toSession(computer, `run:${n}`, { name: `kimi/p/s${n}`, machine: "workstation", state: "running", tracked: true, run_id: `r${n}`,
    session_id: id(n), conversation_id: `c${n}`, parent: parent === undefined ? undefined : typeof parent === "number" ? link(parent) : parent, ...extra });
const machines: Record<string, string> = { "": MACHINE, peer: OTHER };
const names = (rows: readonly Session[]) => rows.map((s) => s.name.slice("kimi/p/".length));
const walk = (parent: Session, rows: Session[]) => liveDescendants(parent, rows, parentIndex(rows, (c) => machines[c]));

describe("a parent's live descendants (zerus-sa9qn.17)", () => {
  it("lists every child before any grandchild, on the parent's own computer only", () => {
    const parent = session(1);
    const rows = [parent, session(2, 1), session(3, 2), session(4, 1, { state: "stopped" }), session(5), session(6, 5),
      // Archived rows are no longer live children; a session on another computer is never listed.
      session(7, 1, { state: "archived" }), session(8, 1, {}, "peer")];
    expect(names(walk(parent, rows))).toEqual(["s2", "s4", "s3"]);
  });

  it("matches a parent listed without a session ID by its conversation, as the list nests", () => {
    const parent = session(1, undefined, { session_id: undefined });
    const child = session(2, link(1, { session_id: "" }));
    expect(names(walk(parent, [parent, child]))).toEqual(["s2"]);
  });

  it("never loops on a cycle and never lists the parent itself", () => {
    const a = session(1, 2), b = session(2, 1), c = session(3, 2);
    expect(names(walk(a, [a, b, c]))).toEqual(["s2", "s3"]);
    const self = session(4, 4);
    expect(walk(self, [self])).toEqual([]);
  });

  it(`stops after ${CHILD_DEPTH} levels`, () => {
    const chain = Array.from({ length: 20 }, (_, i) => session(i + 1, i ? i : undefined));
    const found = walk(chain[0], chain);
    expect(found).toHaveLength(CHILD_DEPTH);
    expect(found.at(-1)?.name).toBe(`kimi/p/s${CHILD_DEPTH + 1}`);
  });

  it("walks 20,000 children in one bounded pass", () => {
    const parent = session(1);
    const rows = [parent, ...Array.from({ length: 20000 }, (_, i) => session(i + 2, 1))];
    const started = performance.now();
    expect(walk(parent, rows)).toHaveLength(20000);
    expect(performance.now() - started).toBeLessThan(3000);
  });

  it("splits the children by whether the same action applies to them now", () => {
    const rows = [session(2, 1), session(3, 1, { state: "stopped" })].map(selectionSnapshot);
    const offer = childOffer(rows, (s) => s.state === "running");
    expect(names(offer.ending as Session[])).toEqual(["s2"]);
    expect(names(offer.staying as Session[])).toEqual(["s3"]);
  });

  it("words the opt-in and the children left as they are by count", () => {
    expect(childChoiceText("terminate", 1)).toBe("Also end 1 child session");
    expect(childChoiceText("terminate", 3)).toBe("Also end 3 child sessions");
    expect(childChoiceText("archive", 2)).toBe("Also archive 2 child sessions");
    expect(childStayText("archive", 1)).toBe("1 child session cannot be archived now and stays as it is:");
    expect(childStayText("terminate", 4)).toBe("4 child sessions cannot be terminated now and stay as they are:");
  });
});

describe("the confirmations read the live catalog (zerus-sa9qn.17)", () => {
  it("offer the parent's live descendants on its computer, split by whether the action applies now", async () => {
    const { createRoot } = await import("solid-js");
    const { applyBatch } = await import("../../store/catalog");
    const { applyMachines } = await import("../../store/machines");
    const { childOfferFor } = await import("../../store/endChildren");
    const { snapshotOf } = await import("./actions-ui");
    const { emptyBatch } = await import("../../types");
    const { rowOut } = await import("../../store/testing");
    applyMachines({ ...emptyBatch(), machines: [{ computer: "", machine: { machine_id: MACHINE } as never }, { computer: "peer", machine: { machine_id: OTHER } as never }] });
    const row = (n: number, parent: number | undefined, extra: Partial<ViewRow> = {}, computer = "") => rowOut(computer, n, {
      name: `kimi/p/s${n}`, agent: "kimi", tracked: true, run_id: `r${n}`, session_id: id(n), conversation_id: `c${n}`,
      parent: parent ? link(parent) : undefined, ...extra });
    applyBatch({ ...emptyBatch(), computers: [{ computer: "", status: "live", attempt: 0 }, { computer: "peer", status: "live", attempt: 0 }], replaces: [
      { computer: "", rows: [row(1, undefined), row(2, 1), row(3, 2, { state: "stopped" }), row(4, 3), row(5, 1, { state: "archived" }), row(6, undefined)] },
      { computer: "peer", rows: [row(7, 1, {}, "peer")] },
    ] });
    createRoot((dispose) => {
      const parent = snapshotOf(toSession("", "run:1", { name: "kimi/p/s1", run_id: "r1", session_id: id(1), conversation_id: "c1", state: "running", tracked: true, agent: "kimi" }));
      const terminate = childOfferFor(parent, "terminate");
      expect(names(terminate.ending as Session[])).toEqual(["s2", "s4"]);
      expect(names(terminate.staying as Session[])).toEqual(["s3"]);
      const archive = childOfferFor(parent, "archive");
      expect(names(archive.ending as Session[])).toEqual(["s3"]);
      expect(names(archive.staying as Session[])).toEqual(["s2", "s4"]);
      // An unlisted parent names nobody.
      expect(childOfferFor({ ...parent, key: "\nrun:gone" }, "terminate")).toEqual({ ending: [], staying: [] });
      dispose();
    });
  });
});
