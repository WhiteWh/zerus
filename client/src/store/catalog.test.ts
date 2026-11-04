import { describe, expect, it, vi } from "vitest";
import { createComputed, createRoot } from "solid-js";
import { agentKind, createCatalog, rowId, statusOf, toSession } from "./catalog";
import { createMarks, marks as appMarks } from "./marks";
import { emptyBatch, type Batch, type RowOut, type ViewRow } from "../types";

const view = (name: string, extra: Partial<ViewRow> = {}): ViewRow => ({
  name, title: name.split("/").pop(), agent: "claude", status: "ready", phase: "idle", attention: false, unread: false,
  machine: "workstation", project: "sample-project", folder: "/home/example/sample-project", branch: "main",
  model: "opus", effort: "high", summary: "", updated_at: 1_790_000_000, ...extra,
});
const row = (key: string, name: string, extra: Partial<ViewRow> = {}, computer = ""): RowOut => ({ computer, key, row: view(name, extra) });
const batchOf = (part: Partial<Batch>): Batch => ({ ...emptyBatch(), ...part });

describe("applyBatch", () => {
  it("keeps sh as Qt's Shell presentation rather than a Claude provider", () => {
    expect(agentKind("sh")).toBe("sh"); expect(agentKind("SH")).toBe("sh");
    const shell = toSession("", "run:shell", view("sh/example/probe", { agent: "sh", model: "", effort: null }));
    expect(shell.agent).toBe("sh"); expect(shell.cmd).toBe("sh"); expect(shell.model).toBe("");
    for (const provider of ["claude", "codex", "kimi", "opencode", "deepseek"] as const) expect(agentKind(provider)).toBe(provider);
    expect(agentKind("dsh")).toBe("deepseek");
  });
  it("updates a linked worktree chip and clears it when later metadata is absent", () => {
    const c = createCatalog();
    c.applyBatch(batchOf({ upserts: [row("run:chip", "codex/sample/one", { worktree: "api-work" })] }));
    expect(c.catalog.rows["\nrun:chip"].worktree).toBe("api-work");
    c.applyBatch(batchOf({ upserts: [row("run:chip", "codex/sample/one")] }));
    expect(c.catalog.rows["\nrun:chip"].worktree).toBeUndefined();
  });
  it("keeps verified Rust density keys without reconstructing them from display fields", () => {
    const hashes: [string, string] = ["a".repeat(64), "b".repeat(64)];
    const s=toSession("peer-fixture","run:fixture",view("codex/sample/fixture",{compact_keys:hashes}));
    expect(s.compactKeys).toEqual(hashes);
    expect(toSession("peer-fixture","run:older",view("older")).compactKeys).toBeUndefined();
  });
  it("keeps the identity of unchanged rows and updates a changed row in place", () => {
    createRoot((dispose) => {
      const { catalog, applyBatch } = createCatalog();
      applyBatch(batchOf({ replaces: [{ computer: "", rows: [row("run:a", "claude/sample-project/a"), row("run:b", "claude/sample-project/b")] }] }));
      const a = catalog.rows[rowId("", "run:a")];
      const b = catalog.rows[rowId("", "run:b")];
      let aReads = 0;
      createComputed(() => { void catalog.rows[rowId("", "run:a")].summary; aReads++; });
      aReads = 0;

      applyBatch(batchOf({ upserts: [row("run:b", "claude/sample-project/b", { summary: "Bash: cargo test", status: "working", phase: "tool" })] }));

      expect(catalog.rows[rowId("", "run:a")]).toBe(a);
      expect(catalog.rows[rowId("", "run:b")]).toBe(b);
      expect(b.summary).toBe("Bash: cargo test");
      expect(b.status).toBe("working");
      expect(b.caption).toBe("Using a tool");
      expect(aReads).toBe(0);
      expect(catalog.order).toEqual([rowId("", "run:a"), rowId("", "run:b")]);
      dispose();
    });
  });

  it("replaces one computer's rows in one update without an empty list in between", () => {
    createRoot((dispose) => {
      const { catalog, applyBatch } = createCatalog();
      applyBatch(batchOf({
        replaces: [
          { computer: "", rows: [row("run:a", "claude/p/a"), row("run:b", "claude/p/b")] },
          { computer: "mac", rows: [row("run:m", "codex/p/m", {}, "mac")] },
        ],
      }));
      const kept = catalog.rows[rowId("", "run:b")];
      const mac = catalog.rows[rowId("mac", "run:m")];
      const seen: number[] = [];
      createComputed(() => seen.push(catalog.order.length));
      seen.length = 0;

      applyBatch(batchOf({ replaces: [{ computer: "", rows: [row("run:b", "claude/p/b"), row("run:c", "claude/p/c")] }] }));

      expect(seen).toEqual([3]);
      expect(Object.keys(catalog.rows).sort()).toEqual([rowId("", "run:b"), rowId("", "run:c"), rowId("mac", "run:m")].sort());
      expect(catalog.rows[rowId("", "run:a")]).toBeUndefined();
      expect(catalog.rows[rowId("", "run:b")]).toBe(kept);
      expect(catalog.rows[rowId("mac", "run:m")]).toBe(mac);
      expect(catalog.order).toEqual([rowId("", "run:b"), rowId("mac", "run:m"), rowId("", "run:c")]);
      dispose();
    });
  });

  it("removes rows and carries a replaced run's selection to the new run of the same name", () => {
    createRoot((dispose) => {
      const { catalog, applyBatch, onReplaced } = createCatalog();
      applyBatch(batchOf({ replaces: [{ computer: "", rows: [row("run:old", "claude/p/review"), row("run:x", "claude/p/x")] }] }));
      const moves: [string, string][] = [];
      onReplaced((from, to) => moves.push([from, to]));

      applyBatch(batchOf({ removes: [{ computer: "", key: "run:old", reason: "replaced" }, { computer: "", key: "run:x", reason: "archived" }] }));
      expect(catalog.order).toEqual([]);
      applyBatch(batchOf({ upserts: [row("run:new", "claude/p/review")] }));

      expect(moves).toEqual([[rowId("", "run:old"), rowId("", "run:new")]]);
      expect(catalog.order).toEqual([rowId("", "run:new")]);
      dispose();
    });
  });

  it("attaches only the marks it is given, never retargeting the app's", () => {
    const appAttach = vi.spyOn(appMarks, "attach");
    createCatalog();
    expect(appAttach).not.toHaveBeenCalled();
    const own = createMarks({ attentionMarks: async () => ({ marks: {}, replies: {} }), updateAttentionMarks: async () => {}, queueReadMarks: async () => [], pendingReadMarks: async () => [] }, { load: () => [], save: () => {} });
    const ownAttach = vi.spyOn(own, "attach");
    createCatalog(own);
    expect(ownAttach).toHaveBeenCalledTimes(1);
    appAttach.mockRestore();
  });

  it("carries a known conversation's resumed run through a coalesced full catalog", () => {
    const c = createCatalog(), moves: [string, string][] = [];
    c.applyBatch(batchOf({ upserts: [row("run:old", "claude/p/review", { run_id: "old", conversation_id: "conversation", state: "paused" })] }));
    c.onReplaced((from, to) => moves.push([from, to]));
    c.applyBatch(batchOf({ replaces: [{ computer: "", rows: [row("run:new", "claude/p/review", { run_id: "new", conversation_id: "conversation", state: "running" })] }] }));
    expect(moves).toEqual([["\nrun:old", "\nrun:new"]]);
  });

  it("hands selection to a successor published before the replaced removal in a later batch", () => {
    const c = createCatalog(), moves: [string, string][] = [];
    c.applyBatch(batchOf({ upserts: [row("run:old", "claude/p/review", { run_id: "old", conversation_id: "conversation", state: "paused" })] }));
    c.onReplaced((from, to) => moves.push([from, to]));
    c.applyBatch(batchOf({ upserts: [row("run:new", "claude/p/review", { run_id: "new", conversation_id: "conversation", state: "running" })] }));
    expect(moves).toEqual([]);
    c.applyBatch(batchOf({ removes: [{ computer: "", key: "run:old", reason: "replaced" }] }));
    expect(moves).toEqual([["\nrun:old", "\nrun:new"]]);
  });

  it.each([{ conversation_id: "different" }, { conversation_id: "" }, { state: "archived" }, { run_id: "" }])(
    "does not infer a resumed run from unknown or different identity: %j", (changed) => {
      const c = createCatalog(), moves: unknown[] = [];
      c.applyBatch(batchOf({ upserts: [row("run:old", "claude/p/review", { run_id: "old", conversation_id: "conversation", state: "paused" })] }));
      c.onReplaced((...move) => moves.push(move));
      c.applyBatch(batchOf({ replaces: [{ computer: "", rows: [row("run:new", "claude/p/review", { run_id: "new", conversation_id: "conversation", state: "running", ...changed })] }] }));
      expect(moves).toEqual([]);
    });

  it("refuses ambiguous full-catalog successors and preserves an explicit handoff delivered before a full catalog", () => {
    const c = createCatalog(), moves: [string, string][] = [];
    c.applyBatch(batchOf({ upserts: [row("run:old", "claude/p/review", { run_id: "old", conversation_id: "conversation", state: "paused" })] }));
    c.onReplaced((from, to) => moves.push([from, to]));
    const next = row("run:new", "claude/p/review", { run_id: "new", conversation_id: "conversation", state: "running" });
    c.applyBatch(batchOf({ replaces: [{ computer: "", rows: [next, row("run:other", "claude/p/review", { run_id: "other", conversation_id: "conversation", state: "running" })] }] }));
    expect(moves).toEqual([]);
    c.applyBatch(batchOf({ removes: [{ computer: "", key: "run:new", reason: "replaced" }] }));
    c.applyBatch(batchOf({ replaces: [{ computer: "", rows: [row("run:last", "claude/p/review", { run_id: "last", conversation_id: "different", state: "running" })] }] }));
    expect(moves).toContainEqual(["\nrun:new", "\nrun:last"]);
  });

  it("records computer status", () => {
    createRoot((dispose) => {
      const { catalog, applyBatch } = createCatalog();
      applyBatch(batchOf({ computers: [{ computer: "mac", status: "reconnecting", attempt: 2 }] }));
      expect(catalog.computers.mac).toEqual({ id: "mac", status: "reconnecting", attempt: 2 });
      dispose();
    });
  });
});

describe("toSession", () => {
  it("carries the background count independently of the presentation status", () => {
    expect(toSession("", "run:a", view("a", { status: "paused", background: 3, activity: "idle", phase: "idle" })).backgroundJobs).toBe(3);
    expect(toSession("", "run:a", view("a")).backgroundJobs).toBeUndefined();
  });
  it("takes bulk lifecycle from the row, else from the agent", () => {
    expect(toSession("", "k", view("a", { agent: "claude" })).bulkLifecycle).toBe(true);
    expect(toSession("", "k", view("a", { agent: "opencode" })).bulkLifecycle).toBe(false);
    expect(toSession("", "k", view("a", { agent: "opencode", bulk_lifecycle_supported: true })).bulkLifecycle).toBe(true);
    expect(toSession("", "k", view("a", { agent: "claude", bulk_lifecycle_supported: false })).bulkLifecycle).toBe(false);
  });
  it("reads the canonical folder only when the core sends one (view.rs skips an empty one)", () => {
    const sent = toSession("", "k", view("a", { cwd_canonical: "/srv/example/project", git_root: "/srv/example/project" }));
    expect([sent.cwdCanonical, sent.gitRoot]).toEqual(["/srv/example/project", "/srv/example/project"]);
    const absent = toSession("", "k", view("a", { git_root: "" }));
    expect([absent.cwdCanonical, absent.gitRoot]).toEqual(["", ""]);
  });
  it("derives the presentation status from status kinds and raw phases", () => {
    expect(statusOf(view("a", { status: "working" }))).toBe("working");
    expect(statusOf(view("a", { status: "", phase: "approval" }))).toBe("attention");
    expect(statusOf(view("a", { status: "busy", phase: "" }))).toBe("working");
    expect(statusOf(view("a", { status: "", phase: "tool" }))).toBe("working");
    expect(statusOf(view("a", { attention: true }))).toBe("attention");
    expect(toSession("", "run:a", view("a", { unread: true })).caption).toBe("New reply");
    expect(toSession("", "run:a", view("a", { status: "Retry in 5 s", phase: "" })).caption).toBe("Retry in 5 s");
    expect(toSession("", "run:a", view("claude/p/a", { title: "" })).title).toBe("claude/p/a");
    // The core's own fields: its caption, the attention reason and the run identity for follow.
    const asking = toSession("", "run:a", view("a", { status: "attention", attention: "question", caption: "Needs input", run_id: "a", conversation_id: "c", turn_started: 5 }));
    expect([asking.status, asking.attention, asking.caption, asking.runId, asking.conversationId, asking.turnStarted]).toEqual(["attention", "question", "Needs input", "a", "c", 5]);
  });

  it("copies what the actions read as flat values, never the core's row", () => {
    const s = toSession("", "run:a", view("dsh/sample-project/a", {
      agent: "deepseek", state: "paused", tracked: true, resumable: true, activity: "idle", process_state: "exited", archive_id: null,
      reply_id: "r1", attention_signature: "s1", shared_read: true, allowed_actions: ["resume", "rename"],
      action_reasons: { archive: "No archive", terminate: "Use Pause", forget: "No forget", pause: "Not live" },
    }));
    expect(s).toMatchObject({
      agent: "deepseek", cmd: "deepseek", state: "paused", tracked: true, resumable: true, activity: "idle", processState: "exited",
      archiveId: undefined, replyId: "r1", attentionSignature: "s1", sharedRead: true, allowed: "resume rename",
      archiveReason: "No archive", terminateReason: "Use Pause", forgetReason: "No forget",
    });
    expect(Object.entries(s).filter(([key]) => key !== "subagents").some(([, v]) => typeof v === "object" && v !== null)).toBe(false);
    expect(s.subagents).toBeUndefined();
    expect(toSession("", "run:a", view("a", { allowed_actions: null })).allowed).toBeUndefined();
    expect(toSession("", "run:a", view("a", { allowed_actions: [] })).allowed).toBe("");
    expect(toSession("", "run:a", view("x/a", { agent: "sh" }))).toMatchObject({ agent: "sh", cmd: "sh" });
  });
});

describe("setDraftKeys", () => {
  it("marks the rows with a nonempty draft, also rows that arrive later or change", () => {
    createRoot((dispose) => {
      const { catalog, applyBatch, setDraftKeys } = createCatalog();
      applyBatch(batchOf({ replaces: [{ computer: "mac", rows: [row("run:a", "claude/p/a", {}, "mac"), row("run:b", "claude/p/b", {}, "mac")] }] }));
      // Draft keys are `computer + "\n" + name`; an archived draft's key also names its archive.
      setDraftKeys(["mac\nclaude/p/a", "mac\nclaude/p/c", "mac\nclaude/p/b\narchive-1"]);
      expect(catalog.rows[rowId("mac", "run:a")].draft).toBe(true);
      expect(catalog.rows[rowId("mac", "run:b")].draft).toBe(false);
      // An update keeps the mark; a row listed later takes it.
      applyBatch(batchOf({ upserts: [row("run:a", "claude/p/a", { summary: "Bash: cargo test" }, "mac"), row("run:c", "claude/p/c", {}, "mac")] }));
      expect(catalog.rows[rowId("mac", "run:a")].draft).toBe(true);
      expect(catalog.rows[rowId("mac", "run:c")].draft).toBe(true);
      setDraftKeys([]);
      expect(catalog.rows[rowId("mac", "run:a")].draft).toBe(false);
      expect(catalog.rows[rowId("mac", "run:c")].draft).toBe(false);
      dispose();
    });
  });
});
