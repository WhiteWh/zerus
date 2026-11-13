// @vitest-environment happy-dom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createComponent, createSignal, Show } from "solid-js";
import { render } from "solid-js/web";
import type { ActionRow } from "./actions-ui";
import type { ChildOffer } from "./end-children";

// zerus-sa9qn.17: the Terminate and Move to archive confirmations of a parent
// with live child sessions list them and offer to end them too, off by default.
const calls = vi.hoisted(() => ({
  offer: vi.fn(), cancel: vi.fn(), end: vi.fn(), perform: vi.fn(), notify: vi.fn(), endAfter: vi.fn(), refresh: vi.fn((out: unknown) => out),
  children: { ending: [], staying: [] } as ChildOffer, asked: [] as string[],
}));
vi.mock("../../bridge", () => ({ terminationCleanupOffer: calls.offer, terminationCleanupCancel: calls.cancel }));
vi.mock("../../store/actions", () => ({
  GONE: "gone", change: vi.fn(), copyCommand: vi.fn(), copyFolder: vi.fn(), copyName: vi.fn(), copyReference: vi.fn(),
  end: calls.end, forget: vi.fn(), interrupting: () => false, notify: calls.notify, perform: calls.perform, stillListed: () => true,
  pendingNotifications: () => ({ total: 0, conversation: false }), toggleMute: vi.fn(), updating: () => false,
  targetOf: (row: ActionRow) => ({ computer: row.computer, name: row.name, run_id: row.runId ?? "", conversation_id: row.conversationId ?? "" }),
}));
vi.mock("../../store/batch", () => ({ bindBatchConfirm: vi.fn(), endAfter: calls.endAfter, running: () => false }));
vi.mock("../../store/endChildren", () => ({ childOfferFor: (_row: ActionRow, a: string) => { calls.asked.push(a); return calls.children; } }));
vi.mock("../../store/settings", () => ({ setSetting: vi.fn() }));
vi.mock("../../store/sessionForks", () => ({ sessionForks: { available: () => false } }));
vi.mock("../../store/restores", () => ({ cancelRestoreFollow: vi.fn(), forgetArchived: vi.fn(), refreshArchiveWhenDone: calls.refresh, restoreSession: vi.fn() }));
vi.mock("../../i18n", () => ({ trNoop: (s: string) => s, tr: (s: string, ...args: unknown[]) => s.replace(/%(\d+)|%n/g, (token, n) => String(token === "%n" ? args[0] : args[Number(n) - 1])) }));
vi.mock("../lib/icons", () => ({ Icon: () => null }));

import { ActionDialogs, ArchiveDialog, TerminateDialog, actionEntries } from "./ActionDialogs";
import { toSession } from "../../store/catalog";
import { selectionSnapshot, type SelectionRow } from "./selection";
import type { ViewRow } from "../../types";

let dispose: (() => void) | undefined;
const flush = async () => { for (let i = 0; i < 10; i++) await Promise.resolve(); };
const done = { outcome: "done", receipt: {}, cleanup: null };
beforeEach(() => {
  vi.clearAllMocks(); calls.asked = [];
  calls.offer.mockRejectedValue("Unexpected worktree check");
  calls.end.mockResolvedValue(done); calls.perform.mockResolvedValue(done);
  calls.children = { ending: [], staying: [] };
});
afterEach(() => { dispose?.(); dispose = undefined; document.body.replaceChildren(); });

const session = (tag: string, extra: Partial<ViewRow> = {}) => toSession("", `run:${tag}`, {
  name: `kimi/zerus/${tag}`, agent: "kimi", state: "running", tracked: true, run_id: `run-${tag}`, conversation_id: `conversation-${tag}`,
  resumable: true, activity: "idle", machine: "fixture", git_worktree: false, ...extra,
});
const child = (tag: string, extra: Partial<ViewRow> = {}): SelectionRow => selectionSnapshot(session(tag, extra));

function mount(Dialog: typeof TerminateDialog, row: ActionRow) {
  const host = document.createElement("div"); document.body.append(host);
  const [open, setOpen] = createSignal<ActionRow | undefined>(row);
  dispose = render(() => createComponent(Show, { get when() { return open(); }, keyed: true,
    children: (captured) => createComponent(Dialog, { row: captured as ActionRow, close: () => setOpen(undefined) }),
  }), host);
}
const box = () => document.querySelector<HTMLInputElement>('.dlg-child-sessions input[type="checkbox"]');
const listed = () => [...document.querySelectorAll(".dlg-child-sessions li")].map((li) => li.textContent);
const button = (text: string) => [...document.querySelectorAll("button")].find((b) => b.textContent === text)!;

describe("Terminate of a parent with live child sessions", () => {
  it("lists them and offers to end them too, off by default; confirming without it ends only the parent", async () => {
    calls.children = { ending: [child("worker-a"), child("worker-b")], staying: [] };
    const row = selectionSnapshot(session("lead"));
    mount(TerminateDialog, row); await flush();
    expect(calls.asked).toEqual(["terminate"]);
    expect(box()?.checked).toBe(false);
    expect(box()?.parentElement?.textContent?.trim()).toBe("Also end 2 child sessions");
    expect(listed()).toEqual(["kimi/zerus/worker-a", "kimi/zerus/worker-b"]);
    button("Stop and archive").click(); await flush();
    expect(calls.end).toHaveBeenCalledExactlyOnceWith(row, "terminate", undefined);
    expect(calls.endAfter).toHaveBeenCalledExactlyOnceWith("terminate", row, done, []);
  });

  it("ends the listed children only after the parent's own outcome, without its folder choice", async () => {
    const workers = [child("worker-a"), child("worker-b")];
    calls.children = { ending: workers, staying: [child("saved", { state: "stopped" })] };
    let answer!: (out: unknown) => void;
    calls.end.mockReturnValue(new Promise((resolve) => { answer = resolve; }));
    const row = selectionSnapshot(session("lead"));
    mount(TerminateDialog, row); await flush();
    // The child that Terminate cannot end now is named, never offered.
    expect(listed()).toEqual(["kimi/zerus/worker-a", "kimi/zerus/worker-b", "kimi/zerus/saved"]);
    expect(document.body.textContent).toContain("1 child session cannot be terminated now and stays as it is:");
    box()!.click(); expect(box()!.checked).toBe(true);
    button("Stop and archive").click(); await flush();
    expect(calls.end).toHaveBeenCalledExactlyOnceWith(row, "terminate", undefined);
    expect(calls.endAfter).not.toHaveBeenCalled();
    const failed = { outcome: "failed", detail: "refused" };
    answer(failed); await flush();
    expect(calls.endAfter).toHaveBeenCalledExactlyOnceWith("terminate", row, failed, workers);
  });

  it("shows nothing more for a session without live children", async () => {
    mount(TerminateDialog, selectionSnapshot(session("alone"))); await flush();
    expect(document.querySelector(".dlg-child-sessions")).toBeNull();
    button("Stop and archive").click(); await flush();
    expect(calls.endAfter).toHaveBeenCalledExactlyOnceWith("terminate", expect.anything(), done, []);
  });

  it("starts with the opt-in off again in a reopened confirmation: the choice is never remembered", async () => {
    calls.children = { ending: [child("worker-a")], staying: [] };
    const row = selectionSnapshot(session("lead"));
    mount(TerminateDialog, row); await flush();
    box()!.click(); expect(box()!.checked).toBe(true);
    dispose?.(); dispose = undefined; document.body.replaceChildren();
    mount(TerminateDialog, row); await flush();
    expect(document.querySelectorAll(".dlg-child-sessions")).toHaveLength(1);
    expect(box()?.checked).toBe(false);
  });

  it("keeps the chosen children when releasing an unused cleanup offer fails: the parent never ran", async () => {
    const workers = [child("worker-a"), child("worker-b")];
    calls.children = { ending: workers, staying: [] };
    calls.offer.mockResolvedValue({ id: "offer-1", checked: false });
    calls.cancel.mockRejectedValue("release refused");
    const row = selectionSnapshot(session("lead", { git_worktree: true }));
    mount(TerminateDialog, row); await flush();
    expect(calls.offer).toHaveBeenCalledOnce();
    box()!.click();
    button("Stop and archive").click(); await flush();
    expect(calls.cancel).toHaveBeenCalledExactlyOnceWith("offer-1");
    expect(calls.notify).toHaveBeenCalledWith("error", "release refused", true);
    expect(calls.end).not.toHaveBeenCalled();
    // No outcome: endAfter keeps every child (store/batch.test.ts).
    expect(calls.endAfter).toHaveBeenCalledExactlyOnceWith("terminate", row, undefined, workers);
  });

  it("says the count covers every level, marks and counts children without an HGS archive, and notes stopped children", async () => {
    calls.children = {
      ending: [child("worker-a"), child("plain", { tracked: false })],
      staying: [child("saved", { state: "stopped" }), child("held", { state: "paused" })],
    };
    mount(TerminateDialog, selectionSnapshot(session("lead"))); await flush();
    expect(listed()).toEqual(["kimi/zerus/worker-a", "kimi/zerus/plain (no HGS archive)", "kimi/zerus/saved", "kimi/zerus/held"]);
    const text = document.body.textContent ?? "";
    expect(text).toContain("The count and the list include child sessions of child sessions, up to 8 levels deep.");
    expect(document.querySelector(".dlg-child-killed")?.textContent).toBe("1 of them has no HGS archive and is killed: it leaves the list, and any native conversation history is kept.");
    expect(document.querySelector(".dlg-child-stopped")?.textContent).toBe("Terminate leaves stopped or paused child sessions as they are. On the command line, hgs terminate --with-children moves them to Archive.");
  });

  it("names at most 50 children one by one and counts the rest", async () => {
    calls.children = { ending: Array.from({ length: 53 }, (_, i) => child(`w${i}`)), staying: [] };
    mount(TerminateDialog, selectionSnapshot(session("lead"))); await flush();
    expect(listed()).toHaveLength(51);
    expect(listed().at(-1)).toBe("+3 more");
    expect(box()?.parentElement?.textContent?.trim()).toBe("Also end 53 child sessions");
  });
});

describe("Move to archive of a parent with live child sessions", () => {
  it("asks first, off by default, and archives the chosen children after the parent", async () => {
    const workers = [child("worker-a", { state: "stopped" })];
    calls.children = { ending: workers, staying: [child("busy")] };
    const row = selectionSnapshot(session("lead", { state: "stopped" }));
    mount(ArchiveDialog, row); await flush();
    expect(box()?.checked).toBe(false);
    expect(box()?.parentElement?.textContent?.trim()).toBe("Also archive 1 child session");
    expect(listed()).toEqual(["kimi/zerus/worker-a", "kimi/zerus/busy"]);
    expect(document.body.textContent).toContain("1 child session cannot be archived now and stays as it is:");
    box()!.click();
    button("Move to archive").click(); await flush();
    expect(calls.perform).toHaveBeenCalledExactlyOnceWith(row, { kind: "archive" });
    expect(calls.refresh).toHaveBeenCalledOnce();
    expect(calls.endAfter).toHaveBeenCalledExactlyOnceWith("archive", row, done, workers);
  });

  it("confirming without the opt-in archives only the parent", async () => {
    calls.children = { ending: [child("worker-a", { state: "stopped" })], staying: [] };
    const row = selectionSnapshot(session("lead", { state: "stopped" }));
    mount(ArchiveDialog, row); await flush();
    expect(box()?.checked).toBe(false);
    button("Move to archive").click(); await flush();
    expect(calls.perform).toHaveBeenCalledExactlyOnceWith(row, { kind: "archive" });
    expect(calls.endAfter).toHaveBeenCalledExactlyOnceWith("archive", row, done, []);
  });

  it("ticking and unticking the opt-in before confirming archives only the parent", async () => {
    calls.children = { ending: [child("worker-a", { state: "stopped" })], staying: [] };
    const row = selectionSnapshot(session("lead", { state: "stopped" }));
    mount(ArchiveDialog, row); await flush();
    box()!.click(); box()!.click(); expect(box()!.checked).toBe(false);
    button("Move to archive").click(); await flush();
    expect(calls.endAfter).toHaveBeenCalledExactlyOnceWith("archive", row, done, []);
  });

  it("says the count covers every level, without Terminate's archive and stopped-child lines", async () => {
    calls.children = { ending: [child("plain", { state: "stopped", tracked: false })], staying: [child("saved", { state: "paused" })] };
    mount(ArchiveDialog, selectionSnapshot(session("lead", { state: "stopped" }))); await flush();
    expect(listed()).toEqual(["kimi/zerus/plain", "kimi/zerus/saved"]);
    expect(document.body.textContent).toContain("The count and the list include child sessions of child sessions, up to 8 levels deep.");
    expect(document.querySelector(".dlg-child-killed")).toBeNull();
    expect(document.querySelector(".dlg-child-stopped")).toBeNull();
  });

  it("Cancel archives nothing", async () => {
    calls.children = { ending: [child("worker-a", { state: "stopped" })], staying: [] };
    mount(ArchiveDialog, selectionSnapshot(session("lead", { state: "stopped" }))); await flush();
    box()!.click();
    button("Cancel").click(); await flush();
    expect(calls.perform).not.toHaveBeenCalled();
    expect(calls.endAfter).not.toHaveBeenCalled();
  });

  it("the menu entry asks only for a parent with live children; any other archives at once, as before", async () => {
    const host = document.createElement("div"); document.body.append(host);
    dispose = render(() => createComponent(ActionDialogs, {}), host);
    const lead = session("lead", { state: "stopped" });
    actionEntries(lead).archive!.action(); await flush();
    expect(calls.perform).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({ name: lead.name }), { kind: "archive" });
    expect(document.querySelector(".dlg-child-sessions")).toBeNull();
    calls.perform.mockClear();
    calls.children = { ending: [], staying: [child("busy")] };
    actionEntries(lead).archive!.action(); await flush();
    expect(calls.perform).not.toHaveBeenCalled();
    expect(document.querySelector("#action-dlg-title")?.textContent).toBe("Move to archive?");
    expect(listed()).toEqual(["kimi/zerus/busy"]);
    // Nothing the action applies to: no opt-in, only the children that stay.
    expect(box()).toBeNull();
  });
});
