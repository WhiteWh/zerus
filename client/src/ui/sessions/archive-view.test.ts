import { beforeEach, describe, expect, it, vi } from "vitest";

vi.hoisted(() => {
  // store/ui.ts reads the page address and storage when it loads.
  const store = new Map<string, string>();
  vi.stubGlobal("location", { search: "", hash: "", href: "http://localhost/" });
  vi.stubGlobal("localStorage", { getItem: (k: string) => store.get(k) ?? null, setItem: (k: string, v: string) => { store.set(k, v); }, removeItem: (k: string) => { store.delete(k); } });
});
vi.mock("../../bridge", () => ({
  // The session entry points also initialize the bounded creation-card store.
  onCreations: vi.fn(async () => () => {}), creationsView: vi.fn(async () => []),
  archivePage: vi.fn(),
  resetArchivePages: vi.fn(async () => {}),
  cancelArchivePages: vi.fn(async () => {}),
  runAction: vi.fn(),
  inspectSession: vi.fn(),
  pendingReadMarks: vi.fn(async () => []),
  attentionMarks: vi.fn(async () => ({ marks: {}, replies: {} })),
  updateAttentionMarks: vi.fn(async () => {}),
  queueReadMarks: vi.fn(async () => []),
  sessionReference: vi.fn(),
  copyCommand: vi.fn(),
  copyText: vi.fn(),
  setNotifications: vi.fn(),
}));

import * as bridge from "../../bridge";
import { applyBatch } from "../../store/catalog";
import { archive, archiveCount, archiveTip } from "../../store/archive";
import { stillListed } from "../../store/actions";
import { counts, matchesFilter, selected, setMachineFilter, setProjectFilter, setSearch, setSelectedKey, setStateFilter } from "../../store/ui";
import { catalog } from "../../store/catalog";
import { applyGrouping, emptyGrouping } from "../../store/organization";
import { applies } from "../../store/batch";
import { listRows, machineChoices, projectChoices } from "./model";
import { markEntry } from "./mark-entries";
import { snapshotOf } from "./actions-ui";
import { emptyBatch, type ArchivePage, type ArchiveRow, type ViewRow } from "../../types";

const mocked = vi.mocked(bridge);
const flush = async () => { for (let i = 0; i < 10; i++) await Promise.resolve(); };
const uuid = (n: number) => `10000000-0000-4000-8000-${String(n).padStart(12, "0")}`;
const archived = (n: number, at: number): ArchiveRow => ({ group: "ungrouped", rank: 0, name: `codex/example/a${n}`, agent: "codex", state: "archived", status: "ended", archive_id: uuid(n), archived_at: at, run_id: `run-a${n}`, conversation_id: `conv-a${n}` });
const live: ViewRow = { name: "codex/example/live", agent: "codex", state: "running", status: "ready", machine: "workstation", run_id: "run-live", conversation_id: "conv-live" };

describe("the window's attention in the app", () => {
  it("follows the computer's stream: an unreachable peer counts only Review later (SessionPresentation.h:281)", () => {
    const asking: ViewRow = { name: "claude/p/ask", agent: "claude", status: "attention", phase: "approval", state: "running",
      needs_action: true, needs_attention: true, child_attention: false };
    applyBatch({ ...emptyBatch(), computers: [{ computer: "attention-peer", status: "live", attempt: 0 }],
      replaces: [{ computer: "attention-peer", rows: [{ computer: "attention-peer", key: "run:ask", row: asking }] }] });
    const row = () => catalog.rows["attention-peer\nrun:ask"];
    const before = counts().attention;
    expect(matchesFilter(row(), "attention")).toBe(true);
    applyBatch({ ...emptyBatch(), computers: [{ computer: "attention-peer", status: "reconnecting", attempt: 1 }] });
    expect(matchesFilter(row(), "attention")).toBe(false);
    expect(counts().attention).toBe(before - 1);
    expect(matchesFilter({ ...row(), reviewLater: true }, "attention")).toBe(true);
    applyBatch({ ...emptyBatch(), removes: [{ computer: "attention-peer", key: "run:ask" }],
      computers: [{ computer: "attention-peer", status: "ended", attempt: 0 }] });
    expect(row()).toBeUndefined();
  });
});

describe("the window's online state in the app", () => {
  it("is the Overview's: a computer still connecting counts as Qt's unreachable box in the list, the batch and Mark as read (SessionsWindow.cpp:3207-3213)", () => {
    const asking: ViewRow = { name: "claude/p/wait", agent: "claude", status: "attention", phase: "approval", state: "running",
      needs_action: true, needs_attention: true, child_attention: false };
    applyBatch({ ...emptyBatch(), computers: [{ computer: "connecting-peer", status: "probing", attempt: 0 }],
      replaces: [{ computer: "connecting-peer", rows: [{ computer: "connecting-peer", key: "run:wait", row: asking }] }] });
    const row = () => catalog.rows["connecting-peer\nrun:wait"];
    const before = counts().attention;
    expect(matchesFilter(row(), "attention")).toBe(false);
    expect(applies(row(), "mark_read", true, false)).toBe(false);
    expect(markEntry(row())?.label).toBe("Mark needs attention");
    applyBatch({ ...emptyBatch(), computers: [{ computer: "connecting-peer", status: "live", attempt: 0 }] });
    expect(matchesFilter(row(), "attention")).toBe(true);
    expect(counts().attention).toBe(before + 1);
    expect(applies(row(), "mark_read", true, false)).toBe(true);
    expect(markEntry(row())?.label).toBe("Mark as read");
    for (const status of ["unsupported", "polled"]) {
      applyBatch({ ...emptyBatch(), computers: [{ computer: "connecting-peer", status, attempt: 0 }] });
      expect(matchesFilter(row(), "attention")).toBe(status === "polled");
    }
    applyBatch({ ...emptyBatch(), removes: [{ computer: "connecting-peer", key: "run:wait" }],
      computers: [{ computer: "connecting-peer", status: "ended", attempt: 0 }] });
  });
});

describe("the Archive view in the app", () => {
  const pending: ((p: ArchivePage) => void)[] = [];
  beforeEach(() => {
    vi.useFakeTimers();
    mocked.archivePage.mockImplementation(() => new Promise((resolve) => { pending.push(resolve); }));
  });

  it("pages only under the Archive filter, lists archived cards newest first, and finds them for the selection and the actions", async () => {
    applyBatch({ ...emptyBatch(), computers: [{ computer: "", status: "live", attempt: 0 }], upserts: [{ computer: "", key: "run:run-live", row: live }] });
    expect(mocked.archivePage).not.toHaveBeenCalled();
    // No organization is presented: one flat list without a project header.
    expect(listRows().map((r) => r.type)).toEqual(["card"]);
    expect(archiveCount()).toBe("…");

    setStateFilter("archived");
    expect(mocked.archivePage).toHaveBeenCalledWith("", 0, 253402300800, "", "", expect.any(Number));
    expect(listRows()).toEqual([]);
    pending.shift()!({ rows: [archived(1, 100), archived(2, 200)], next_cursor: "next", previous_cursor: null, total: 3, position: 0 });
    await flush();

    const rows = listRows();
    expect(rows.map((r) => r.type === "card" && [r.session.name, r.session.machine])).toEqual([["codex/example/a2", "workstation"], ["codex/example/a1", "workstation"]]);
    expect(archiveCount()).toBe("2+");
    expect(archiveTip()).toBe("Archive / 2 loaded; more can be loaded in Archive");
    expect(mocked.resetArchivePages).toHaveBeenCalled();
    // The app numbers its requests from its start time, so a reload never reuses a cancelled number.
    expect(mocked.archivePage.mock.calls[0][5]).toBeGreaterThan(1_700_000_000_000);
    const key = `\narchive:${uuid(2)}`;
    expect(rows[0].id).toBe(key);
    if (rows[0].type === "card") expect(matchesFilter(rows[0].session, "archived")).toBe(true);

    setSelectedKey(key);
    expect(selected()?.archiveId).toBe(uuid(2));
    expect(stillListed(snapshotOf(selected()!))?.key).toBe(key);

    // The search is the archive query: the list is not filtered by text, the pages reload after 300 ms.
    setSearch("a1");
    expect(listRows()).toEqual([]);
    vi.advanceTimersByTime(300);
    expect(mocked.archivePage).toHaveBeenLastCalledWith("", 0, 253402300800, "", "a1", expect.any(Number));
    setSearch("");
    vi.advanceTimersByTime(300);
    pending.splice(0).forEach((answer) => answer({ rows: [archived(1, 100)], next_cursor: "", previous_cursor: null, total: 1, position: 0 }));
    await flush();
    expect(listRows()).toHaveLength(1);

    // A project change resets both sides before re-reading the window.
    setProjectFilter(["p-aurora"]);
    expect(listRows()).toHaveLength(0);
    expect(mocked.resetArchivePages).toHaveBeenCalled();
    setProjectFilter([]);

    // A machine outside the filter pages nothing.
    const calls = mocked.archivePage.mock.calls.length;
    setMachineFilter(["build-box"]);
    expect(listRows()).toEqual([]);
    expect(mocked.archivePage.mock.calls.length).toBe(calls);
    expect(archiveCount()).toBe("0");
    setMachineFilter([]);

    setStateFilter("all");
    expect(listRows().map((r) => r.type)).toEqual(["card"]);
  });
});

it("replays grouped Archive rows through the existing headers, newest first within each project", async () => {
  mocked.archivePage.mockImplementation(() => new Promise(() => {}));
  applyBatch({ ...emptyBatch(), computers: [{ computer: "idle", status: "reconnecting", attempt: 1 }] });
  setStateFilter("archived");
  const request = mocked.archivePage.mock.calls.at(-1)![5];
  const headers = ["beta", "alpha"].map((id, palette) => ({ id, name: id.toUpperCase(), color: null, palette, accessible: true, collapsed: false, is_default: false }));
  const grouping = { ...emptyGrouping(), loaded: true, ready: true, default_project: "alpha", headers };
  applyGrouping({ ...emptyBatch(), grouping });
  // Complete the actual app store's current read via its mock's saved resolver.
  // A fresh Refresh gives this replay its own reply and request tag.
  let answer!: (page: ArchivePage) => void;
  mocked.archivePage.mockImplementation(() => new Promise((resolve) => { answer = resolve; }));
  archive.refresh();
  const current = mocked.archivePage.mock.calls.at(-1)![5];
  archive.applyGroups([{ computer: "", key: `archive:${uuid(10)}`, group: "beta", rank: 99, archive_request: current }]);
  answer({ rows: [
    { ...archived(10, 400), group: "alpha", rank: 1 },
    { ...archived(11, 300), group: "beta", rank: 0 },
    { ...archived(12, 200), group: "alpha", rank: 0 },
  ], total: 3, position: 0, next_cursor: "", previous_cursor: null });
  await flush();
  expect(listRows().map((r) => r.id)).toEqual(["h:beta", `\narchive:${uuid(10)}`, `\narchive:${uuid(11)}`, "h:alpha", `\narchive:${uuid(12)}`]);
  expect(projectChoices()).toEqual([{ id: "beta", label: "BETA", count: 2 }, { id: "alpha", label: "ALPHA", count: 1 }]);
  expect(machineChoices()).toContainEqual({ id: "idle", label: "idle", computer: "idle", count: 0 });
  archive.applyGroups([{ computer: "", key: `archive:${uuid(11)}`, group: "alpha", rank: 50, archive_request: current }]);
  expect(listRows().map((r) => r.id)).toEqual(["h:beta", `\narchive:${uuid(10)}`, "h:alpha", `\narchive:${uuid(11)}`, `\narchive:${uuid(12)}`]);
  archive.applyGroups([{ computer: "", key: `archive:${uuid(10)}`, group: "alpha", rank: 0, archive_request: request }]);
  expect(listRows()[0].id).toBe("h:beta");
  applyGrouping({ ...emptyBatch(), grouping: { ...grouping, headers: [{ ...headers[0], collapsed: true }, headers[1]] } });
  expect(listRows().map((r) => r.id)).toEqual(["h:beta", "h:alpha", `\narchive:${uuid(11)}`, `\narchive:${uuid(12)}`]);
  setStateFilter("all");
  applyBatch({ ...emptyBatch(), computers: [{ computer: "idle", status: "ended", attempt: 0 }] });
  applyGrouping({ ...emptyBatch(), grouping: emptyGrouping() });
});
