import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.hoisted(() => {
  // store/ui.ts reads the page address and storage when it loads.
  const store = new Map<string, string>();
  vi.stubGlobal("location", { search: "", hash: "", href: "http://localhost/" });
  vi.stubGlobal("localStorage", { getItem: (k: string) => store.get(k) ?? null, setItem: (k: string, v: string) => { store.set(k, v); }, removeItem: (k: string) => { store.delete(k); } });
});
vi.mock("../bridge", () => ({
  archivePage: vi.fn(() => new Promise(() => {})),
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

import { createRoot, createSignal } from "solid-js";
import * as bridge from "../bridge";
import { createCatalog } from "./catalog";
import { createActions, withArchived } from "./actions";
import { archivedSession } from "./archive";
import { createRestores, NO_LONGER_AVAILABLE, RESTORE_LIMIT, RESTORES_PENDING, RESTORE_UNVERIFIED } from "./restores";
import type { StateFilter } from "./ui";
import { snapshotOf } from "../ui/sessions/actions-ui";
import type { Session } from "../ui/sessions/types";
import { emptyBatch, type Outcome, type ViewRow } from "../types";

// Restore session, its follow-up and Forget archived session, after Qt's
// changeSession (SessionsWindow.cpp 5542-5560), writeDone (1915-1972), the
// restore follow-up (3281-3336), terminateSession (5758-5826) and
// runNextSelectionAction's restore limit (SessionSelection.cpp 333-343).

const mocked = vi.mocked(bridge);
const flush = async () => { for (let i = 0; i < 10; i++) await Promise.resolve(); };
const done = (): Outcome => ({ outcome: "done", receipt: {}, cleanup: null });
const failed = (detail: string): Outcome => ({ outcome: "failed", detail });
const uuid = (n: number) => `10000000-0000-4000-8000-${String(n).padStart(12, "0")}`;
const archivedView = (n: number, extra: Partial<ViewRow> = {}): ViewRow => ({
  name: `codex/example/a${n}`, agent: "codex", state: "archived", status: "ended", archive_id: uuid(n), archived_at: 100 + n,
  run_id: `run-a${n}`, conversation_id: `conv-a${n}`, resumable: true, created: 50, ...extra,
});
const liveView = (name: string, extra: Partial<ViewRow> = {}): ViewRow => ({
  name, agent: "codex", state: "running", status: "ready", resumable: true, activity: "idle", process_state: "running", tracked: true,
  run_id: "run-new", conversation_id: "conv-a1", machine: "box", ...extra,
});

/** Calls that answer only when the test says so, in order. */
function deferredCalls() {
  const pending: ((o: Outcome) => void)[] = [];
  mocked.runAction.mockImplementation(() => new Promise<Outcome>((resolve) => { pending.push(resolve); }));
  return { answer: async (o: Outcome) => { pending.shift()!(o); await flush(); }, waiting: () => pending.length };
}

function setup(options: { online?: boolean; perform?: (...args: Parameters<ReturnType<typeof createActions>["perform"]>) => Promise<Outcome> } = {}) {
  const catalog = createCatalog();
  const archived = new Map<string, Session>();
  const page = (...rows: ViewRow[]) => { for (const r of rows) { const s = archivedSession("", "box", r); archived.set(s.key, s); } };
  const online = () => options.online ?? true;
  const actions = createActions(withArchived(catalog.catalog, (key) => archived.get(key)), online);
  const [selectedKey, setSelectedKey] = createSignal<string | undefined>();
  const [stateFilter, setStateFilter] = createSignal<StateFilter>("archived");
  const [search, setSearch] = createSignal("");
  const [batchRunning, setBatchRunning] = createSignal(false);
  const refreshArchive = vi.fn();
  const r = createRestores({
    catalog: catalog.catalog,
    archived: (key) => archived.get(key),
    online,
    updating: actions.updating,
    batchRunning,
    perform: options.perform ?? actions.perform,
    notify: actions.notify,
    stillListed: actions.stillListed,
    refreshArchive,
    selectedKey, setSelectedKey, stateFilter, setStateFilter, search,
  });
  const live = (name: string, extra: Partial<ViewRow> = {}, key = `run:${name}`) => catalog.applyBatch({ ...emptyBatch(), upserts: [{ computer: "", key, row: liveView(name, extra) }] });
  const row = (n: number) => snapshotOf(archived.get(`\narchive:${uuid(n)}`)!);
  const texts = () => actions.notices().map((n) => `${n.tone}:${n.text}`);
  return { ...r, catalog, archived, page, live, row, texts, refreshArchive, selectedKey, setSelectedKey, stateFilter, setStateFilter, setSearch, setBatchRunning, actions };
}

beforeEach(() => {
  vi.useFakeTimers();
  mocked.runAction.mockReset();
});
afterEach(() => vi.useRealTimers());

describe("Restore session (Qt changeSession 5542-5560)", () => {
  it("restores the archived instance once, refreshes the archive and switches the Archive filter to All for the selected row", async () => {
    await createRoot(async (dispose) => {
      const a = setup();
      a.page(archivedView(1));
      a.setSelectedKey(a.row(1).key);
      mocked.runAction.mockResolvedValueOnce(done());
      await a.restore(a.row(1));
      expect(mocked.runAction).toHaveBeenCalledTimes(1);
      expect(mocked.runAction).toHaveBeenCalledWith({ computer: "", name: "codex/example/a1", run_id: "run-a1", conversation_id: "conv-a1" }, { kind: "restore", archive_id: uuid(1) });
      expect(a.refreshArchive).toHaveBeenCalledTimes(1);
      expect(a.stateFilter()).toBe("all");
      expect(a.texts()).toEqual(["info:Session updated. Refreshing…"]);
      // The selection stays on the archived row until the live row appears.
      expect(a.selectedKey()).toBe(a.row(1).key);
      dispose();
    });
  });

  it("keeps other scopes and an unselected row's filter", async () => {
    await createRoot(async (dispose) => {
      const a = setup();
      a.page(archivedView(1), archivedView(2));
      mocked.runAction.mockResolvedValue(done());
      a.setSelectedKey(a.row(2).key);
      await a.restore(a.row(1));
      expect(a.stateFilter()).toBe("archived");
      a.setSelectedKey(a.row(2).key);
      a.setStateFilter("paused");
      await a.restore(a.row(2));
      expect(a.stateFilter()).toBe("paused");
      dispose();
    });
  });

  it("does nothing but Qt's notice when the row changed or is gone since the menu", async () => {
    await createRoot(async (dispose) => {
      const a = setup();
      a.page(archivedView(1));
      const shown = a.row(1);
      a.page(archivedView(1, { created: 77 }));
      await a.restore(shown);
      a.archived.clear();
      await a.restore(shown);
      expect(mocked.runAction).not.toHaveBeenCalled();
      expect(a.texts()).toEqual([
        "info:This session is no longer available. Open its actions again.",
        "info:This session is no longer available. Open its actions again.",
      ]);
      dispose();
    });
  });

  it("refuses a reloaded row with the same archive id and another name, as a vanished row (Qt's key holds the name)", async () => {
    await createRoot(async (dispose) => {
      const a = setup();
      a.page(archivedView(1));
      const shown = a.row(1);
      a.page(archivedView(1, { name: "codex/example/renamed" }));
      await a.restore(shown);
      expect(mocked.runAction).not.toHaveBeenCalled();
      expect(a.pendingCount()).toBe(0);
      expect(a.texts()).toEqual(["info:This session is no longer available. Open its actions again."]);
      dispose();
    });
  });

  it("returns silently when the entry is no longer enabled (offline, a live row took the name, no archive id, a batch runs)", async () => {
    await createRoot(async (dispose) => {
      const off = setup({ online: false });
      off.page(archivedView(1));
      await off.restore(off.row(1));
      const a = setup();
      a.page(archivedView(1), archivedView(2, { archive_id: "" }));
      a.live("codex/example/a1", { state: "paused" });
      await a.restore(a.row(1));
      await a.restore(snapshotOf(a.archived.get("\narchive:")!));
      a.setBatchRunning(true);
      a.catalog.applyBatch({ ...emptyBatch(), removes: [{ computer: "", key: "run:codex/example/a1" }] });
      await a.restore(a.row(1));
      expect(mocked.runAction).not.toHaveBeenCalled();
      expect([...off.texts(), ...a.texts()]).toEqual([]);
      dispose();
    });
  });

  it("refuses with the sticky notice while the same session or another single restore is pending", async () => {
    await createRoot(async (dispose) => {
      const a = setup();
      a.page(archivedView(1), archivedView(2));
      const calls = deferredCalls();
      const first = a.restore(a.row(1));
      await flush();
      // Another single restore while the first write runs (Qt's m_restoreWriteKey).
      await a.restore(a.row(2));
      expect(calls.waiting()).toBe(1);
      expect(a.texts()).toEqual([`error:${RESTORES_PENDING}`]);
      await calls.answer(done());
      await first;
      // The same session restored and still waiting for its live row.
      a.page(archivedView(1, { archive_id: uuid(9) }));
      await a.restore(snapshotOf(a.archived.get(`\narchive:${uuid(9)}`)!));
      expect(mocked.runAction).toHaveBeenCalledTimes(1);
      expect(a.texts().filter((t) => t === `error:${RESTORES_PENDING}`)).toHaveLength(2);
      expect(RESTORES_PENDING).toBe("Finish or refresh the earlier restores before restoring another session.");
      dispose();
    });
  });

  it("refuses once 64 restores are pending", async () => {
    await createRoot(async (dispose) => {
      const a = setup();
      mocked.runAction.mockResolvedValue(done());
      for (let n = 1; n <= RESTORE_LIMIT + 1; n++) a.page(archivedView(n));
      for (let n = 1; n <= RESTORE_LIMIT; n++) await a.restore(a.row(n));
      expect(mocked.runAction).toHaveBeenCalledTimes(RESTORE_LIMIT);
      expect(a.pendingCount()).toBe(RESTORE_LIMIT);
      await a.restore(a.row(RESTORE_LIMIT + 1));
      expect(mocked.runAction).toHaveBeenCalledTimes(RESTORE_LIMIT);
      expect(a.texts().at(-1)).toBe(`error:${RESTORES_PENDING}`);
      dispose();
    });
  });

  it("shows a thrown restore as the sticky failure a failed outcome shows", async () => {
    await createRoot(async (dispose) => {
      const a = setup({ perform: () => Promise.reject(new Error("bridge gone")) });
      a.page(archivedView(1));
      a.setSelectedKey(a.row(1).key);
      await a.restore(a.row(1));
      expect(a.texts()).toEqual(["error:bridge gone"]);
      expect(a.actions.notices()[0].sticky).toBe(true);
      expect(a.pendingCount()).toBe(0);
      expect(a.refreshArchive).not.toHaveBeenCalled();
      expect(a.stateFilter()).toBe("archived");
      dispose();
    });
  });

  it("does not restore a row whose computer leaves restore out (zerus-wyrqg); an unreported one keeps Qt's rule", async () => {
    await createRoot(async (dispose) => {
      const a = setup();
      // An archive that ended before its first message saved no conversation.
      a.page(archivedView(1, { allowed_actions: ["rename", "forget"] }), archivedView(2, { allowed_actions: ["restore", "rename", "forget"] }));
      await a.restore(a.row(1));
      expect(mocked.runAction).not.toHaveBeenCalled();
      mocked.runAction.mockResolvedValueOnce(done());
      await a.restore(a.row(2));
      expect(mocked.runAction).toHaveBeenCalledWith(expect.objectContaining({ name: "codex/example/a2" }), { kind: "restore", archive_id: uuid(2) });
      dispose();
    });
  });

  it("forgets a failed restore: no refresh, the filter stays, the failure is sticky and the session can be restored again", async () => {
    await createRoot(async (dispose) => {
      const a = setup();
      a.page(archivedView(1));
      a.setSelectedKey(a.row(1).key);
      mocked.runAction.mockResolvedValueOnce(failed("resume failed")).mockRejectedValueOnce("hgs exited");
      await a.restore(a.row(1));
      expect(a.refreshArchive).not.toHaveBeenCalled();
      expect(a.stateFilter()).toBe("archived");
      expect(a.pendingCount()).toBe(0);
      // An unknown outcome is a failure too, never replayed.
      await a.restore(a.row(1));
      expect(mocked.runAction).toHaveBeenCalledTimes(2);
      expect(a.texts()).toEqual(["error:resume failed", "error:hgs exited"]);
      expect(a.pendingCount()).toBe(0);
      dispose();
    });
  });
});

describe("after a successful restore (Qt 3281-3336)", () => {
  async function restored(a: ReturnType<typeof setup>, n = 1) {
    a.page(archivedView(n));
    a.setSelectedKey(a.row(n).key);
    mocked.runAction.mockResolvedValueOnce(done());
    await a.restore(a.row(n));
    return a.row(n).key;
  }

  it("selects the live row once it appears with the restored conversation", async () => {
    await createRoot(async (dispose) => {
      const a = setup();
      await restored(a);
      a.live("codex/example/a1", { state: "paused" });
      await flush();
      expect(a.pendingCount()).toBe(1);
      a.live("codex/example/a1");
      await flush();
      expect(a.selectedKey()).toBe("\nrun:codex/example/a1");
      expect(a.pendingCount()).toBe(0);
      expect(a.texts()).toEqual(["info:Session updated. Refreshing…"]);
      dispose();
    });
  });

  it("clears the selection with the sticky notice when it appears with another conversation", async () => {
    await createRoot(async (dispose) => {
      const a = setup();
      await restored(a);
      a.live("codex/example/a1", { conversation_id: "conv-other" });
      await flush();
      expect(a.selectedKey()).toBeUndefined();
      expect(a.texts().at(-1)).toBe(`error:${RESTORE_UNVERIFIED}`);
      expect(a.actions.notices().at(-1)?.sticky).toBe(true);
      expect(RESTORE_UNVERIFIED).toBe("The restored conversation could not be verified. Your archived draft remains in Saved drafts.");
      dispose();
    });
  });

  it("follows a live row without a conversation, as Qt's legacy rows", async () => {
    await createRoot(async (dispose) => {
      const a = setup();
      await restored(a);
      a.live("codex/example/a1", { conversation_id: undefined });
      await flush();
      expect(a.selectedKey()).toBe("\nrun:codex/example/a1");
      expect(a.texts()).toEqual(["info:Session updated. Refreshing…"]);
      dispose();
    });
  });

  it("follows a live row that was listed before the outcome", async () => {
    await createRoot(async (dispose) => {
      const a = setup();
      a.page(archivedView(1));
      a.setSelectedKey(a.row(1).key);
      const calls = deferredCalls();
      const restore = a.restore(a.row(1));
      await flush();
      // Not occupied any more once the call ran: the live row arrives first.
      a.live("codex/example/a1");
      await flush();
      expect(a.selectedKey()).toBe(a.row(1).key);
      await calls.answer(done());
      await restore;
      expect(a.selectedKey()).toBe("\nrun:codex/example/a1");
      dispose();
    });
  });

  it("leaves a selection the user changed meanwhile", async () => {
    await createRoot(async (dispose) => {
      const a = setup();
      await restored(a);
      a.setSelectedKey("\nrun:elsewhere");
      a.live("codex/example/a1", { conversation_id: "conv-other" });
      await flush();
      expect(a.selectedKey()).toBe("\nrun:elsewhere");
      expect(a.texts()).toEqual(["info:Session updated. Refreshing…"]);
      expect(a.pendingCount()).toBe(0);
      dispose();
    });
  });

  it("drops a cancelled follow: the user's filter or search change clears the old selection when the verified row appears", async () => {
    for (const change of ["filter", "search", "batch"] as const) {
      await createRoot(async (dispose) => {
        const a = setup();
        const old = await restored(a);
        if (change === "filter") a.setStateFilter("archived");
        else if (change === "search") a.setSearch("a1");
        else { a.setBatchRunning(true); a.setBatchRunning(false); }
        expect(a.selectedKey()).toBe(old);
        a.live("codex/example/a1");
        await flush();
        expect(a.selectedKey()).toBeUndefined();
        expect(a.texts()).toEqual(["info:Session updated. Refreshing…"]);
        dispose();
      });
    }
  });

  it("keeps the restored source as the selected row while it waits (Qt's selected())", async () => {
    await createRoot(async (dispose) => {
      const a = setup();
      const key = await restored(a);
      a.archived.clear();
      expect(a.sourceOf(key)?.archiveId).toBe(uuid(1));
      // Its header Restore reaches Qt's limit check, as changeSession on selected().
      await a.restore(snapshotOf(a.sourceOf(key)!));
      expect(mocked.runAction).toHaveBeenCalledTimes(1);
      expect(a.texts()).toEqual(["info:Session updated. Refreshing…", `error:${RESTORES_PENDING}`]);
      a.live("codex/example/a1");
      await flush();
      expect(a.sourceOf(key)).toBeUndefined();
      dispose();
    });
  });
});

describe("the batch's archive calls (Qt runNextSelectionAction 333-343, finishSelectionAction 379-384)", () => {
  it("find the archived row as its page lists it now", async () => {
    await createRoot(async (dispose) => {
      const a = setup();
      a.page(archivedView(1));
      const row = a.row(1);
      expect(a.calls.current(row)?.archiveId).toBe(uuid(1));
      a.archived.clear();
      expect(a.calls.current(row)).toBeUndefined();
      dispose();
    });
  });

  it("restore quietly, count a pending one of the same session or the 64th as the limit, and follow nothing", async () => {
    await createRoot(async (dispose) => {
      const a = setup();
      mocked.runAction.mockResolvedValue(done());
      for (let n = 1; n <= RESTORE_LIMIT + 1; n++) a.page(archivedView(n));
      a.setSelectedKey(a.row(1).key);
      expect(await a.calls.restore(a.row(1))).toEqual(done());
      expect(mocked.runAction).toHaveBeenLastCalledWith(expect.objectContaining({ name: "codex/example/a1" }), { kind: "restore", archive_id: uuid(1) });
      expect(a.texts()).toEqual([]);
      expect(a.refreshArchive).not.toHaveBeenCalled();
      expect(a.stateFilter()).toBe("archived");
      expect(a.calls.restore(a.row(1))).toBe("limit");
      for (let n = 2; n <= RESTORE_LIMIT; n++) await a.calls.restore(a.row(n));
      expect(a.calls.restore(a.row(RESTORE_LIMIT + 1))).toBe("limit");
      expect(mocked.runAction).toHaveBeenCalledTimes(RESTORE_LIMIT);
      // A batch's restore is not followed: a verified row clears the old selection (Qt 3297-3305).
      a.live("codex/example/a1");
      await flush();
      expect(a.selectedKey()).toBeUndefined();
      expect(a.pendingCount()).toBe(RESTORE_LIMIT - 1);
      dispose();
    });
  });

  it("see a name as taken only while the catalog lists the live row a restore saw appear", async () => {
    await createRoot(async (dispose) => {
      const a = setup();
      a.page(archivedView(1), archivedView(1, { archive_id: uuid(2) }));
      const second = snapshotOf(a.archived.get(`\narchive:${uuid(2)}`)!);
      mocked.runAction.mockResolvedValueOnce(done());
      await a.calls.restore(a.row(1));
      // Pending, its live row not listed yet: the restore limit answers, the name is not taken.
      expect(a.calls.taken(second)).toBe(false);
      expect(a.calls.restore(second)).toBe("limit");
      a.live("codex/example/a1");
      await flush();
      expect(a.pendingCount()).toBe(0);
      expect(a.calls.taken(second)).toBe(true);
      a.catalog.applyBatch({ ...emptyBatch(), removes: [{ computer: "", key: "run:codex/example/a1" }] });
      expect(a.calls.taken(second)).toBe(false);
      dispose();
    });
  });

  it("drop a failed restore and forget quietly with the archive id", async () => {
    await createRoot(async (dispose) => {
      const a = setup();
      a.page(archivedView(1));
      mocked.runAction.mockResolvedValueOnce(failed("no")).mockResolvedValueOnce(done());
      expect(await a.calls.restore(a.row(1))).toEqual(failed("no"));
      expect(a.pendingCount()).toBe(0);
      expect(await a.calls.forget(a.row(1))).toEqual(done());
      expect(mocked.runAction).toHaveBeenLastCalledWith(expect.objectContaining({ name: "codex/example/a1" }), { kind: "forget_archive", archive_id: uuid(1) });
      expect(a.texts()).toEqual([]);
      a.calls.refresh();
      expect(a.refreshArchive).toHaveBeenCalledTimes(1);
      dispose();
    });
  });
});

describe("Forget archived session (Qt terminateSession 5758-5826)", () => {
  it("forgets the exact instance the dialog named, refreshes the archive and words the outcome", async () => {
    await createRoot(async (dispose) => {
      const a = setup();
      a.page(archivedView(1));
      mocked.runAction.mockResolvedValueOnce(done());
      await a.forgetArchived(a.row(1));
      expect(mocked.runAction).toHaveBeenCalledTimes(1);
      expect(mocked.runAction).toHaveBeenCalledWith({ computer: "", name: "codex/example/a1", run_id: "run-a1", conversation_id: "conv-a1" }, { kind: "forget_archive", archive_id: uuid(1) });
      expect(a.refreshArchive).toHaveBeenCalledTimes(1);
      expect(a.texts()).toEqual(["info:Session updated. Refreshing…"]);
      dispose();
    });
  });

  it("does nothing but Qt's notice for a row that is gone or another instance since the dialog opened", async () => {
    await createRoot(async (dispose) => {
      const a = setup();
      a.page(archivedView(1));
      const shown = a.row(1);
      a.page(archivedView(1, { created: 99 }));
      await a.forgetArchived(shown);
      a.archived.clear();
      await a.forgetArchived(shown);
      expect(mocked.runAction).not.toHaveBeenCalled();
      expect(a.refreshArchive).not.toHaveBeenCalled();
      expect(a.texts()).toEqual([`error:${NO_LONGER_AVAILABLE}`, `error:${NO_LONGER_AVAILABLE}`]);
      expect(NO_LONGER_AVAILABLE).toBe("This session is no longer available. Refresh the list and open its actions again.");
      dispose();
    });
  });

  it("refuses at confirmation a reloaded row with the same archive id and another name", async () => {
    await createRoot(async (dispose) => {
      const a = setup();
      a.page(archivedView(1));
      const shown = a.row(1);
      a.page(archivedView(1, { name: "codex/example/renamed" }));
      await a.forgetArchived(shown);
      expect(mocked.runAction).not.toHaveBeenCalled();
      expect(a.refreshArchive).not.toHaveBeenCalled();
      expect(a.texts()).toEqual([`error:${NO_LONGER_AVAILABLE}`]);
      dispose();
    });
  });

  it("ends silently when the computer went offline or a batch runs, and refreshes nothing after a failure", async () => {
    await createRoot(async (dispose) => {
      const off = setup({ online: false });
      off.page(archivedView(1));
      await off.forgetArchived(off.row(1));
      const a = setup();
      a.page(archivedView(1));
      a.setBatchRunning(true);
      await a.forgetArchived(a.row(1));
      expect(mocked.runAction).not.toHaveBeenCalled();
      expect([...off.texts(), ...a.texts()]).toEqual([]);
      a.setBatchRunning(false);
      mocked.runAction.mockResolvedValueOnce(failed("kill failed"));
      await a.forgetArchived(a.row(1));
      expect(a.refreshArchive).not.toHaveBeenCalled();
      expect(a.texts()).toEqual(["error:kill failed"]);
      dispose();
    });
  });

  it("cancels a pending follow (Qt clears m_restoreKey)", async () => {
    await createRoot(async (dispose) => {
      const a = setup();
      a.page(archivedView(1), archivedView(2));
      a.setSelectedKey(a.row(1).key);
      mocked.runAction.mockResolvedValue(done());
      await a.restore(a.row(1));
      await a.forgetArchived(a.row(2));
      a.live("codex/example/a1");
      await flush();
      expect(a.selectedKey()).toBeUndefined();
      dispose();
    });
  });
});
