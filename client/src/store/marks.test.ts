import { beforeEach, describe, expect, it, vi } from "vitest";
import { createComputed, createRoot } from "solid-js";
import { createCatalog } from "./catalog";
import { CAP, attentionKey, byCodePoint, createMarks, fallbackKey, replyKey, type AttentionSnapshot, type Mark, type MarksPort, type MigratedStorage, type RowsPort } from "./marks";
import { snapshotOf } from "../ui/sessions/actions-ui";
import { emptyBatch, type Batch, type ReadMark, type RowOut, type ViewRow } from "../types";
import overlayFixture from "../../core/tests/fixtures/marks-overlay.json";
import { attentionNeeded, needsAttention, visibleUnreadReply } from "./attention";

// The marks store against Qt's FleetState (updateUnread, markSessionRead,
// setReviewLater, unreadReplies, markRepliesRead, legacyReadMarks), with a
// mocked bridge: nothing here runs hgs.

const NAME = "claude/sample-project/a";
const FP = JSON.stringify(["run-1", "", "", null, {}]);
const ASK = JSON.stringify(["run-1", "approval", "q1", null, {}]);

/** An idle shared-read row with an unread reply. */
const view = (extra: Partial<ViewRow> = {}): ViewRow => ({
  name: NAME, agent: "claude", status: "ready", caption: "Ready", phase: "idle", state: "running", tracked: true, activity: "idle",
  run_id: "run-1", conversation_id: "conv-1", created: 1_790_000_000, attention_fingerprint: FP, needs_action: false,
  acknowledged_status: "ready", acknowledged_caption: "Ready", unread_candidate: true, unread: false, summary: "",
  reply_id: "r1", attention_signature: null, shared_read: true, read_reply_id: null, read_attention_signature: null, ...extra,
});
/** A row that asks for approval, with its host signature. */
const asking = (extra: Partial<ViewRow> = {}) => view({
  status: "attention", caption: "Needs approval", phase: "approval", attention: "approval", needs_action: true,
  acknowledged_status: "ready", acknowledged_caption: "Needs approval", attention_fingerprint: ASK, attention_signature: "s1",
  summary: "Needs approval: rm -rf scratch", ...extra,
});
const rowOut = (extra: Partial<ViewRow> = {}, key = "run:a", computer = ""): RowOut => ({ computer, key, row: view(extra) });
const batchOf = (part: Partial<Batch>): Batch => ({ ...emptyBatch(), ...part });
const flush = async () => { for (let i = 0; i < 10; i++) await Promise.resolve(); };
const id = (key = "run:a", computer = "") => `${computer}\n${key}`;
const deferred = <T>() => {
  let resolve: (value: T) => void = () => {};
  const promise = new Promise<T>((r) => { resolve = r; });
  return { promise, resolve };
};
const mark = (extra: Partial<ReadMark> = {}): ReadMark => ({
  computer: "", name: NAME, run_id: "run-1", conversation_id: "conv-1", reply_id: "r1", attention_signature: null, ...extra,
});

/** A port that answers as the Rust side: queued marks merge by computer and name. */
function fakePort(snapshot: AttentionSnapshot = { marks: {}, replies: {} }, pending: ReadMark[] = []) {
  const server = new Map<string, ReadMark>(pending.map((m) => [JSON.stringify([m.computer, m.name]), m]));
  const port = {
    attentionMarks: vi.fn(async () => snapshot),
    updateAttentionMarks: vi.fn(async (_marks: Record<string, Mark | null>, _replies: Record<string, string | null>) => {}),
    queueReadMarks: vi.fn(async (marks: ReadMark[]): Promise<ReadMark[] | undefined> => {
      for (const next of marks) {
        const key = JSON.stringify([next.computer, next.name]);
        const old = server.get(key);
        const same = old && old.run_id === next.run_id && old.conversation_id === next.conversation_id;
        server.set(key, same ? { ...next, reply_id: next.reply_id ?? old.reply_id, attention_signature: next.attention_signature ?? old.attention_signature } : next);
      }
      return [...server.values()];
    }),
    pendingReadMarks: vi.fn(async (): Promise<ReadMark[] | undefined> => [...server.values()]),
  } satisfies MarksPort;
  return port;
}

function memoryStorage(initial: string[] = []): MigratedStorage & { list: string[] } {
  const s = { list: [...initial], load: () => [...s.list], save: (list: string[]) => { s.list = [...list]; } };
  return s;
}

async function setup(rows: RowOut[] = [rowOut()], port = fakePort(), storage = memoryStorage()) {
  const marks = createMarks(port, storage);
  const catalog = createCatalog(marks);
  catalog.applyBatch(batchOf({ replaces: [{ computer: "", rows: rows.filter((r) => r.computer === "") }, ...[...new Set(rows.filter((r) => r.computer).map((r) => r.computer))].map((computer) => ({ computer, rows: rows.filter((r) => r.computer === computer) }))] }));
  await marks.load();
  await flush();
  const receive = (b: Batch) => { catalog.applyBatch(b); marks.observe(b); };
  return { marks, port, storage, ...catalog, receive, row: (key = "run:a", computer = "") => catalog.catalog.rows[id(key, computer)] };
}

const keyOf = (extra: Partial<{ computer: string; cmd: string; name: string; created: number; conversationId: string }> = {}) =>
  attentionKey({ computer: "", cmd: "claude", name: NAME, created: 1_790_000_000, conversationId: "conv-1", ...extra });
const REPLY = replyKey("", "claude", "conv-1");

beforeEach(() => vi.restoreAllMocks());

describe("keys", () => {
  it("are Qt's: the reply key, else the name and created without a conversation", () => {
    expect(REPLY).toBe(JSON.stringify(["", "claude", "conv-1"]));
    expect(keyOf()).toBe(REPLY);
    expect(keyOf({ conversationId: "" })).toBe(JSON.stringify(["", "claude", NAME, 1_790_000_000]));
    expect(fallbackKey({ computer: "mac", cmd: "codex", name: "codex/p/x", created: 5 })).toBe(JSON.stringify(["mac", "codex", "codex/p/x", 5]));
  });

  it("sort as Rust strings do (UTF-8 bytes), not as UTF-16 units", () => {
    const keys = ["\u{1F600}", "\uFFFD", "b", "a", "ab", "\u00E9"];
    expect([...keys].sort(byCodePoint)).toEqual(["a", "ab", "b", "\u00E9", "\uFFFD", "\u{1F600}"]);
    expect([...keys].sort()).not.toEqual([...keys].sort(byCodePoint));
  });
});

describe("overlay", () => {
  it("shared read: a reply is unread until the host's mark or a pending mark of this run reads it, and blanks the summary meanwhile", async () => {
    const a = await setup([rowOut({ summary: "Ready" })]);
    expect(a.row()).toMatchObject({ unread: true, summary: "" });
    a.receive(batchOf({ upserts: [rowOut({ summary: "Ready", read_reply_id: "r1" })] }));
    expect(a.row()).toMatchObject({ unread: false, summary: "Ready" });

    const b = await setup([rowOut()], fakePort(undefined, [mark()]));
    expect(b.row().unread).toBe(false);
  });

  it("ignores a pending mark for another run or conversation", async () => {
    expect((await setup([rowOut()], fakePort(undefined, [mark({ run_id: "run-0" })]))).row().unread).toBe(true);
    expect((await setup([rowOut()], fakePort(undefined, [mark({ conversation_id: "conv-0" })]))).row().unread).toBe(true);
    expect((await setup([rowOut()], fakePort(undefined, [mark({ reply_id: "r0" })]))).row().unread).toBe(true);
  });

  it("shared read: a signed attention is acknowledged by the host's mark or a pending one, never by a local mark", async () => {
    // This computer's older marks were queued once already (see "legacy read marks").
    const open = await setup([{ computer: "", key: "run:a", row: asking() }], fakePort({ marks: { [keyOf()]: { review: false, acknowledged: ASK } }, replies: {} }), memoryStorage([""]));
    expect(open.row()).toMatchObject({ status: "attention", caption: "Needs approval", attention: "approval" });

    const host = await setup([{ computer: "", key: "run:a", row: asking({ read_attention_signature: "s1", status: "ready" }) }]);
    expect(host.row()).toMatchObject({ status: "ready", caption: "Needs approval" });
    expect(host.row().attention).toBeUndefined();

    const queued = await setup([{ computer: "", key: "run:a", row: asking() }], fakePort(undefined, [mark({ reply_id: null, attention_signature: "s1" })]));
    expect(queued.row().status).toBe("ready");
    expect(queued.row().attention).toBeUndefined();
  });

  it("shared read: an attention without a signature is acknowledged by the local mark for its fingerprint", async () => {
    const recovery = { attention_signature: "", status: "attention", caption: "Needs attention", phase: "idle", attention: null, acknowledged_caption: "Needs attention" };
    const marked = await setup([{ computer: "", key: "run:a", row: asking(recovery) }], fakePort({ marks: { [keyOf()]: { review: false, acknowledged: ASK } }, replies: {} }));
    expect(marked.row()).toMatchObject({ status: "ready", caption: "Needs attention" });
    expect(marked.row().attention).toBeUndefined();
    const other = await setup([{ computer: "", key: "run:a", row: asking(recovery) }], fakePort({ marks: { [keyOf()]: { review: false, acknowledged: FP } }, replies: {} }));
    expect(other.row().status).toBe("attention");
  });

  it("older host: this device's read replies and acknowledgements decide", async () => {
    const legacy = { shared_read: false, attention_signature: undefined };
    expect((await setup([rowOut(legacy)])).row().unread).toBe(true);
    expect((await setup([rowOut(legacy)], fakePort({ marks: {}, replies: { [REPLY]: "r1" } }))).row().unread).toBe(false);
    expect((await setup([rowOut(legacy)], fakePort({ marks: {}, replies: { [REPLY]: "r0" } }))).row().unread).toBe(true);
    const ask = (await setup([{ computer: "", key: "run:a", row: asking(legacy) }], fakePort({ marks: { [keyOf()]: { review: false, acknowledged: ASK } }, replies: {} }))).row();
    expect(ask).toMatchObject({ status: "ready", caption: "Needs approval" });
  });

  it("review later comes from the mark and never applies to an archived row", async () => {
    const port = () => fakePort({ marks: { [keyOf()]: { review: true, acknowledged: "" } }, replies: {} });
    expect((await setup([rowOut()], port())).row().reviewLater).toBe(true);
    expect((await setup([rowOut({ state: "archived", unread_candidate: false })], port())).row().reviewLater).toBe(false);
  });

  it("keeps the summary of an unread reply while the session needs action, and a reply in progress is not shown unread", async () => {
    const a = await setup([{ computer: "", key: "run:a", row: asking() }]);
    expect(a.row()).toMatchObject({ unread: true, summary: "Needs approval: rm -rf scratch" });
    expect((await setup([rowOut({ unread_candidate: false, summary: "Working" })])).row()).toMatchObject({ unread: false, summary: "Working" });
  });

  it("keeps the row's own values for a projection without the read fields", async () => {
    const old = rowOut({ unread_candidate: undefined, attention_fingerprint: undefined, acknowledged_status: undefined, acknowledged_caption: undefined, shared_read: false, reply_id: undefined, unread: true });
    expect((await setup([old])).row()).toMatchObject({ unread: true, status: "ready", caption: "Ready" });
  });
});

describe("notification conflicts", () => {
  it("shows the core's kept preferences over a conflicting listing as a conflict", async () => {
    const a = await setup([rowOut({ muted: true, shared_notifications: true, notification_revision: 2, notification_error: "Conflicting shared notification preferences. Refresh the session." })]);
    expect(a.row()).toMatchObject({ muted: true, notificationConflict: true });
    a.receive(batchOf({ upserts: [rowOut({ muted: true, shared_notifications: true, notification_revision: 2 })] }));
    expect(a.row().notificationConflict).toBe(false);
  });
});

describe("the shared overlay fixture", () => {
  // client/core's menu-bar overlay (menubar::read_state) reads the same cases:
  // both apply this device's marks alike, and count with Qt's rules. The menu
  // bar counts SessionInfo::needsAttention (`counts`, `counts_stale`); the
  // window adds a subagent that asks, as Qt's SessionPresentation does (`window`).
  interface OverlayCase {
    case: string;
    computer: string;
    view: Partial<ViewRow>;
    marks: Record<string, Mark>;
    replies: Record<string, string>;
    pending: ReadMark[];
    expect: {
      review_later: boolean; unread_reply: boolean; unread: boolean; acknowledged: boolean;
      needs_attention: boolean; counts: boolean; counts_stale: boolean; window: boolean; window_stale: boolean;
      visible?: boolean; visible_stale?: boolean;
    };
  }
  const cases = (overlayFixture as unknown as { cases: OverlayCase[] }).cases;

  it("has the cases", () => expect(cases.length).toBeGreaterThanOrEqual(30));

  it.each(cases.map((c) => [c.case, c] as const))("%s", async (_, c) => {
    // This computer's older marks were queued once already (see "legacy read marks").
    const s = await setup([{ computer: c.computer, key: "k", row: c.view as ViewRow }], fakePort({ marks: c.marks, replies: c.replies }, c.pending), memoryStorage([c.computer]));
    const row = s.row("k", c.computer);
    expect(s.marks.readState(row)).toEqual({ reviewLater: c.expect.review_later, unreadReply: c.expect.unread_reply, unread: c.expect.unread, ack: c.expect.acknowledged });
    expect({ reviewLater: row.reviewLater, unread: row.unread, unreadReply: row.unreadReply, attentionNeeded: attentionNeeded(row) })
      .toEqual({ reviewLater: c.expect.review_later, unread: c.expect.unread, unreadReply: c.expect.unread_reply, attentionNeeded: c.expect.needs_attention });
    if (c.expect.visible !== undefined) expect(visibleUnreadReply(row, true)).toBe(c.expect.visible);
    if (c.expect.visible_stale !== undefined) expect(visibleUnreadReply(row, false)).toBe(c.expect.visible_stale);
    // A project header's request part (SessionsWindowRows.cpp:400-401): needsAction() && !attentionAcknowledged.
    expect(row.actionNeeded).toBe(!!c.view.needs_action && !c.expect.acknowledged);
    // FleetState::attentionSessions, as menubar::Desk::attention counts a row.
    const counts = (current: boolean) => !row.muted && (!!row.reviewLater || (current && attentionNeeded(row)));
    expect({ counts: counts(true), counts_stale: counts(false) }).toEqual({ counts: c.expect.counts, counts_stale: c.expect.counts_stale });
    // The window: online, and on an unreachable computer only Review later.
    expect({ window: needsAttention(row, true), window_stale: needsAttention(row, false) })
      .toEqual({ window: c.expect.window, window_stale: c.expect.window_stale });
  });
});

describe("markRead", () => {
  it("reads the reply and the attention locally first and queues one mark for them", async () => {
    const a = await setup([{ computer: "", key: "run:a", row: asking() }]);
    expect(a.marks.markRead(snapshotOf(a.row()))).toBe(true);
    expect(a.row()).toMatchObject({ unread: false, status: "ready", reviewLater: false });
    expect(a.port.queueReadMarks).toHaveBeenCalledWith([mark({ attention_signature: "s1" })]);
    expect(a.port.updateAttentionMarks).toHaveBeenCalledWith({ [keyOf()]: { review: false, acknowledged: ASK } }, { [REPLY]: "r1" });
    await flush();
    expect(a.marks.pending()).toEqual([mark({ attention_signature: "s1" })]);
    expect(a.row().unread).toBe(false);
  });

  it("shows a newer reply or question as unread again", async () => {
    const a = await setup([{ computer: "", key: "run:a", row: asking() }]);
    a.marks.markRead(snapshotOf(a.row()));
    await flush();
    a.receive(batchOf({ upserts: [{ computer: "", key: "run:a", row: asking({ reply_id: "r2" }) }] }));
    expect(a.row().unread).toBe(true);
    a.receive(batchOf({ upserts: [{ computer: "", key: "run:a", row: asking({ reply_id: "r2", attention_signature: "s2", attention_fingerprint: ASK.replace("q1", "q2") }) }] }));
    expect(a.row().status).toBe("attention");

    const legacy = { shared_read: false, attention_signature: undefined };
    const b = await setup([rowOut(legacy)]);
    b.marks.markRead(snapshotOf(b.row()));
    expect(b.row().unread).toBe(false);
    expect(b.port.queueReadMarks).not.toHaveBeenCalled();
    b.receive(batchOf({ upserts: [rowOut({ ...legacy, reply_id: "r2" })] }));
    expect(b.row().unread).toBe(true);
  });

  it("refuses a row whose fingerprint, reply, signature, created or state changed since the menu", async () => {
    const changes: Partial<ViewRow>[] = [
      { attention_fingerprint: ASK.replace("q1", "q2") }, { reply_id: "r2" }, { attention_signature: "s2" }, { created: 1_790_000_001 },
      { state: "archived" }, { conversation_id: "conv-2" }, { name: "claude/sample-project/b" },
    ];
    for (const change of changes) {
      const a = await setup([{ computer: "", key: "run:a", row: asking() }]);
      const menu = snapshotOf(a.row());
      a.receive(batchOf({ upserts: [{ computer: "", key: "run:a", row: asking(change) }] }));
      expect(a.marks.markRead(menu)).toBe(false);
      expect(a.port.queueReadMarks).not.toHaveBeenCalled();
      expect(a.port.updateAttentionMarks).not.toHaveBeenCalled();
    }
  });

  it("matches the listed row by name, conversation and created, not by the row id", async () => {
    const a = await setup([rowOut()]);
    const menu = { ...snapshotOf(a.row()), key: id("run:other") };
    expect(a.marks.markRead(menu)).toBe(true);
  });

  it("merges with the pending mark of the same run", async () => {
    const a = await setup([{ computer: "", key: "run:a", row: asking({ read_reply_id: "r1" }) }], fakePort(undefined, [mark({ reply_id: "r0" })]));
    a.marks.markRead(snapshotOf(a.row()));
    expect(a.port.queueReadMarks).toHaveBeenCalledWith([mark({ reply_id: "r0", attention_signature: "s1" })]);
  });

  it("queues nothing for a row without a run", async () => {
    const a = await setup([rowOut({ run_id: null })]);
    expect(a.marks.markRead(snapshotOf(a.row()))).toBe(true);
    expect(a.port.queueReadMarks).not.toHaveBeenCalled();
    expect(a.row().unread).toBe(true);
  });
});

describe("markReplyRead", () => {
  it("reads only the reply: the attention stays unacknowledged and one reply-only mark is queued", async () => {
    const a = await setup([{ computer: "", key: "run:a", row: asking() }]);
    expect(a.row()).toMatchObject({ unreadReply: true, status: "attention" });
    expect(a.marks.markReplyRead("", NAME, "conv-1", "r1")).toBe(true);
    expect(a.row()).toMatchObject({ unreadReply: false, status: "attention", caption: "Needs approval", attention: "approval" });
    expect(a.port.queueReadMarks).toHaveBeenCalledWith([mark()]);
    expect(a.port.updateAttentionMarks).toHaveBeenCalledWith({}, { [REPLY]: "r1" });
    await flush();
    expect(a.marks.pending()).toEqual([mark()]);
    expect(a.row().unreadReply).toBe(false);
  });

  it("keeps Review later", async () => {
    const a = await setup([rowOut()], fakePort({ marks: { [keyOf()]: { review: true, acknowledged: "" } }, replies: {} }));
    expect(a.row()).toMatchObject({ unread: true, reviewLater: true });
    expect(a.marks.markReplyRead("", NAME, "conv-1", "r1")).toBe(true);
    expect(a.row()).toMatchObject({ unread: false, unreadReply: false, reviewLater: true });
    expect(a.port.updateAttentionMarks).toHaveBeenCalledWith({}, { [REPLY]: "r1" });
  });

  it("refuses another computer, name, conversation or reply, an empty identity, a read reply and an archived row", async () => {
    const refused: [Partial<ViewRow>, [string, string, string, string]][] = [
      [{}, ["mac", NAME, "conv-1", "r1"]],
      [{}, ["", "claude/sample-project/b", "conv-1", "r1"]],
      [{}, ["", NAME, "conv-2", "r1"]],
      [{}, ["", NAME, "conv-1", "r0"]],
      [{}, ["", NAME, "conv-1", ""]],
      [{ conversation_id: null }, ["", NAME, "", "r1"]],
      [{ read_reply_id: "r1" }, ["", NAME, "conv-1", "r1"]],
      [{ state: "archived" }, ["", NAME, "conv-1", "r1"]],
    ];
    for (const [extra, args] of refused) {
      const a = await setup([rowOut(extra)]);
      expect(a.marks.markReplyRead(...args)).toBe(false);
      expect(a.port.queueReadMarks).not.toHaveBeenCalled();
      expect(a.port.updateAttentionMarks).not.toHaveBeenCalled();
    }
  });

  it("never reads the next reply that arrived after the view", async () => {
    const a = await setup([rowOut()]);
    a.receive(batchOf({ upserts: [rowOut({ reply_id: "r2" })] }));
    expect(a.marks.markReplyRead("", NAME, "conv-1", "r1")).toBe(false);
    expect(a.row().unread).toBe(true);
  });

  it("keeps an older host's read reply on this device and queues nothing", async () => {
    const legacy = { shared_read: false, attention_signature: undefined };
    const a = await setup([rowOut(legacy, "run:d", "mac")]);
    expect(a.marks.markReplyRead("mac", NAME, "conv-1", "r1")).toBe(true);
    expect(a.row("run:d", "mac").unread).toBe(false);
    expect(a.port.queueReadMarks).not.toHaveBeenCalled();
    expect(a.port.updateAttentionMarks).toHaveBeenCalledWith({}, { [replyKey("mac", "claude", "conv-1")]: "r1" });
    a.receive(batchOf({ upserts: [rowOut({ ...legacy, reply_id: "r2" }, "run:d", "mac")] }));
    expect(a.row("run:d", "mac").unread).toBe(true);
  });

  it("reads a reply not shown unread yet (a session with background work), as Qt's unreadReply does", async () => {
    const a = await setup([rowOut({ unread_candidate: false, status: "background" })]);
    expect(a.row()).toMatchObject({ unread: false, unreadReply: true });
    expect(a.marks.markReplyRead("", NAME, "conv-1", "r1")).toBe(true);
    expect(a.row().unreadReply).toBe(false);
    expect(a.port.queueReadMarks).toHaveBeenCalledWith([mark()]);
  });
});

describe("affected rows only", () => {
  it("marking one row patches that row and leaves the other untouched", async () => {
    await createRoot(async (dispose) => {
      const marks = createMarks(fakePort(), memoryStorage([""]));
      const attach = marks.attach;
      const patched: string[] = [];
      marks.attach = (rows: RowsPort) => attach({ ...rows, patch: (rowId, fields) => { patched.push(rowId); rows.patch(rowId, fields); } });
      const catalog = createCatalog(marks);
      catalog.applyBatch(batchOf({ replaces: [{ computer: "", rows: [rowOut(), rowOut({ name: "claude/sample-project/b", run_id: "run-2", conversation_id: "conv-2", reply_id: "rb" }, "run:b")] }] }));
      await marks.load();
      await flush();
      const b = catalog.catalog.rows[id("run:b")];
      let bReads = 0;
      createComputed(() => { void [b.unread, b.status, b.reviewLater, b.summary]; bReads++; });
      bReads = 0;
      patched.length = 0;

      marks.markRead(snapshotOf(catalog.catalog.rows[id()]));
      marks.markNeedsAttention(snapshotOf(catalog.catalog.rows[id()]));
      await flush();

      expect(new Set(patched)).toEqual(new Set([id()]));
      expect(bReads).toBe(0);
      expect(catalog.catalog.rows[id("run:b")]).toBe(b);
      dispose();
    });
  });
});

describe("markNeedsAttention", () => {
  it("sets the review mark and keeps the acknowledgement", async () => {
    const a = await setup([rowOut()], fakePort({ marks: { [keyOf()]: { review: false, acknowledged: FP } }, replies: {} }));
    expect(a.marks.markNeedsAttention(snapshotOf(a.row()))).toBe(true);
    expect(a.row().reviewLater).toBe(true);
    expect(a.port.updateAttentionMarks).toHaveBeenCalledWith({ [keyOf()]: { review: true, acknowledged: FP } }, {});
    // Writing the same mark again changes nothing and sends nothing.
    a.port.updateAttentionMarks.mockClear();
    a.marks.markNeedsAttention(snapshotOf(a.row()));
    expect(a.port.updateAttentionMarks).not.toHaveBeenCalled();
    // Mark as read clears it.
    a.marks.markRead(snapshotOf(a.row()));
    expect(a.row().reviewLater).toBe(false);
  });

  it("refuses a row that is gone or archived", async () => {
    const a = await setup([rowOut()]);
    const menu = snapshotOf(a.row());
    a.receive(batchOf({ upserts: [rowOut({ state: "archived" })] }));
    expect(a.marks.markNeedsAttention(menu)).toBe(false);
    a.receive(batchOf({ removes: [{ computer: "", key: "run:a" }] }));
    expect(a.marks.markNeedsAttention(menu)).toBe(false);
    expect(a.port.updateAttentionMarks).not.toHaveBeenCalled();
  });
});

describe("unreadReplies and markAllRead", () => {
  const rows = () => [
    rowOut({}, "run:a"),
    rowOut({ name: "claude/sample-project/b", run_id: "run-2", conversation_id: "conv-2", reply_id: "rb" }, "run:b"),
    rowOut({ name: "claude/sample-project/c", run_id: "run-3", conversation_id: "conv-3", reply_id: "rc", read_reply_id: "rc" }, "run:c"),
    rowOut({ name: "codex/sample-project/d", agent: "codex", run_id: "run-4", conversation_id: "conv-4", reply_id: "rd", shared_read: false }, "run:d", "mac"),
    // A reply still in progress counts, as Qt's unreadReply does, though it does not show yet.
    rowOut({ name: "claude/sample-project/e", run_id: "run-5", conversation_id: "conv-5", reply_id: "re", unread_candidate: false }, "run:e"),
  ];
  const review = () => fakePort({ marks: { [replyKey("", "claude", "conv-2")]: { review: true, acknowledged: "" } }, replies: {} });

  it("lists every unread reply not marked for review", async () => {
    const a = await setup(rows(), review());
    expect(a.marks.unreadReplies()).toEqual(new Map([
      [REPLY, "r1"], [replyKey("mac", "codex", "conv-4"), "rd"], [replyKey("", "claude", "conv-5"), "re"],
    ]));
  });

  it("marks them, skips review rows, queues the shared hosts' reply marks and counts", async () => {
    const a = await setup(rows(), review());
    expect(a.marks.markAllRead()).toBe(3);
    expect(a.row("run:a").unread).toBe(false);
    expect(a.row("run:d", "mac").unread).toBe(false);
    expect(a.row("run:b")).toMatchObject({ unread: true, reviewLater: true });
    expect(a.port.queueReadMarks).toHaveBeenCalledTimes(1);
    expect(a.port.queueReadMarks).toHaveBeenCalledWith([
      mark(), mark({ name: "claude/sample-project/e", run_id: "run-5", conversation_id: "conv-5", reply_id: "re" }),
    ]);
    expect(a.port.updateAttentionMarks).toHaveBeenCalledWith({}, {
      [REPLY]: "r1", [replyKey("mac", "codex", "conv-4")]: "rd", [replyKey("", "claude", "conv-5")]: "re",
    });
    expect(a.marks.unreadReplies().size).toBe(0);
    expect(a.marks.markAllRead()).toBe(0);
  });

  it("queues exactly the ReadMark fields the Rust worker reads", async () => {
    const a = await setup([rowOut({}, "run:a")]);
    a.marks.markAllRead();
    expect(JSON.stringify(a.port.queueReadMarks.mock.calls[0][0])).toBe(
      '[{"computer":"","name":"claude/sample-project/a","run_id":"run-1","conversation_id":"conv-1","reply_id":"r1","attention_signature":null}]',
    );
  });

  it("makes the footer state follow a batch and Mark all as read", async () => {
    await createRoot(async (dispose) => {
      const a = await setup([rowOut({}, "run:a")]);
      const seen: number[] = [];
      createComputed(() => { seen.push(a.marks.unreadReplies().size); });
      expect(seen).toEqual([1]);
      a.receive(batchOf({ upserts: [rowOut({ name: "claude/sample-project/b", run_id: "run-2", conversation_id: "conv-2", reply_id: "rb" }, "run:b")] }));
      expect(seen.at(-1)).toBe(2);
      a.marks.markAllRead();
      expect(seen.at(-1)).toBe(0);
      dispose();
    });
  });

  it("keeps the reply of a row marked for review after the snapshot unread", async () => {
    const a = await setup(rows(), review());
    const snapshot = a.marks.unreadReplies();
    a.marks.markNeedsAttention(snapshotOf(a.row("run:a")));
    expect(a.marks.markAllRead(snapshot)).toBe(2);
    expect(a.row("run:a")).toMatchObject({ unread: true, reviewLater: true });
  });

  it("never marks a reply newer than the snapshot", async () => {
    const a = await setup(rows(), review());
    const snapshot = a.marks.unreadReplies();
    a.receive(batchOf({ upserts: [rowOut({ reply_id: "r2" }, "run:a")] }));
    expect(a.marks.markAllRead(snapshot)).toBe(2);
    expect(a.row("run:a").unread).toBe(true);
  });
});

describe("persistence", () => {
  it("waits for the stored marks before its first update, and a field written meanwhile wins", async () => {
    const port = fakePort();
    const stored = deferred<AttentionSnapshot>();
    port.attentionMarks.mockReturnValue(stored.promise);
    const marks = createMarks(port, memoryStorage([""]));
    const { applyBatch, catalog } = createCatalog(marks);
    applyBatch(batchOf({ replaces: [{ computer: "", rows: [rowOut()] }] }));
    const loading = marks.load();
    expect(marks.markNeedsAttention(snapshotOf(catalog.rows[id()]))).toBe(true);
    expect(catalog.rows[id()].reviewLater).toBe(true);
    expect(port.updateAttentionMarks).not.toHaveBeenCalled();
    stored.resolve({ marks: { [keyOf()]: { review: false, acknowledged: "old" }, other: { review: true, acknowledged: "" } }, replies: { [REPLY]: "r1" } });
    await loading;
    expect(port.updateAttentionMarks).toHaveBeenCalledTimes(1);
    // Mark needs attention sets only `review`; the stored acknowledgement stays (setReviewLater).
    expect(port.updateAttentionMarks).toHaveBeenCalledWith({ [keyOf()]: { review: true, acknowledged: "old" } }, {});
    expect(catalog.rows[id()]).toMatchObject({ reviewLater: true, unread: true });
  });

  it("lets a Mark as read made before the load replace both fields", async () => {
    const port = fakePort();
    const stored = deferred<AttentionSnapshot>();
    port.attentionMarks.mockReturnValue(stored.promise);
    const marks = createMarks(port, memoryStorage([""]));
    const { applyBatch, catalog } = createCatalog(marks);
    applyBatch(batchOf({ replaces: [{ computer: "", rows: [rowOut()] }] }));
    const loading = marks.load();
    marks.markNeedsAttention(snapshotOf(catalog.rows[id()]));
    marks.markRead(snapshotOf(catalog.rows[id()]));
    stored.resolve({ marks: { [keyOf()]: { review: true, acknowledged: "old" } }, replies: {} });
    await loading;
    expect(port.updateAttentionMarks).toHaveBeenCalledWith({ [keyOf()]: { review: false, acknowledged: FP } }, { [REPLY]: "r1" });
    expect(catalog.rows[id()]).toMatchObject({ reviewLater: false, unread: false });
  });

  it("loads empty when the stored marks cannot be read", async () => {
    const port = fakePort();
    port.attentionMarks.mockRejectedValue("no such command");
    port.pendingReadMarks.mockRejectedValue("no such command");
    const a = await setup([rowOut()], port);
    expect(a.row().unread).toBe(true);
    expect(a.marks.markNeedsAttention(snapshotOf(a.row()))).toBe(true);
    expect(port.updateAttentionMarks).toHaveBeenCalledTimes(1);
  });

  it("moves a mark under the fallback key to the conversation key once the row has one", async () => {
    const fallback = keyOf({ conversationId: "" });
    const a = await setup([rowOut({ conversation_id: null, reply_id: null })], fakePort({ marks: { [fallback]: { review: true, acknowledged: "" } }, replies: {} }));
    expect(a.row().reviewLater).toBe(true);
    expect(a.port.updateAttentionMarks).not.toHaveBeenCalled();
    // The row applied with its conversation already finds the mark, before the move.
    const withConversation = batchOf({ upserts: [rowOut()] });
    a.applyBatch(withConversation);
    expect(a.row().reviewLater).toBe(true);
    a.marks.observe(withConversation);
    expect(a.row().reviewLater).toBe(true);
    expect(a.port.updateAttentionMarks).toHaveBeenCalledWith({ [keyOf()]: { review: true, acknowledged: "" }, [fallback]: null }, {});
    // The conversation key wins over a fallback mark that is still there.
    const b = await setup([rowOut()], fakePort({ marks: { [fallback]: { review: true, acknowledged: "" }, [keyOf()]: { review: false, acknowledged: "" } }, replies: {} }));
    expect(b.row().reviewLater).toBe(false);
    expect(b.port.updateAttentionMarks).toHaveBeenCalledWith({ [fallback]: null }, {});
  });

  it(`keeps the ${CAP} newest written entries of each map`, async () => {
    expect(CAP).toBe(20_000);
    const legacy = { shared_read: false, attention_signature: undefined };
    const replies: Record<string, string> = { [REPLY]: "r1" };
    const marks: Record<string, Mark> = { [keyOf()]: { review: true, acknowledged: "" } };
    for (let i = 1; i < CAP; i++) { replies[`filler-${i}`] = "x"; marks[`filler-${i}`] = { review: true, acknowledged: "" }; }
    const rows = [rowOut(legacy), rowOut({ ...legacy, name: "claude/sample-project/b", run_id: "run-2", conversation_id: "conv-2", reply_id: "rb" }, "run:b")];
    const a = await setup(rows, fakePort({ marks, replies }));
    expect(a.row()).toMatchObject({ unread: false, reviewLater: true });
    // One more read reply and one more mark: the oldest written of each map goes.
    a.marks.markRead(snapshotOf(a.row("run:b")));
    expect(a.row("run:b")).toMatchObject({ unread: false });
    expect(a.row()).toMatchObject({ unread: true, reviewLater: false });
    // The Rust side caps its own copy the same way; no removal is sent.
    expect(a.port.updateAttentionMarks).toHaveBeenCalledWith(
      { [replyKey("", "claude", "conv-2")]: { review: false, acknowledged: FP } }, { [replyKey("", "claude", "conv-2")]: "rb" });
  });
});

describe("write order", () => {
  const legacy = { shared_read: false, attention_signature: undefined };
  const conv = (n: number, key = `run:${n}`) => rowOut({ ...legacy, name: `claude/sample-project/s${n}`, run_id: `run-${n}`, conversation_id: `conv-${n}`, reply_id: `r${n}` }, key);
  const markKey = (n: number) => replyKey("", "claude", `conv-${n}`);

  it("loads in the stored write order, so the next write at the cap drops the oldest written, as the Rust side does", async () => {
    // conv-2 was written first and conv-1 last; by key, conv-1 sorts first.
    const order = [markKey(2), ...Array.from({ length: CAP - 2 }, (_, i) => `filler-${i}`), markKey(1)];
    const marks = Object.fromEntries([...order].sort().map((k) => [k, { review: true, acknowledged: "" }]));
    const a = await setup([conv(1), conv(2), conv(3)], fakePort({ marks, replies: {}, order: { marks: order, replies: [] } }));
    expect(a.row("run:1").reviewLater).toBe(true);
    expect(a.row("run:2").reviewLater).toBe(true);
    a.marks.markNeedsAttention(snapshotOf(a.row("run:3")));
    expect(a.row("run:2").reviewLater).toBe(false);
    expect(a.row("run:1").reviewLater).toBe(true);
    expect(a.marks.writeOrder().marks.slice(-2)).toEqual([markKey(1), markKey(3)]);
  });

  it("keeps entries the order does not list, as the oldest", async () => {
    const a = await setup([conv(1)], fakePort({ marks: { [markKey(1)]: { review: true, acknowledged: "" }, stray: { review: true, acknowledged: "" } }, replies: {}, order: { marks: [markKey(1)], replies: [] } }));
    expect(a.marks.writeOrder().marks).toEqual(["stray", markKey(1)]);
    expect(a.row("run:1").reviewLater).toBe(true);
  });

  it("an early change equal to the stored value sends nothing and keeps its place", async () => {
    const port = fakePort();
    const stored = deferred<AttentionSnapshot>();
    port.attentionMarks.mockReturnValue(stored.promise);
    const m = createMarks(port, memoryStorage([""]));
    const { applyBatch, catalog } = createCatalog(m);
    applyBatch(batchOf({ replaces: [{ computer: "", rows: [conv(1)] }] }));
    const loading = m.load();
    m.markNeedsAttention(snapshotOf(catalog.rows[id("run:1")]));
    stored.resolve({ marks: { [markKey(1)]: { review: true, acknowledged: "x" }, later: { review: true, acknowledged: "" } }, replies: {}, order: { marks: [markKey(1), "later"], replies: [] } });
    await loading;
    expect(port.updateAttentionMarks).not.toHaveBeenCalled();
    expect(m.writeOrder().marks).toEqual([markKey(1), "later"]);
  });

  it("makes one change's entries the newest in key order, as one Rust update applies them", async () => {
    // Rows in catalog order conv-9, conv-1: Rust applies the delta's keys sorted.
    const a = await setup([conv(9), conv(1)], fakePort({ marks: {}, replies: { old: "x" }, order: { marks: [], replies: ["old"] } }));
    expect(a.marks.markAllRead()).toBe(2);
    expect(a.marks.writeOrder().replies).toEqual(["old", markKey(1), markKey(9)]);
  });

  it("orders one change's keys as Rust strings, also beyond the basic plane", async () => {
    const wide = (n: string, key: string) => rowOut({ ...legacy, name: `claude/sample-project/${key}`, run_id: key, conversation_id: `conv-${n}`, reply_id: "r" }, `run:${key}`);
    const a = await setup([wide("\uFFFD", "bmp"), wide("\u{1F600}", "astral")]);
    expect(a.marks.markAllRead()).toBe(2);
    expect(a.marks.writeOrder().replies).toEqual([replyKey("", "claude", "conv-\uFFFD"), replyKey("", "claude", "conv-\u{1F600}")]);
  });
});

describe("pending mirror", () => {
  it("drops a mark a batch row shows settled", async () => {
    const a = await setup([rowOut()], fakePort(undefined, [mark()]));
    a.receive(batchOf({ upserts: [rowOut({ read_reply_id: "r1" })] }));
    expect(a.marks.pending()).toEqual([]);
    // Without the mark, a host that lost its read mark shows the reply unread again.
    a.receive(batchOf({ upserts: [rowOut()] }));
    expect(a.row().unread).toBe(true);
  });

  it("drops a mark of another run, and keeps one that is still current", async () => {
    const a = await setup([rowOut()], fakePort(undefined, [mark()]));
    a.receive(batchOf({ upserts: [rowOut({ summary: "x" })] }));
    expect(a.marks.pending()).toEqual([mark()]);
    a.receive(batchOf({ upserts: [rowOut({ run_id: "run-2" })] }));
    expect(a.marks.pending()).toEqual([]);
  });

  it("drops a mark when a non-empty snapshot of its computer no longer lists the session, and never on an empty one", async () => {
    const a = await setup([rowOut()], fakePort(undefined, [mark()]));
    a.receive(batchOf({ replaces: [{ computer: "", rows: [] }] }));
    expect(a.marks.pending()).toEqual([mark()]);
    a.receive(batchOf({ replaces: [{ computer: "mac", rows: [rowOut({}, "run:a", "mac")] }] }));
    expect(a.marks.pending()).toEqual([mark()]);
    a.receive(batchOf({ replaces: [{ computer: "", rows: [rowOut({ name: "claude/sample-project/b" }, "run:b")] }] }));
    expect(a.marks.pending()).toEqual([]);
  });

  it("takes the queue's answer, but an older answer never undoes a newer mark", async () => {
    const rows = [rowOut(), rowOut({ name: "claude/sample-project/b", run_id: "run-2", conversation_id: "conv-2", reply_id: "rb" }, "run:b")];
    const a = await setup(rows);
    const original = a.port.queueReadMarks.getMockImplementation()!;
    const late = deferred<void>();
    // The first answer (only mark a) arrives after the second (a and b).
    a.port.queueReadMarks.mockImplementationOnce(async (marks) => { const answer = await original(marks); await late.promise; return answer; });
    a.marks.markRead(snapshotOf(a.row()));
    a.marks.markRead(snapshotOf(a.row("run:b")));
    await flush();
    expect(a.marks.pending().map((m) => m.name).sort()).toEqual([NAME, "claude/sample-project/b"]);
    late.resolve();
    await flush();
    expect(a.marks.pending().map((m) => m.name).sort()).toEqual([NAME, "claude/sample-project/b"]);
    expect(a.row("run:b").unread).toBe(false);
    expect(a.row().unread).toBe(false);
  });

  it("keeps its own marks when there is no app to answer (the browser build)", async () => {
    const a = await setup([rowOut()]);
    a.port.queueReadMarks.mockResolvedValueOnce(undefined);
    a.marks.markRead(snapshotOf(a.row()));
    await flush();
    expect(a.marks.pending()).toEqual([mark()]);
    expect(a.row().unread).toBe(false);
  });

  it("asks for the pending marks again when the queue fails", async () => {
    const a = await setup([rowOut()]);
    a.port.pendingReadMarks.mockClear();
    a.port.queueReadMarks.mockRejectedValueOnce("busy");
    a.port.pendingReadMarks.mockResolvedValueOnce([mark()]);
    a.marks.markRead(snapshotOf(a.row()));
    await flush();
    expect(a.port.pendingReadMarks).toHaveBeenCalledTimes(1);
    expect(a.row().unread).toBe(false);
  });
});

describe("legacy read marks", () => {
  const shared = (extra: Partial<ViewRow> = {}) => ({ computer: "mac", key: "run:a", row: asking({ agent: "codex", name: "codex/sample-project/a", ...extra }) });
  const stored = () => fakePort({
    marks: { [replyKey("mac", "codex", "conv-1")]: { review: false, acknowledged: ASK } },
    replies: { [replyKey("mac", "codex", "conv-1")]: "r1" },
  });
  const legacyMark = mark({ computer: "mac", name: "codex/sample-project/a", attention_signature: "s1" });

  it("are queued once when a host starts sharing read state", async () => {
    const storage = memoryStorage();
    const a = await setup([shared({ shared_read: false, attention_signature: undefined })], stored(), storage);
    expect(a.port.queueReadMarks).not.toHaveBeenCalled();
    a.receive(batchOf({ replaces: [{ computer: "mac", rows: [shared()] }] }));
    expect(a.port.queueReadMarks).toHaveBeenCalledWith([legacyMark]);
    expect(storage.list).toEqual(["mac"]);
    expect(a.row("run:a", "mac")).toMatchObject({ unread: false, status: "ready" });
    a.receive(batchOf({ replaces: [{ computer: "mac", rows: [shared()] }] }));
    expect(a.port.queueReadMarks).toHaveBeenCalledTimes(1);
  });

  it("are queued at load for a host already listed, and never for a host already migrated", async () => {
    expect((await setup([shared()], stored())).port.queueReadMarks).toHaveBeenCalledWith([legacyMark]);
    expect((await setup([shared()], stored(), memoryStorage(["mac"]))).port.queueReadMarks).not.toHaveBeenCalled();
  });

  it("cover only the values the host has not read yet", async () => {
    const a = await setup([shared({ read_reply_id: "r1", read_attention_signature: "s1" })], stored());
    expect(a.port.queueReadMarks).not.toHaveBeenCalled();
  });
});

// These reads belong to the service actor. A GUI may only overlay a committed
// exact identity; IPC availability, a local queue or a superseded row is not it.
describe("service read intents", () => {
  async function fixture(ready: boolean, extra: Partial<ViewRow> = {}) {
    const { createDesktopService } = await import("./desktopService");
    const service = createDesktopService();
    service.apply({ connected: true, epoch: "epoch", generation: 1,
      background: { reads: { ready, code: ready ? "ready" : "not_started" } },
      attention_counts: null, automatic_approvals: null, details_available: false, code: "background_unready" });
    const port = fakePort();
    const store = createMarks(port, memoryStorage(), service), data = createCatalog(store);
    data.applyBatch(batchOf({ upserts: [rowOut(extra)] }));
    await store.load(); await flush();
    return { port, store, ...data, row: () => data.catalog.rows[id()] };
  }
  it("shows retained exact local read overlays without claiming host acknowledgement or starting a writer", async () => {
    const f = await fixture(false, { local_read_reply: true, local_read_attention: true, attention_signature: "s1" });
    expect(f.row().unread).toBe(false);
    expect(f.row().readReplyId).toBeUndefined();
    expect(f.row().readAttentionSignature).toBeUndefined();
    expect(f.port.queueReadMarks).not.toHaveBeenCalled();
    f.applyBatch(batchOf({ upserts: [rowOut({ reply_id: "new-reply", attention_signature: "new-attention" })] }));
    expect(f.row().unread).toBe(true);
    expect(f.row().localReadReply).toBe(false);
    expect(f.row().localReadAttention).toBe(false);
    expect(f.port.queueReadMarks).not.toHaveBeenCalled();
  });
  it("does not migrate or mark locally when the service read component is unavailable", async () => {
    const f = await fixture(false);
    expect(f.store.markRead(snapshotOf(f.row()))).toBe(false);
    expect(f.row().unread).toBe(true);
    expect(f.port.queueReadMarks).not.toHaveBeenCalled();
  });
  it("reports service readiness without blaming a newer reply", async () => {
    const f = await fixture(false);
    expect(f.store.markReadUnavailable(snapshotOf(f.row()))).toBe("Read marks are unavailable until the background service is ready.");
    expect(f.port.queueReadMarks).not.toHaveBeenCalled();
    const missing = await fixture(true, { run_id: "" });
    expect(missing.store.markReadUnavailable(snapshotOf(missing.row()))).toBe("This host has no shared read state for this session.");
  });
  it("keeps local mark-read, dwell and bulk read for hosts without shared read state", async () => {
    const single = await fixture(false, { shared_read: false });
    expect(single.store.markReadUnavailable(snapshotOf(single.row()))).toBeUndefined();
    expect(single.store.markRead(snapshotOf(single.row()))).toBe(true);
    expect(single.row().unread).toBe(false);
    expect(single.port.queueReadMarks).not.toHaveBeenCalled();
    const dwell = await fixture(false, { shared_read: false });
    expect(dwell.store.markReplyRead("", NAME, "conv-1", "r1")).toBe(true);
    expect(dwell.row().unread).toBe(false);
    expect(dwell.port.queueReadMarks).not.toHaveBeenCalled();
    const all = await fixture(false, { shared_read: false });
    expect(all.store.markAllRead()).toBe(1);
    expect(all.row().unread).toBe(false);
    expect(all.port.queueReadMarks).not.toHaveBeenCalled();
  });
  it("overlays only after durable commit and coalesces repeated dwell/menu actions", async () => {
    const f = await fixture(true), commit = deferred<ReadMark[]>();
    f.port.queueReadMarks.mockImplementationOnce(() => commit.promise);
    const row = snapshotOf(f.row());
    expect(f.store.markRead(row)).toBe(true);
    expect(f.store.markRead(row)).toBe(false);
    expect(f.row().unread).toBe(true);
    expect(f.port.updateAttentionMarks).not.toHaveBeenCalled();
    commit.resolve([mark()]); await flush();
    expect(f.row().unread).toBe(false);
    expect(f.port.queueReadMarks).toHaveBeenCalledOnce();
  });
  it("does not acknowledge a newer reply while an older read is being committed", async () => {
    const f = await fixture(true), commit = deferred<ReadMark[]>();
    f.port.queueReadMarks.mockImplementationOnce(() => commit.promise);
    f.store.markRead(snapshotOf(f.row()));
    f.applyBatch(batchOf({ upserts: [rowOut({ reply_id: "r2" })] }));
    commit.resolve([mark()]); await flush();
    expect(f.row().replyId).toBe("r2");
    expect(f.row().unread).toBe(true);
    expect(f.port.updateAttentionMarks).not.toHaveBeenCalled();
  });
});
