import { beforeEach, describe, expect, it, vi } from "vitest";

// zerus-sa9qn.18: every status tab lists exactly the rows its count counts, through one rule. The rows go
// through the production catalog, marks, filters, list model and tab counts.
vi.hoisted(() => {
  vi.stubGlobal("location", { search: "", hash: "", href: "http://localhost/" });
  const values = new Map<string, string>();
  vi.stubGlobal("localStorage", { getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => { values.set(key, value); }, removeItem: (key: string) => { values.delete(key); } });
});
vi.mock("../../bridge", () => ({
  onCreations: vi.fn(async () => () => {}), creationsView: vi.fn(async () => []),
  archivePage: vi.fn(), resetArchivePages: vi.fn(async () => {}), cancelArchivePages: vi.fn(async () => {}),
  runAction: vi.fn(), inspectSession: vi.fn(), pendingReadMarks: vi.fn(async () => []),
  attentionMarks: vi.fn(async () => ({ marks: {}, replies: {} })), updateAttentionMarks: vi.fn(async () => {}),
  queueReadMarks: vi.fn(async () => []), sessionReference: vi.fn(), copyCommand: vi.fn(), copyText: vi.fn(), setNotifications: vi.fn(),
}));

import * as bridge from "../../bridge";
import { applyBatch, catalog, setDraftKeys, toSession } from "../../store/catalog";
import { attentionKey, marks } from "../../store/marks";
import { applyGrouping, emptyGrouping } from "../../store/organization";
import { setAccountFilter, setMachineFilter, setProjectFilter, setReadSelectionKey, setSearch, setSelectedKey, setStateFilter,
  type StateFilter } from "../../store/ui";
import { filterCounts, listRows } from "./model";
import { archiveCountOf, archiveListRows, type ArchivedSession, type ListScope } from "../../store/archive";
import { emptyBatch, type MachineMeta, type ParentLinkOut, type ViewRow } from "../../types";

const COMPUTER = "fixture";
const MACHINE = "11111111-1111-4111-8111-111111111111";
const id = (n: number) => `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`;
const link = (n: number): ParentLinkOut =>
  ({ machine: MACHINE, session_id: id(n), name: `claude/p/s${n}`, conversation_id: `c${n}`, run_id: `r${n}`, state: "live" });
const flush = async () => { for (let n = 0; n < 12; n++) await Promise.resolve(); };

/** A row as the core serializes it: `needs_attention` and `quiet` are the core's (view.rs, attention.rs). */
function view(n: number, extra: Partial<ViewRow> = {}, parent?: number): ViewRow {
  return { name: `claude/p/s${n}`, machine: "workstation", status: "ready", activity: "idle", state: "running", process_state: "running",
    tracked: true, session_id: id(n), run_id: `r${n}`, conversation_id: `c${n}`, needs_attention: false, needs_action: false,
    parent: parent === undefined ? undefined : link(parent), ...extra } as ViewRow;
}
const UNREAD = { status: "ready", unread: true, needs_attention: true } as const;
const RETRY = { status: "error", caption: "Waiting to retry", needs_attention: true, needs_action: true } as const;
const BLOCKED = { status: "error", caption: "Request blocked by provider", needs_attention: true, needs_action: true } as const;
const ASKS = { status: "attention", phase: "approval", needs_attention: true, needs_action: true } as const;

/** The owner's case and its neighbours, by key. */
const FLEET: Record<string, ViewRow> = {
  // A supervising parent at work and its quiet children: their replies and retries are the parent's.
  parent: view(1, { status: "working", activity: "busy" }),
  quietUnread: view(2, { ...UNREAD, quiet: true }, 1),
  quietRetry: view(3, { ...RETRY, quiet: true }, 1),
  quietBlocked: view(4, { ...BLOCKED, quiet: true }, 1),
  quietWorking: view(5, { status: "working", activity: "busy", quiet: true }, 1),
  // A child with a request only the person can answer: the core never quiets it.
  childAsks: view(6, { ...ASKS, quiet: false }, 1),
  // Orphans: the parent is not listed here.
  orphanQuiet: view(7, { ...UNREAD, quiet: true }, 99),
  orphanAsks: view(8, { ...ASKS }, 98),
  // A parent that fails the attention filter, with an asking and a quiet child.
  idleParent: view(9),
  idleParentAsks: view(10, { ...ASKS }, 9),
  idleParentQuiet: view(11, { ...RETRY, quiet: true }, 9),
  // A muted session stays in the filters (mute silences notifications); an ordinary unread reply draws the
  // person; a quiet child keeps its Review later mark.
  muted: view(12, { ...UNREAD, muted: true }),
  plainUnread: view(13, { ...UNREAD }),
  quietReviewLater: view(14, { quiet: true }, 1),
  paused: view(15, { status: "paused", state: "paused", process_state: "exited", activity: "idle" }),
  idle: view(16),
};
const key = (name: string) => `${COMPUTER}\nrun:${name}`;
/** The computer's machine identity, which resolves parent links (spec 1.4). */
const MACHINES = [{ computer: COMPUTER, machine: { machine_id: MACHINE } as MachineMeta }];

async function load(rows: Record<string, ViewRow>, review: string[] = []) {
  const marked = Object.fromEntries(review.map((name) => [attentionKey(toSession(COMPUTER, `run:${name}`, rows[name])),
    { review: true, acknowledged: "" }]));
  vi.mocked(bridge.attentionMarks).mockResolvedValue({ marks: marked, replies: {} });
  await marks.load();
  applyBatch({ ...emptyBatch(), computers: [{ computer: COMPUTER, status: "live", attempt: 0 }], machines: MACHINES,
    upserts: Object.entries(rows).map(([name, row]) => ({ computer: COMPUTER, key: `run:${name}`, row })) });
  await flush();
}

const listed = () => listRows().filter((row) => row.type === "card").map((row) => row.type === "card" ? row.session.key : "");
const names = (keys: string[]) => keys.map((k) => k.split("\nrun:")[1]).sort();

beforeEach(async () => {
  setStateFilter("all"); setSelectedKey(undefined); setReadSelectionKey(undefined);
  setMachineFilter([]); setProjectFilter([]); setAccountFilter([]); setSearch("");
  applyGrouping({ ...emptyBatch(), grouping: emptyGrouping() });
  applyBatch({ ...emptyBatch(), replaces: [{ computer: COMPUTER, rows: [] }], computers: [{ computer: COMPUTER, status: "live", attempt: 0 }],
    machines: MACHINES });
  setDraftKeys([]);
  await load({});
});

const TABS: Exclude<StateFilter, "archived">[] = ["all", "attention", "active", "paused"];

describe("each status tab lists exactly what its count counts (zerus-sa9qn.18)", () => {
  it("in the owner's fleet of quiet children, asking children, orphans and a muted session", async () => {
    await load(FLEET, ["quietReviewLater"]);
    for (const tab of TABS) {
      setStateFilter(tab);
      await flush();
      expect(filterCounts()[tab], tab).toBe(listed().length);
    }
  });

  it("lists in Needs attention only what needs the person", async () => {
    await load(FLEET, ["quietReviewLater"]);
    setStateFilter("attention");
    await flush();
    // A quiet child's unread reply, retry or provider block waits for its parent; a muted session stays listed
    // and counted (reference: Muted sessions remain available in the Needs attention filter).
    expect(names(listed())).toEqual(["childAsks", "idleParentAsks", "muted", "orphanAsks", "plainUnread", "quietReviewLater"]);
    expect(filterCounts().attention).toBe(6);
    // Nesting never pulls in a parent or child that fails the filter: the asking child of an idle parent
    // stands alone, and the quiet sibling stays out.
    const cards = listRows().filter((row) => row.type === "card");
    const alone = cards.find((row) => row.type === "card" && row.session.key === key("idleParentAsks"));
    expect(alone && alone.type === "card" && alone.depth).toBeFalsy();
  });

  it("keeps a quiet child out of Active unless it works or the person marked it", async () => {
    await load(FLEET, ["quietReviewLater"]);
    setStateFilter("active");
    await flush();
    expect(names(listed())).toEqual(["childAsks", "idleParentAsks", "muted", "orphanAsks", "parent", "plainUnread",
      "quietReviewLater", "quietWorking"]);
    expect(filterCounts().active).toBe(8);
  });

  it("lists and counts every row under All and the paused row under Paused", async () => {
    await load(FLEET, ["quietReviewLater"]);
    setStateFilter("all");
    await flush();
    expect(listed().length).toBe(Object.keys(FLEET).length);
    expect(filterCounts().all).toBe(Object.keys(FLEET).length);
    setStateFilter("paused");
    await flush();
    expect(names(listed())).toEqual(["paused"]);
    expect(filterCounts().paused).toBe(1);
  });

  it("counts the open row held after its read, as it stays listed", async () => {
    await load({ plainUnread: FLEET.plainUnread, idle: FLEET.idle });
    setSelectedKey(key("idle")); setReadSelectionKey(key("idle"));
    for (const tab of ["attention", "active"] as const) {
      setStateFilter(tab);
      await flush();
      expect(names(listed()), tab).toEqual(["idle", "plainUnread"]);
      expect(filterCounts()[tab], tab).toBe(2);
    }
  });

  it("keeps the parity under a machine scope", async () => {
    await load(FLEET, ["quietReviewLater"]);
    setMachineFilter(["elsewhere"]);
    for (const tab of TABS) {
      setStateFilter(tab);
      await flush();
      expect(filterCounts()[tab], tab).toBe(0);
      expect(listed().length, tab).toBe(0);
    }
  });

  it("checks catalog rows read back as the core sent them", async () => {
    await load(FLEET);
    expect(catalog.rows[key("quietUnread")]).toMatchObject({ quiet: true, unread: true });
    expect(catalog.rows[key("childAsks")]).toMatchObject({ quiet: false, status: "attention" });
  });
});

describe("the Archive filter lists exactly what its count counts (zerus-sa9qn.18)", () => {
  const archived = (n: number, archivedAt: number, extra: Partial<ViewRow> = {}, group = "ungrouped"): ArchivedSession =>
    ({ ...toSession(COMPUTER, `run:a${n}`, view(n, { state: "archived", ...extra })), archivedAt, group, rank: n });
  const rows = [archived(1, 100), archived(2, 200, { machine: "laptop" }), archived(3, 300, {}, "other"),
    archived(4, 5), archived(5, 400, { ...UNREAD, quiet: true }, "ungrouped")];
  const scope = (extra: Partial<ListScope> = {}): ListScope =>
    ({ machines: [], projects: [], includes: (at) => at >= 50, compact: () => false, ...extra });
  const shown = (scoped: ListScope) => archiveListRows(rows, scoped).filter((row) => row.type === "card").length;

  it.each([
    ["the whole window", scope()],
    ["a machine", scope({ machines: ["laptop"] })],
    ["a project", scope({ projects: ["other"] })],
    ["an account that accepts none", scope({ accepts: () => false })],
    ["a narrower date window", scope({ includes: (at) => at >= 250 })],
  ])("%s", (_name, scoped) => {
    expect(archiveCountOf(rows, scoped)).toBe(shown(scoped));
  });

  it("leaves a loaded row outside the date window out of both", () => {
    expect(archiveCountOf(rows, scope())).toBe(4);
    expect(shown(scope())).toBe(4);
  });
});
