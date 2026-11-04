import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.hoisted(() => {
  // store/ui.ts reads the page address and storage when it loads.
  const store = new Map<string, string>();
  vi.stubGlobal("location", { search: "", hash: "", href: "http://localhost/" });
  vi.stubGlobal("localStorage", { getItem: (k: string) => store.get(k) ?? null, setItem: (k: string, v: string) => { store.set(k, v); }, removeItem: (k: string) => { store.delete(k); } });
  // The dialogs module registers Solid's delegated events when it loads; nothing renders here.
  vi.stubGlobal("window", { document: { addEventListener: () => {} } });
});
vi.mock("../../store/endActions", () => ({ endActions: { schedule: async (_captures: unknown[], run: (refs: unknown[], key: string) => Promise<unknown>) => run([{ id: "test-delay", index: 0 }], "test-end") } }));
vi.mock("../../bridge", () => ({
  // The session entry points also initialize the bounded creation-card store.
  onCreations: vi.fn(async () => () => {}), creationsView: vi.fn(async () => []),
  archivePage: vi.fn(),
  resetArchivePages: vi.fn(async () => {}),
  cancelArchivePages: vi.fn(async () => {}),
  runAction: vi.fn(),
  inspectSession: vi.fn(async () => null),
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
import { applyBatch, catalog } from "../../store/catalog";
import { archive, archiveCount } from "../../store/archive";
import { notices } from "../../store/actions";
import { bindBatchConfirm } from "../../store/batch";
import { selected, selectedKey, setSelectedKey, setStateFilter, setProjectFilter, stateFilter } from "../../store/ui";
import { actionEntries, renameSession } from "./ActionDialogs";
import { snapshotOf } from "./actions-ui";
import { clearArchiveMenuEntry, selectionCount } from "./selection-live";
import type { BatchConfirmation } from "./selection";
import { emptyBatch, type ArchivePage, type ArchiveRow, type Outcome, type ViewRow } from "../../types";

// Restore session, Forget archived session and Clear archive through the
// app's stores: the archive pages, the guarded calls and the batch.

const mocked = vi.mocked(bridge);
const flush = async () => { for (let i = 0; i < 40; i++) await Promise.resolve(); };
const uuid = (n: number) => `20000000-0000-4000-8000-${String(n).padStart(12, "0")}`;
const done = (): Outcome => ({ outcome: "done", receipt: {}, cleanup: null });
const archived = (n: number, group = "ungrouped"): ArchiveRow => ({ group, rank: 0,
  name: `codex/example/r${n}`, agent: "codex", state: "archived", status: "ended", archive_id: uuid(n), archived_at: 100 + n,
  run_id: `run-r${n}`, conversation_id: `conv-r${n}`, resumable: true,
});
const key = (n: number) => `\narchive:${uuid(n)}`;
const texts = () => notices().map((n) => n.text);
const pages: ((p: ArchivePage) => void)[] = [];
const page = (...rows: ArchiveRow[]): ArchivePage => ({ rows, next_cursor: "", previous_cursor: null, total: rows.length, position: 0 });

async function loadArchive(...rows: ArchiveRow[]) {
  setStateFilter("archived");
  await flush();
  pages.splice(0).forEach((answer) => answer(page(...rows)));
  await flush();
}

let copies: BatchConfirmation[] = [];
let confirmAnswer = true;

beforeEach(async () => {
  vi.useFakeTimers();
  mocked.runAction.mockReset();
  mocked.archivePage.mockReset();
  mocked.archivePage.mockImplementation(() => new Promise((resolve) => { pages.push(resolve); }));
  copies = [];
  confirmAnswer = true;
  bindBatchConfirm(async (copy) => { copies.push(copy); return confirmAnswer; });
  setStateFilter("all");
  setProjectFilter([]);
  setSelectedKey(undefined);
  applyBatch({ ...emptyBatch(), computers: [{ computer: "", status: "live", attempt: 0 }], replaces: [{ computer: "", rows: [{ computer: "", key: "run:keep", row: { name: "codex/example/keep", agent: "codex", state: "paused", resumable: true, machine: "box", run_id: "run-keep", conversation_id: "conv-keep" } }] }] });
  await flush();
  archive.refresh();
  await flush();
});
afterEach(() => vi.useRealTimers());

describe("the archived row's menu entries", () => {
  it("offer Restore session and Forget archived session, Restore off while a live row has the name", async () => {
    await loadArchive(archived(1), archived(2));
    const one = archive.row(key(1))!;
    const menu = actionEntries(one, undefined, { context: true });
    expect([menu.change.label, menu.change.disabled, menu.change.hint]).toEqual(["Restore session", false, undefined]);
    expect([menu.end.label, menu.end.disabled, menu.end.hint]).toEqual(["Forget archived session…", false, undefined]);
    applyBatch({ ...emptyBatch(), upserts: [{ computer: "", key: "run:r1", row: { name: "codex/example/r1", agent: "codex", state: "paused", resumable: true, run_id: "run-x", conversation_id: "conv-x" } }] });
    expect(actionEntries(one, undefined, { context: true }).change.disabled).toBe(true);
    expect(actionEntries(archive.row(key(2))!, undefined, { context: true }).change.disabled).toBe(false);
  });

  it("restore from the menu: one call, the archive reloads, the filter becomes All and the selection follows the live row", async () => {
    await loadArchive(archived(3));
    setSelectedKey(key(3));
    mocked.runAction.mockResolvedValueOnce(done());
    const before = mocked.archivePage.mock.calls.length;
    actionEntries(archive.row(key(3))!, undefined, { context: true }).change.action!();
    await flush();
    expect(mocked.runAction).toHaveBeenCalledTimes(1);
    expect(mocked.runAction).toHaveBeenCalledWith(expect.objectContaining({ name: "codex/example/r3" }), { kind: "restore", archive_id: uuid(3) });
    expect(mocked.archivePage.mock.calls.length).toBe(before + 1);
    expect(stateFilter()).toBe("all");
    // The archived row stays selected while its live row is awaited.
    expect(selectedKey()).toBe(key(3));
    expect(selected()?.archiveId).toBe(uuid(3));
    applyBatch({ ...emptyBatch(), upserts: [{ computer: "", key: "run:r3", row: { name: "codex/example/r3", agent: "codex", state: "running", resumable: true, run_id: "run-new", conversation_id: "conv-r3" } }] });
    await flush();
    expect(selectedKey()).toBe("\nrun:r3");
    expect(selected()?.state).toBe("running");
  });
});

describe("a reloaded page that lists the archive id under another name", () => {
  it("makes the entries opened for the old name refuse at the click, with no call and no dialog", async () => {
    await loadArchive(archived(14));
    const menu = actionEntries(archive.row(key(14))!, undefined, { context: true });
    archive.refresh();
    await flush();
    pages.splice(0).forEach((answer) => answer(page({ ...archived(14), name: "codex/example/renamed" })));
    await flush();
    expect(archive.row(key(14))?.name).toBe("codex/example/renamed");
    const before = notices().length;
    menu.change.action!();
    await flush();
    menu.end.action!();
    await flush();
    expect(mocked.runAction).not.toHaveBeenCalled();
    expect(notices().slice(before).map((n) => n.text)).toEqual([
      "This session is no longer available. Open its actions again.",
      "This session is no longer available. Open its actions again.",
    ]);
  });
});

describe("the header and the context menu Restore", () => {
  it("restore a row whose operations leave out restore, as Qt's header and selected-row menu do", async () => {
    await loadArchive({ ...archived(12), allowed_actions: ["rename", "forget"] });
    const row = archive.row(key(12))!;
    expect(actionEntries(row, undefined, { context: true }).change.disabled).toBe(false);
    const header = actionEntries(row);
    expect([header.change.label, header.change.disabled]).toEqual(["Restore session", false]);
    mocked.runAction.mockResolvedValueOnce(done());
    header.change.action!();
    await flush();
    expect(mocked.runAction).toHaveBeenCalledTimes(1);
    expect(mocked.runAction).toHaveBeenCalledWith(expect.objectContaining({ name: "codex/example/r12" }), { kind: "restore", archive_id: uuid(12) });
  });
});

describe("Rename of a loaded archived row", () => {
  it("reloads the archive after a success and cancels a pending restore's follow", async () => {
    await loadArchive(archived(10), archived(11));
    setSelectedKey(key(10));
    let finishRestore: (o: Outcome) => void = () => {};
    mocked.runAction.mockImplementation((_target, action) => (action.kind === "restore" ? new Promise((r) => { finishRestore = r; }) : Promise.resolve(done())));
    actionEntries(archive.row(key(10))!, undefined, { context: true }).change.action!();
    await flush();
    const before = mocked.archivePage.mock.calls.length;
    const out = await renameSession(snapshotOf(archive.row(key(11))!), "codex/example/renamed");
    await flush();
    expect(out.outcome).toBe("done");
    expect(mocked.runAction).toHaveBeenLastCalledWith(expect.objectContaining({ name: "codex/example/r11" }), { kind: "rename", new_name: "codex/example/renamed", archive_id: uuid(11) });
    expect(mocked.archivePage.mock.calls.length).toBe(before + 1);
    // The restore is no longer followed: the filter stays, and the verified live row only clears the old selection.
    finishRestore(done());
    await flush();
    expect(stateFilter()).toBe("archived");
    applyBatch({ ...emptyBatch(), upserts: [{ computer: "", key: "run:r10", row: { name: "codex/example/r10", agent: "codex", state: "running", resumable: true, run_id: "run-new", conversation_id: "conv-r10" } }] });
    await flush();
    expect(selectedKey()).toBeUndefined();
  });

  it("does not reload the archive after a failure", async () => {
    await loadArchive(archived(13));
    mocked.runAction.mockResolvedValueOnce({ outcome: "failed", detail: "name in use" });
    const before = mocked.archivePage.mock.calls.length;
    await renameSession(snapshotOf(archive.row(key(13))!), "codex/example/other");
    await flush();
    expect(mocked.archivePage.mock.calls.length).toBe(before);
  });
});

describe("Move to archive", () => {
  it("reloads the archive after a success only (Qt writeDone 1917)", async () => {
    await loadArchive(archived(4));
    const kept = catalog.rows["\nrun:keep"]!;
    mocked.runAction.mockResolvedValueOnce({ outcome: "failed", detail: "no" }).mockResolvedValueOnce(done());
    const before = mocked.archivePage.mock.calls.length;
    actionEntries(kept, undefined, { context: true }).archive!.action!();
    await flush();
    expect(mocked.archivePage.mock.calls.length).toBe(before);
    actionEntries(kept, undefined, { context: true }).archive!.action!();
    await flush();
    expect(mocked.archivePage.mock.calls.length).toBe(before + 1);
  });
});

describe("the selection counts", () => {
  it("count archived Resume and Forget, Resume not for a name a live row has", async () => {
    await loadArchive(archived(5), archived(6));
    const rows = [archive.row(key(5))!, archive.row(key(6))!];
    expect(selectionCount(rows)("resume")).toBe(2);
    expect(selectionCount(rows)("forget")).toBe(2);
    applyBatch({ ...emptyBatch(), upserts: [{ computer: "", key: "run:r6", row: { name: "codex/example/r6", agent: "codex", state: "paused", resumable: true, run_id: "run-y", conversation_id: "conv-y" } }] });
    expect(selectionCount(rows)("resume")).toBe(1);
    expect(selectionCount(rows)("forget")).toBe(2);
  });
});

describe("Clear archive (Qt SessionSelection.cpp 171-181, 226-235)", () => {
  it("forgets the loaded archived rows with Qt's confirmation, then reloads the archive once", async () => {
    await loadArchive(archived(7), archived(8));
    setStateFilter("all");
    const entry = clearArchiveMenuEntry();
    expect(entry).toMatchObject({ label: "Remove loaded archived sessions…", danger: true, disabled: false });
    mocked.runAction.mockResolvedValue(done());
    if ("action" in entry) entry.action!();
    await flush();
    expect(copies).toHaveLength(1);
    expect(copies[0]).toMatchObject({ title: "Clear archive", confirm: "Clear archive", details: ["box / codex/example/r8", "box / codex/example/r7"] });
    expect(copies[0].text).toBe("Remove 2 loaded archived sessions in the current date and machine scope? Unloaded entries are kept.\n\nOnly these saved or archived entries are removed. Live sessions and native conversation history are kept.");
    expect(mocked.runAction.mock.calls.map((c) => c[1])).toEqual([{ kind: "forget_archive", archive_id: uuid(8) }, { kind: "forget_archive", archive_id: uuid(7) }]);
    expect(texts()).toContain("Forget: 2 of 2 completed.");
    // Leaving the Archive filter kept the pages; the batch's end drops them for a reload.
    expect(archive.rows()).toEqual([]);
  });

  it("asks and does nothing on Cancel; is off with nothing to forget", async () => {
    await loadArchive(archived(9));
    confirmAnswer = false;
    const entry = clearArchiveMenuEntry();
    if ("action" in entry) entry.action!();
    await flush();
    expect(copies).toHaveLength(1);
    expect(mocked.runAction).not.toHaveBeenCalled();
    expect(archive.rows().map((r) => r.archiveId)).toEqual([uuid(9)]);
    archive.refresh();
    await flush();
    pages.splice(0).forEach((answer) => answer(page()));
    await flush();
    expect(clearArchiveMenuEntry()).toMatchObject({ disabled: true });
  });
});

it("Clear archive under a project filter confirms only that project's loaded identities and resets after the batch", async () => {
  setProjectFilter(["alpha"]);
  await loadArchive(archived(20, "alpha"), archived(21, "beta"));
  const resets = mocked.resetArchivePages.mock.calls.length;
  expect(archiveCount()).toBe("1");
  const entry = clearArchiveMenuEntry();
  expect(entry).toMatchObject({ disabled: false });
  mocked.runAction.mockResolvedValue(done());
  if ("action" in entry) entry.action!();
  await flush();
  expect(copies[0].details).toEqual(["box / codex/example/r20"]);
  expect(mocked.runAction.mock.calls.map((c) => c[1])).toEqual([{ kind: "forget_archive", archive_id: uuid(20) }]);
  expect(mocked.resetArchivePages.mock.calls.length).toBeGreaterThan(resets);
  expect(archive.rows()).toEqual([]);
});

it("Cancel in filtered Clear archive keeps the rows and their backend placement", async () => {
  setProjectFilter(["alpha"]);
  await loadArchive(archived(22, "alpha"), archived(23, "beta"));
  const resets = mocked.resetArchivePages.mock.calls.length;
  confirmAnswer = false;
  const entry = clearArchiveMenuEntry();
  if ("action" in entry) entry.action!();
  await flush();
  expect(mocked.runAction).not.toHaveBeenCalled();
  expect(mocked.resetArchivePages.mock.calls.length).toBe(resets);
  expect(archive.rows()).toHaveLength(2);
});
