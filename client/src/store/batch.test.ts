import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.hoisted(() => {
  // store/ui.ts reads the page address and storage when it loads.
  const store = new Map<string, string>();
  vi.stubGlobal("location", { search: "", hash: "", href: "http://localhost/" });
  vi.stubGlobal("localStorage", { getItem: (k: string) => store.get(k) ?? null, setItem: (k: string, v: string) => { store.set(k, v); }, removeItem: (k: string) => { store.delete(k); } });
});
vi.mock("../bridge", () => ({
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

import { createRoot } from "solid-js";
import { createStore } from "solid-js/store";
import * as bridge from "../bridge";
import { createCatalog, toSession } from "./catalog";
import { createActions } from "./actions";
import { createBatch, type BatchDeps, type ArchiveCalls } from "./batch";
import { needsAttention } from "./ui";
import { snapshotOf } from "../ui/sessions/actions-ui";
import type { BatchConfirmation, SelectionRow } from "../ui/sessions/selection";
import type { Session } from "../ui/sessions/types";
import { emptyBatch, type Batch, type Outcome, type RowOut, type ViewRow } from "../types";

const mocked = vi.mocked(bridge);
const view = (tag: string, extra: Partial<ViewRow> = {}): ViewRow => ({
  name: `claude/p/${tag}`, agent: "claude", state: "running", resumable: true, activity: "idle", process_state: "running", tracked: true,
  run_id: `run-${tag}`, conversation_id: `conv-${tag}`, machine: "box", created: 100, ...extra,
});
const rowOut = (tag: string, extra: Partial<ViewRow> = {}, computer = ""): RowOut => ({ computer, key: `run:${tag}`, row: view(tag, extra) });
const batchOf = (part: Partial<Batch>): Batch => ({ ...emptyBatch(), ...part });
const done = (cleanup: Extract<Outcome, { outcome: "done" }>["cleanup"] = null): Outcome => ({ outcome: "done", receipt: {}, cleanup });
const failed = (detail: string): Outcome => ({ outcome: "failed", detail });
const flush = async () => { for (let i = 0; i < 10; i++) await Promise.resolve(); };

/** Calls that answer only when the test says so, in order. */
function deferredCalls() {
  const pending: ((o: Outcome) => void)[] = [];
  mocked.runAction.mockImplementation(() => new Promise<Outcome>((resolve) => { pending.push(resolve); }));
  return { answer: async (o: Outcome) => { pending.shift()!(o); await flush(); }, waiting: () => pending.length };
}

function setup(rows: RowOut[], options: { online?: (computer: string) => boolean; confirm?: boolean; archive?: ArchiveCalls; scans?: { n: number }; delay?: BatchDeps["delay"] } = {}) {
  const online = options.online ?? (() => true);
  const catalog = createCatalog();
  catalog.applyBatch(batchOf({ upserts: rows }));
  // Counts the walks over every catalog row (a `for in` asks for the keys once per walk).
  const scans = options.scans;
  const listedRows = scans ? new Proxy(catalog.catalog.rows, { ownKeys: (t) => { scans.n++; return Reflect.ownKeys(t); } }) : catalog.catalog.rows;
  const actions = createActions(catalog.catalog, online);
  const [compact, setCompact] = createStore<Record<string, boolean>>({});
  const confirm = vi.fn(async (_copy: BatchConfirmation) => options.confirm ?? true);
  const markRead = vi.fn((_row: unknown) => true);
  const markNeedsAttention = vi.fn((_row: unknown) => true);
  const b = createBatch({
    catalog: { rows: listedRows },
    online,
    updating: actions.updating,
    perform: (row, action, delay) => actions.perform(row, action, { quiet: true, delay }),
    delay: options.delay,
    notify: actions.notify,
    dismiss: actions.dismiss,
    compact: { get: (key) => !!compact[key], set: (key, value) => setCompact(key, value) },
    attention: (s) => needsAttention(s as Session, online(s.computer)),
    markRead,
    markNeedsAttention,
    confirm,
    archive: () => options.archive,
  });
  /** The listed rows, as the selection hands them over. */
  const listed = (...tags: string[]): SelectionRow[] => tags.map((t) => catalog.catalog.rows[`\nrun:${t}`]!);
  return { ...catalog, ...actions, ...b, compact, confirm, markRead, markNeedsAttention, listed, texts: () => actions.notices().map((n) => n.text) };
}

beforeEach(() => {
  vi.useFakeTimers();
  mocked.runAction.mockReset();
});
afterEach(() => vi.useRealTimers());

describe("the queue", () => {
  it("runs one session at a time, each only after the previous outcome", async () => {
    await createRoot(async (dispose) => {
      const a = setup([rowOut("a"), rowOut("b"), rowOut("c")]);
      const calls = deferredCalls();
      const run = a.run("pause", a.listed("a", "b", "c"));
      await flush();
      expect(a.running()).toBe(true);
      expect(mocked.runAction).toHaveBeenCalledTimes(1);
      expect(mocked.runAction).toHaveBeenLastCalledWith({ computer: "", name: "claude/p/a", run_id: "run-a", conversation_id: "conv-a" }, { kind: "pause" });
      expect(a.texts()).toEqual(["Pause: 1 / 3…"]);
      await calls.answer(done());
      expect(mocked.runAction).toHaveBeenCalledTimes(2);
      expect(mocked.runAction).toHaveBeenLastCalledWith(expect.objectContaining({ name: "claude/p/b" }), { kind: "pause" });
      // One progress line at a time, as Qt's single notice; no single-session notices.
      expect(a.texts()).toEqual(["Pause: 2 / 3…"]);
      await calls.answer(done());
      await calls.answer(done());
      await run;
      expect(mocked.runAction).toHaveBeenCalledTimes(3);
      expect(a.running()).toBe(false);
      expect(a.notices()).toEqual([expect.objectContaining({ tone: "info", text: "Pause: 3 of 3 completed.", sticky: false })]);
      dispose();
    });
  });

  it("counts a failed item as failed, goes on with the next and never retries", async () => {
    await createRoot(async (dispose) => {
      const a = setup([rowOut("a"), rowOut("b"), rowOut("c"), rowOut("d")]);
      mocked.runAction
        .mockResolvedValueOnce(done())
        .mockResolvedValueOnce(failed("boom"))
        .mockRejectedValueOnce("hgs exited")
        .mockResolvedValueOnce(done());
      await a.run("pause", a.listed("a", "b", "c", "d"));
      expect(mocked.runAction).toHaveBeenCalledTimes(4);
      expect(a.notices()).toEqual([expect.objectContaining({
        tone: "error", sticky: true,
        text: "Pause: 2 of 4 completed. 2 failed or changed; refresh before retrying.\npause claude/p/b: boom\npause claude/p/c: hgs exited",
      })]);
      vi.advanceTimersByTime(60_000);
      await flush();
      expect(mocked.runAction).toHaveBeenCalledTimes(4);
      expect(a.texts()).toEqual(["Pause: 2 of 4 completed. 2 failed or changed; refresh before retrying.\npause claude/p/b: boom\npause claude/p/c: hgs exited"]);
      dispose();
    });
  });

  it("shows the first three errors of the failures", async () => {
    await createRoot(async (dispose) => {
      const a = setup(["a", "b", "c", "d"].map((t) => rowOut(t)));
      mocked.runAction.mockResolvedValue(failed("no"));
      await a.run("pause", a.listed("a", "b", "c", "d"));
      expect(a.texts()).toEqual(["Pause: 0 of 4 completed. 4 failed or changed; refresh before retrying.\npause claude/p/a: no\npause claude/p/b: no\npause claude/p/c: no"]);
      dispose();
    });
  });

  it("leaves a row that changed or went away meanwhile as it is, without a call", async () => {
    await createRoot(async (dispose) => {
      const a = setup(["a", "b", "c", "d", "e", "f"].map((t) => rowOut(t)));
      const calls = deferredCalls();
      const run = a.run("pause", a.listed("a", "b", "c", "d", "e", "f"));
      await flush();
      // While a runs: b is paused, c is replaced by a new run, d gets another conversation, e another created, f becomes busy.
      a.applyBatch(batchOf({
        removes: [{ computer: "", key: "run:c" }],
        upserts: [rowOut("b", { state: "paused" }), rowOut("c2", { name: "claude/p/c" }), rowOut("d", { conversation_id: "other" }), rowOut("e", { created: 200 }), rowOut("f", { activity: "busy" })],
      }));
      await calls.answer(done());
      await run;
      expect(mocked.runAction).toHaveBeenCalledTimes(1);
      expect(a.texts()).toEqual([
        "Pause: 1 of 6 completed. 5 failed or changed; refresh before retrying.\nbox / claude/p/b: session changed or is unavailable"
        + "\nbox / claude/p/c: session changed or is unavailable\nbox / claude/p/d: session changed or is unavailable",
      ]);
      dispose();
    });
  });

  it("numbers the progress by the items already counted", async () => {
    await createRoot(async (dispose) => {
      const a = setup([rowOut("a"), rowOut("b"), rowOut("c")]);
      const calls = deferredCalls();
      const run = a.run("pause", a.listed("a", "b", "c"));
      await flush();
      a.applyBatch(batchOf({ upserts: [rowOut("b", { state: "paused" })] }));
      await calls.answer(done());
      expect(a.texts()).toEqual(["Pause: 3 / 3…"]);
      await calls.answer(done());
      await run;
      dispose();
    });
  });

  it("resumes, archives and keeps the worktree notices of each item", async () => {
    await createRoot(async (dispose) => {
      const a = setup([rowOut("a", { state: "paused" }), rowOut("b", { state: "stopped" })]);
      mocked.runAction
        .mockResolvedValueOnce(done({ status: "kept", path: "/home/example/wt", common_dir: "/home/example/.git", reason: "uncommitted changes" }))
        .mockResolvedValueOnce(done({ status: "removed", path: "/home/example/wt2", common_dir: "/home/example/.git", reason: null }));
      await a.run("archive", a.listed("a", "b"));
      expect(mocked.runAction.mock.calls.map((c) => c[1])).toEqual([{ kind: "archive" }, { kind: "archive" }]);
      expect(a.texts()).toEqual([
        "Working folder kept for review.",
        "Worktree removed automatically. The branch and archived conversation are kept.",
        "Move to archive: 2 of 2 completed.",
      ]);
      mocked.runAction.mockReset();
      mocked.runAction.mockResolvedValue(done());
      for (const n of a.notices()) a.dismiss(n.id);
      a.applyBatch(batchOf({ upserts: [rowOut("a", { state: "paused" })] }));
      await a.run("resume", a.listed("a"));
      expect(mocked.runAction).toHaveBeenCalledWith(expect.objectContaining({ name: "claude/p/a" }), { kind: "resume" });
      expect(a.texts()).toEqual(["Resume / restore: 1 of 1 completed."]);
      dispose();
    });
  });

  it("does nothing without an applicable row", async () => {
    await createRoot(async (dispose) => {
      const a = setup([rowOut("a", { state: "paused" })]);
      await a.run("pause", a.listed("a"));
      await a.run("terminate", a.listed("a"));
      await a.run("pause", []);
      expect(mocked.runAction).not.toHaveBeenCalled();
      expect(a.confirm).not.toHaveBeenCalled();
      expect(a.notices()).toEqual([]);
      expect(a.running()).toBe(false);
      dispose();
    });
  });

  it("acts only on the applicable rows; a row on an offline computer or with a write pending is left out", async () => {
    await createRoot(async (dispose) => {
      const a = setup([rowOut("a"), rowOut("b", {}, "peer"), rowOut("c")], { online: (c) => c !== "peer" });
      mocked.runAction.mockResolvedValue(done());
      let finish: (o: Outcome) => void = () => {};
      mocked.runAction.mockReturnValueOnce(new Promise((r) => { finish = r; }));
      const single = a.perform(snapshotOf(a.listed("c")[0]), { kind: "pause" });
      const rows = [...a.listed("a"), a.catalog.rows["peer\nrun:b"]!, ...a.listed("c")];
      await a.run("pause", rows);
      expect(mocked.runAction).toHaveBeenCalledTimes(2);
      expect(mocked.runAction).toHaveBeenLastCalledWith(expect.objectContaining({ name: "claude/p/a" }), { kind: "pause" });
      expect(a.texts()).toContain("Pause: 1 of 1 completed.");
      finish(done());
      await single;
      dispose();
    });
  });
});

describe("Forget and Terminate", () => {
  it("ask first with Qt's copy; Cancel runs nothing", async () => {
    await createRoot(async (dispose) => {
      const a = setup([rowOut("a"), rowOut("b", { state: "paused" })], { confirm: false });
      await a.run("terminate", a.listed("a", "b"));
      expect(a.confirm).toHaveBeenCalledTimes(1);
      expect(a.confirm.mock.calls[0][0]).toEqual({
        title: "Terminate",
        text: "Terminate 1 selected sessions?\n\nRunning processes will stop. Tracked Codex, Claude and Kimi conversations are kept in Archive; native conversation history is kept."
          + "\n\n1 unavailable or inapplicable sessions will be left as they are.",
        details: ["box / claude/p/a"],
        confirm: "Terminate 1 sessions",
      });
      expect(mocked.runAction).not.toHaveBeenCalled();
      expect(a.notices()).toEqual([]);
      expect(a.running()).toBe(false);
      dispose();
    });
  });

  it("terminate a tracked Codex, Claude or Kimi session into Archive and kill any other", async () => {
    await createRoot(async (dispose) => {
      const a = setup([rowOut("a"), rowOut("b", { tracked: false }), rowOut("c", { agent: "kimi" })]);
      mocked.runAction.mockResolvedValue(done());
      await a.run("terminate", a.listed("a", "b", "c"));
      expect(mocked.runAction.mock.calls).toEqual([
        [{ computer: "", name: "claude/p/a", run_id: "run-a", conversation_id: "conv-a" }, { kind: "terminate" }],
        [expect.objectContaining({ name: "claude/p/b" }), { kind: "kill" }],
        [expect.objectContaining({ name: "claude/p/c" }), { kind: "terminate" }],
      ]);
      expect(a.texts()).toEqual(["Terminate: 3 of 3 completed."]);
      dispose();
    });
  });

  it("forget an OpenCode binding by its run and conversation and a saved session by kill", async () => {
    await createRoot(async (dispose) => {
      const a = setup([rowOut("a", { agent: "opencode", allowed_actions: ["forget", "rename"] }), rowOut("b", { state: "paused" }), rowOut("c")]);
      mocked.runAction.mockResolvedValueOnce(done()).mockResolvedValueOnce(failed("refused"));
      await a.run("forget", a.listed("a", "b", "c"));
      expect(a.confirm.mock.calls[0][0].text).toBe("Forget 2 selected sessions?\n\nThese saved or archived entries are removed. Other live sessions and native conversation history are kept."
        + "\n\nOpenCode sessions: only Zerus forgets them; their conversations stay in OpenCode.\n\n1 unavailable or inapplicable sessions will be left as they are.");
      expect(mocked.runAction.mock.calls).toEqual([
        [{ computer: "", name: "claude/p/a", run_id: "run-a", conversation_id: "conv-a" }, { kind: "forget_binding" }],
        [expect.objectContaining({ name: "claude/p/b" }), { kind: "forget" }],
      ]);
      expect(a.texts()).toEqual(["Forget: 1 of 2 completed. 1 failed or changed; refresh before retrying.\nkill claude/p/b: refused"]);
      dispose();
    });
  });

  it("act only on the exact instance the dialog named", async () => {
    await createRoot(async (dispose) => {
      const a = setup([rowOut("a"), rowOut("b")]);
      let answer: (ok: boolean) => void = () => {};
      a.confirm.mockImplementationOnce(() => new Promise((r) => { answer = r; }));
      mocked.runAction.mockResolvedValue(done());
      const run = a.run("terminate", a.listed("a", "b"));
      await flush();
      expect(a.running()).toBe(false);
      a.applyBatch(batchOf({ upserts: [rowOut("b", { conversation_id: "another" })] }));
      answer(true);
      await run;
      expect(mocked.runAction).toHaveBeenCalledTimes(1);
      expect(mocked.runAction).toHaveBeenCalledWith(expect.objectContaining({ name: "claude/p/a" }), { kind: "terminate" });
      expect(a.texts()).toEqual(["Terminate: 1 of 2 completed. 1 failed or changed; refresh before retrying.\nbox / claude/p/b: session changed or is unavailable"]);
      dispose();
    });
  });

  it("word Clear archive when the archive is cleared", async () => {
    await createRoot(async (dispose) => {
      const a = setup([], { confirm: false });
      const archived = [toSession("", "archive:a1", view("a", { state: "archived", archive_id: "a1" }))];
      await a.run("forget", archived, { clearArchive: true });
      expect(a.confirm.mock.calls[0][0]).toMatchObject({ title: "Clear archive", confirm: "Clear archive", details: ["box / claude/p/a"] });
      expect(a.confirm.mock.calls[0][0].text.startsWith("Remove 1 archived sessions from HGS on the selected machines?")).toBe(true);
      await a.run("forget", archived, { clearArchive: true, paged: true });
      expect(a.confirm.mock.calls[1][0].text.startsWith("Remove 1 loaded archived sessions in the current date and machine scope? Unloaded entries are kept.")).toBe(true);
      dispose();
    });
  });
});

describe("archived rows", () => {
  const archivedRow = () => toSession("", "archive:a1", view("a", { state: "archived", archive_id: "a1" }));

  it("fail without a call until the archive calls are bound", async () => {
    await createRoot(async (dispose) => {
      const a = setup([]);
      await a.run("resume", [archivedRow()]);
      expect(mocked.runAction).not.toHaveBeenCalled();
      expect(a.texts()).toEqual(["Resume / restore: 0 of 1 completed. 1 failed or changed; refresh before retrying.\nbox / claude/p/a: session changed or is unavailable"]);
      dispose();
    });
  });

  it("restore and forget through the bound archive calls, once each", async () => {
    await createRoot(async (dispose) => {
      const row = archivedRow();
      const archive: ArchiveCalls = {
        current: vi.fn(() => row),
        restore: vi.fn(async () => done()),
        forget: vi.fn(async () => failed("not found")),
        refresh: vi.fn(),
        taken: () => false,
      };
      const a = setup([], { archive });
      await a.run("resume", [row]);
      expect(archive.restore).toHaveBeenCalledTimes(1);
      expect(a.texts()).toContain("Resume / restore: 1 of 1 completed.");
      await a.run("forget", [row]);
      expect(archive.forget).toHaveBeenCalledTimes(1);
      expect(a.texts()).toContain("Forget: 0 of 1 completed. 1 failed or changed; refresh before retrying.\nforget archive claude/p/a: not found");
      expect(mocked.runAction).not.toHaveBeenCalled();
      dispose();
    });
  });

  it("count a restore past the restore limit as failed", async () => {
    await createRoot(async (dispose) => {
      const row = archivedRow();
      const archive: ArchiveCalls = { current: () => row, restore: vi.fn(() => "limit" as const), forget: vi.fn(), refresh: vi.fn(), taken: () => false };
      const a = setup([], { archive });
      await a.run("resume", [row]);
      expect(a.texts()).toEqual(["Resume / restore: 0 of 1 completed. 1 failed or changed; refresh before retrying.\nclaude/p/a: earlier restores still need a refresh"]);
      dispose();
    });
  });

  it("are compacted and expanded as their page lists them now (archive.row)", async () => {
    await createRoot(async (dispose) => {
      const row = archivedRow();
      const listed = new Map([[row.key, row]]);
      const archive: ArchiveCalls = { current: vi.fn((r) => listed.get(r.key)), restore: vi.fn(), forget: vi.fn(), refresh: vi.fn(), taken: () => false };
      const a = setup([], { archive });
      await a.run("compact", [row]);
      expect(a.compact).toEqual({ [row.key]: true });
      await a.run("expand", [row]);
      expect(a.compact).toEqual({ [row.key]: false });
      // Gone from its page, or another instance: left as it is.
      listed.clear();
      await a.run("compact", [row]);
      listed.set(row.key, { ...row, created: 999 });
      await a.run("compact", [row]);
      expect(a.compact).toEqual({ [row.key]: false });
      dispose();
    });
  });

  it("check the name of every archived row against one name index per run, not a scan per row", async () => {
    await createRoot(async (dispose) => {
      const scans = { n: 0 };
      const rows = Array.from({ length: 5 }, (_, i) => toSession("", `archive:a${i}`, view(`a${i}`, { state: "archived", archive_id: `a${i}` })));
      const listed = new Map(rows.map((r) => [r.key, r]));
      const archive: ArchiveCalls = { current: (r) => listed.get(r.key), restore: vi.fn(async () => done()), forget: vi.fn(), refresh: vi.fn(), taken: () => false };
      const a = setup([rowOut("live", { name: "claude/p/a3", state: "paused" }), rowOut("other")], { archive, scans });
      await a.run("resume", rows);
      expect(scans.n).toBe(1);
      expect((archive.restore as ReturnType<typeof vi.fn>).mock.calls.map((c) => (c[0] as SelectionRow).name)).toEqual(["claude/p/a0", "claude/p/a1", "claude/p/a2", "claude/p/a4"]);
      dispose();
    });
  });

  /** Restore calls that keep their pending names as the restore store does (Qt's m_restores). */
  function trackingCalls(listed: Map<string, SelectionRow>, taken: (row: SelectionRow) => boolean = () => false) {
    const pending = new Set<string>();
    const restore = vi.fn((r: SelectionRow): Promise<Outcome> | "limit" => {
      if (pending.has(r.name)) return "limit";
      pending.add(r.name);
      return Promise.resolve(done());
    });
    const calls: ArchiveCalls = { current: (r) => listed.get(r.key), restore, forget: vi.fn(), refresh: vi.fn(), taken: (r) => taken(r as SelectionRow) };
    return { calls, restore, pending };
  }

  it("count a second instance of a name restored earlier in the run as past the restore limit (Qt SessionSelection.cpp 336-338)", async () => {
    await createRoot(async (dispose) => {
      const one = archivedRow();
      const two = toSession("", "archive:a2", view("a", { state: "archived", archive_id: "a2" }));
      const { calls, restore } = trackingCalls(new Map([[one.key, one], [two.key, two]]));
      const a = setup([], { archive: calls });
      await a.run("resume", [one, two]);
      expect(restore.mock.calls.map((c) => c[0].archiveId)).toEqual(["a1", "a2"]);
      expect(mocked.runAction).not.toHaveBeenCalled();
      expect(a.texts()).toEqual(["Resume / restore: 1 of 2 completed. 1 failed or changed; refresh before retrying.\nclaude/p/a: earlier restores still need a refresh"]);
      dispose();
    });
  });

  it("see a name taken when the catalog lists its live row by the next item: no call, session changed", async () => {
    await createRoot(async (dispose) => {
      const one = archivedRow();
      const two = toSession("", "archive:a2", view("a", { state: "archived", archive_id: "a2" }));
      let a: ReturnType<typeof setup> | undefined;
      // Only a live row the catalog holds takes the name; nothing is inferred from the earlier item.
      const listedLive = (r: SelectionRow) => Object.values(a!.catalog.rows).some((x) => x.name === r.name && x.state !== "archived");
      const tracking = trackingCalls(new Map([[one.key, one], [two.key, two]]), listedLive);
      tracking.restore.mockImplementationOnce((r) => {
        a!.applyBatch(batchOf({ upserts: [rowOut("restored", { name: r.name })] }));
        return Promise.resolve(done());
      });
      a = setup([], { archive: tracking.calls });
      await a.run("resume", [one, two]);
      expect(tracking.restore).toHaveBeenCalledTimes(1);
      expect(a.texts()).toEqual(["Resume / restore: 1 of 2 completed. 1 failed or changed; refresh before retrying.\nbox / claude/p/a: session changed or is unavailable"]);
      dispose();
    });
  });

  it("refresh the archive once when a write batch ends (Qt SessionsWindow.cpp 1916)", async () => {
    await createRoot(async (dispose) => {
      const row = archivedRow();
      const archive: ArchiveCalls = { current: () => row, restore: vi.fn(), forget: vi.fn(async () => done()), refresh: vi.fn(), taken: () => false };
      const a = setup([rowOut("b", { state: "paused" })], { archive });
      const calls = deferredCalls();
      const forget = a.run("forget", [row, ...a.listed("b")]);
      await flush();
      expect(archive.refresh).not.toHaveBeenCalled();
      await calls.answer(done());
      await forget;
      expect(archive.refresh).toHaveBeenCalledTimes(1);
      // A batch on live rows ends with a refresh too; marks, a cancelled confirmation or no target do not.
      mocked.runAction.mockResolvedValue(done());
      await a.run("archive", a.listed("b"));
      expect(archive.refresh).toHaveBeenCalledTimes(2);
      await a.run("review_later", a.listed("b"));
      await a.run("pause", a.listed("b"));
      a.confirm.mockResolvedValueOnce(false);
      await a.run("forget", [row]);
      expect(archive.refresh).toHaveBeenCalledTimes(2);
      dispose();
    });
  });

  it("go on with the next item when an archive hook throws, counting it failed with its error, never retried", async () => {
    await createRoot(async (dispose) => {
      const one = archivedRow();
      const two = toSession("", "archive:a2", view("b", { state: "archived", archive_id: "a2" }));
      const three = toSession("", "archive:a3", view("c", { state: "archived", archive_id: "a3" }));
      const current = vi.fn((r: { key: string }) => {
        if (r.key === two.key) throw new Error("page read failed");
        return r.key === one.key ? one : three;
      });
      const restore = vi.fn((r: { key: string }) => {
        if (r.key === one.key) throw new Error("hook broke");
        return Promise.resolve(done());
      });
      const archive: ArchiveCalls = { current, restore, forget: vi.fn(), refresh: vi.fn(), taken: () => false };
      const a = setup([], { archive });
      await a.run("resume", [one, two, three]);
      expect(restore.mock.calls.map((c) => c[0].key)).toEqual([one.key, three.key]);
      expect(a.running()).toBe(false);
      expect(a.texts()).toEqual(["Resume / restore: 1 of 3 completed. 2 failed or changed; refresh before retrying.\nrestore claude/p/a: hook broke\nrestore claude/p/b: page read failed"]);
      expect(archive.refresh).toHaveBeenCalledTimes(1);
      dispose();
    });
  });

  it("are not restored while a live row has their name", async () => {
    await createRoot(async (dispose) => {
      const a = setup([rowOut("live", { name: "claude/p/a", state: "paused" })]);
      expect(a.applies(archivedRow(), "resume", true, false)).toBe(false);
      const other = setup([rowOut("live", { name: "claude/p/other", state: "paused" })]);
      expect(other.applies(archivedRow(), "resume", true, false)).toBe(true);
      dispose();
    });
  });
});

describe("while a batch runs", () => {
  it("no other batch, Mark as read or Mark needs attention starts; Compact and Expand still apply", async () => {
    await createRoot(async (dispose) => {
      const a = setup([rowOut("a"), rowOut("b", { state: "paused", unread: true })]);
      const calls = deferredCalls();
      const run = a.run("pause", a.listed("a"));
      await flush();
      expect(a.running()).toBe(true);
      await a.run("archive", a.listed("b"));
      await a.run("mark_read", a.listed("b"));
      await a.run("review_later", a.listed("a"));
      expect(mocked.runAction).toHaveBeenCalledTimes(1);
      expect(a.markRead).not.toHaveBeenCalled();
      expect(a.markNeedsAttention).not.toHaveBeenCalled();
      await a.run("compact", a.listed("a", "b"));
      expect(a.compact).toEqual({ "\nrun:a": true, "\nrun:b": true });
      await calls.answer(done());
      await run;
      expect(a.running()).toBe(false);
      dispose();
    });
  });
});

describe("marks", () => {
  it("mark the rows that need attention read and say how many changed", async () => {
    await createRoot(async (dispose) => {
      const a = setup([rowOut("a", { unread: true }), rowOut("b", { unread: true }), rowOut("c")]);
      await a.run("mark_read", a.listed("a", "b", "c"));
      expect(a.markRead.mock.calls.map((c) => (c[0] as SelectionRow).name)).toEqual(["claude/p/a", "claude/p/b"]);
      expect(a.notices()).toEqual([expect.objectContaining({ tone: "info", text: "Mark as read: 2 of 2 sessions updated.", sticky: false })]);
      for (const n of a.notices()) a.dismiss(n.id);
      a.markRead.mockReturnValueOnce(false);
      await a.run("mark_read", a.listed("a", "b"));
      expect(a.notices()).toEqual([expect.objectContaining({ tone: "error", text: "Mark as read: 1 of 2 sessions updated.", sticky: true })]);
      expect(mocked.runAction).not.toHaveBeenCalled();
      dispose();
    });
  });

  it("mark the rows that do not need attention for review", async () => {
    await createRoot(async (dispose) => {
      const a = setup([rowOut("a", { unread: true }), rowOut("b"), rowOut("c", { state: "paused" })]);
      await a.run("review_later", a.listed("a", "b", "c"));
      expect(a.markNeedsAttention.mock.calls.map((c) => (c[0] as SelectionRow).name)).toEqual(["claude/p/b", "claude/p/c"]);
      expect(a.texts()).toEqual(["Mark needs attention: 2 of 2 sessions updated."]);
      // Offline, a listed reply is not attention (SessionPresentation.h:281): it can be marked too.
      const offline = setup([rowOut("a", { unread: true }), rowOut("b")], { online: () => false });
      await offline.run("review_later", offline.listed("a", "b"));
      expect(offline.markNeedsAttention.mock.calls.map((c) => (c[0] as SelectionRow).name)).toEqual(["claude/p/a", "claude/p/b"]);
      dispose();
    });
  });
});

describe("row presentation", () => {
  it("Compact and Expand change the row preference at once, only for the same instance", async () => {
    await createRoot(async (dispose) => {
      const a = setup([rowOut("a"), rowOut("b"), rowOut("c")]);
      const shown = a.listed("a", "b", "c").map((s) => ({ ...s }));
      a.applyBatch(batchOf({ upserts: [rowOut("b", { created: 300 })], removes: [{ computer: "", key: "run:c" }] }));
      await a.run("compact", shown);
      expect(a.compact).toEqual({ "\nrun:a": true });
      await a.run("expand", a.listed("a", "b"));
      expect(a.compact).toEqual({ "\nrun:a": false });
      expect(a.notices()).toEqual([]);
      expect(mocked.runAction).not.toHaveBeenCalled();
      dispose();
    });
  });

  it("Move to project waits for project organisation", async () => {
    await createRoot(async (dispose) => {
      const a = setup([rowOut("a")]);
      expect(a.applies(a.listed("a")[0], "move", false, true)).toBe(true);
      await a.run("move", a.listed("a"));
      expect(mocked.runAction).not.toHaveBeenCalled();
      expect(a.notices()).toEqual([]);
      dispose();
    });
  });
});

describe("applies", () => {
  it("reads the row preference and the attention of the row", async () => {
    await createRoot(async (dispose) => {
      const a = setup([rowOut("a", { unread: true }), rowOut("b")]);
      const [ra, rb] = a.listed("a", "b");
      expect(a.applies(ra, "compact", true, false)).toBe(true);
      await a.run("compact", [ra]);
      expect(a.applies(ra, "compact", true, false)).toBe(false);
      expect(a.applies(ra, "expand", true, false)).toBe(true);
      expect(a.applies(ra, "mark_read", true, false)).toBe(true);
      expect(a.applies(rb, "mark_read", true, false)).toBe(false);
      expect(a.applies(rb, "review_later", true, false)).toBe(true);
      expect(a.applies(rb, "pause", true, false)).toBe(true);
      expect(a.applies(rb, "pause", false, false)).toBe(false);
      expect(a.applies(rb, "pause", true, true)).toBe(false);
      dispose();
    });
  });
});


describe("one grace period per selection", () => {
  it("does not write before release and refuses only a changed member afterwards", async () => {
    await createRoot(async (dispose) => {
      let release!: () => Promise<void>;
      const delay: BatchDeps["delay"] = (rows, _action, execute) => new Promise((resolve) => {
        expect(rows).toHaveLength(3);
        release = async () => { await execute(rows.map((_, index) => ({ id: "batch-window", index })), "batch-key"); resolve(); };
      });
      const a = setup([rowOut("a", { state: "paused" }), rowOut("b", { state: "paused" }), rowOut("c", { state: "paused" })], { delay });
      mocked.runAction.mockResolvedValue(done());
      const job = a.run("archive", a.listed("a", "b", "c"));
      await flush(); expect(mocked.runAction).not.toHaveBeenCalled();
      a.applyBatch(batchOf({ upserts: [rowOut("b", { state: "paused", run_id: "replacement" })] }));
      await release(); await job;
      expect(mocked.runAction.mock.calls.map((c) => [c[0].name, c[2]])).toEqual([
        ["claude/p/a", { id: "batch-window", index: 0 }], ["claude/p/c", { id: "batch-window", index: 2 }],
      ]);
      expect(a.texts().at(-1)).toContain("2 of 3 completed. 1 failed or changed;"); dispose();
    });
  });
  it("Clear archive bypasses Undo", async () => {
    const delay = vi.fn();
    const one = toSession("", "archive:one", view("a", { state: "archived", archive_id: "archive-one" }));
    const forget = vi.fn(async () => done());
    const calls: ArchiveCalls = { current: () => one, restore: async () => done(), forget, refresh: () => {}, taken: () => false };
    const a = setup([], { archive: calls, delay });
    await a.run("forget", [one], { clearArchive: true });
    expect(delay).not.toHaveBeenCalled(); expect(forget).toHaveBeenCalledTimes(1);
  });
});
