import { describe, expect, it } from "vitest";
import { createJournal, PLACEHOLDER, type JournalState, type JournalScope, type ListRequest, type Page } from "./approvals-journal";

/** A promise the test settles. */
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
const settle = () => new Promise((done) => setTimeout(done, 0));
const entry = (id: number) => ({ id, at: 1_800_000_000_000, host: "desktop-a", session: "claude/sample/review", prompt: "May I run this command?", result: "Approved once" });

function harness() {
  const lists: { request: ListRequest; reply: ReturnType<typeof deferred<Page>> }[] = [];
  const details: { id: number; scope?: JournalScope; reply: ReturnType<typeof deferred<string>> }[] = [];
  let shown: JournalState | undefined;
  const filters = { host: "", session: "" };
  const journal = createJournal({
    list: (request) => { const reply = deferred<Page>(); lists.push({ request, reply }); return reply.promise; },
    detail: (id, scope) => { const reply = deferred<string>(); details.push({ id, scope, reply }); return reply.promise; },
  }, () => filters, (state) => { shown = state; });
  return { journal, lists, details, filters, shown: () => shown! };
}

describe("the approval journal", () => {
  it("carries the selected service journal and fixed high water through pages and details", async () => {
    const h = harness();
    const scope: JournalScope = { configuration: "a".repeat(64), epoch: "epoch-a", generation: 1, journal: "journal-a", through: 53, host: "", session: "" };
    h.journal.firstPage();
    h.lists[0].reply.resolve({ entries: [entry(53)], next: 4, previous: 0, scope });
    await settle();
    h.journal.select("53");
    expect(h.details[0].scope).toEqual(scope);
    h.journal.older();
    expect(h.lists[1].request.scope).toEqual(scope);
    expect(h.lists[1].request.before).toBe(4);
    h.lists[1].reply.resolve({ entries: [entry(3)], next: 0, previous: 3, scope });
    await settle();
    h.journal.newer();
    expect(h.lists[2].request.scope).toEqual(scope);
    expect(h.lists[2].request.after).toBe(3);
    // A first-page refresh/filter edit must not inherit the previous journal.
    h.journal.firstPage();
    h.lists[2].reply.resolve({ entries: [entry(53)], next: 4, previous: 0, scope });
    await settle();
    expect(h.lists[3].request.scope).toBeUndefined();
    h.details[0].reply.resolve("old journal detail");
    expect(h.shown().detail).toBeUndefined();
    h.lists[3].reply.reject("The approval journal changed. Refresh to read it again.");
    await settle();
    expect(h.shown().page.entries).toEqual([]);
    expect(h.shown().page.scope).toBeUndefined();
    expect(h.shown().status).toContain("Refresh");
  });
  it("reads one page at a time and once more for edits made meanwhile", async () => {
    const h = harness();
    h.journal.firstPage();
    h.filters.host = "d";
    h.journal.firstPage();
    h.filters.host = "desktop-a";
    h.journal.firstPage();
    expect(h.lists).toHaveLength(1);
    h.lists[0].reply.resolve({ entries: [entry(1)], next: 0, previous: 0 });
    await settle();
    // The first answer is dropped; one more read with the latest filters.
    expect(h.lists).toHaveLength(2);
    expect(h.lists[1].request.host).toBe("desktop-a");
    expect(h.shown().loading).toBe(true);
    h.lists[1].reply.resolve({ entries: [entry(2), entry(3)], next: 3, previous: 0 });
    await settle();
    expect(h.lists).toHaveLength(2);
    expect(h.shown().page.entries.map((e) => e.id)).toEqual([2, 3]);
    expect(h.shown().status).toBe("2 entries. Unconfirmed attempts require checking Activity or Terminal; they are not retried.");
  });

  it("moves by the journal's cursors and starts again from the first page", async () => {
    const h = harness();
    h.journal.firstPage();
    h.lists[0].reply.resolve({ entries: [entry(9)], next: 9, previous: 0 });
    await settle();
    h.journal.newer();
    expect(h.lists).toHaveLength(1);
    h.journal.older();
    expect(h.lists[1].request).toEqual({ host: "", session: "", before: 9, after: 0 });
    h.lists[1].reply.resolve({ entries: [entry(8)], next: 0, previous: 8 });
    await settle();
    h.journal.newer();
    expect(h.lists[2].request).toEqual({ host: "", session: "", before: 0, after: 8 });
    h.lists[2].reply.resolve({ entries: [entry(9)], next: 9, previous: 0 });
    await settle();
    h.journal.firstPage();
    expect(h.lists[3].request).toEqual({ host: "", session: "", before: 0, after: 0 });
  });

  it("leaves nothing to page or select after a failed read", async () => {
    const h = harness();
    h.journal.firstPage();
    h.lists[0].reply.resolve({ entries: [entry(9)], next: 9, previous: 4 });
    await settle();
    h.journal.older();
    h.lists[1].reply.reject("The automatic approval journal is unavailable. No new approval was authorized.");
    await settle();
    const state = h.shown();
    expect(state.status).toBe("The automatic approval journal is unavailable. No new approval was authorized.");
    expect(state.page).toEqual({ entries: [], next: 0, previous: 0 });
    h.journal.older();
    h.journal.newer();
    expect(h.lists).toHaveLength(2);
  });

  it("shows only the latest selection's detail", async () => {
    const h = harness();
    h.journal.firstPage();
    h.lists[0].reply.resolve({ entries: [entry(1), entry(2)], next: 0, previous: 0 });
    await settle();
    h.journal.select("1");
    expect(h.shown().detailHint).toBe("Loading question…");
    h.journal.select("2");
    h.details[0].reply.resolve("first");
    await settle();
    expect(h.shown().detail).toBeUndefined();
    h.details[1].reply.resolve("second");
    await settle();
    expect(h.shown().detail).toBe("second");
    // A new read forgets the selection and any answer still on its way.
    h.journal.select("1");
    h.journal.firstPage();
    h.details[2].reply.resolve("late");
    await settle();
    expect(h.shown().detail).toBeUndefined();
    expect(h.shown().detailHint).toBe(PLACEHOLDER);
  });
});
